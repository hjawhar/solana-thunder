#![allow(deprecated)] // get_program_accounts_with_config is deprecated but its replacement returns UI-encoded data

//! Async pool loading from Solana RPC for all supported DEXs.
//!
//! Uses `getProgramAccounts` with dataSize filters to discover pools,
//! deserializes them, batch-fetches vault balances, and constructs
//! `PoolEntry` values for the `PoolIndex`.

use std::sync::Arc;

use borsh::BorshDeserialize;
use solana_account_decoder_client_types::UiAccountEncoding;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_rpc_client_api::filter::RpcFilterType;

use meteora_damm::{
    derive_token_vault_address, MeteoraDAMMMarket, MeteoraDAMMPool, MeteoraDAMMV2Market,
    MeteoraDAMMV2Pool, METEORA_DYNAMIC_AMM, METEORA_DYNAMIC_AMM_V2,
};
use meteora_dlmm::{MeteoraDlmmMarket, MeteoraDLMMPool, METEORA_DYNAMIC_LMM};
use pumpfun_amm::{PumpfunAmmMarket, PumpfunAmmPool, PUMPFUN_AMM_PROGRAM};
use raydium_amm_v4::{RaydiumAmmV4Market, RaydiumAMMV4, RAYDIUM_LIQUIDITY_POOL_V4};
use raydium_clmm::{RaydiumClmmMarket, RaydiumCLMMPool, RAYDIUM_CLMM};
use thunder_core::GenericError;

use crate::pool_index::PoolIndex;
use crate::types::{LoadPhase, LoadProgress, PoolEntry};

/// Callback invoked with progress updates during loading.
pub type ProgressCallback = Box<dyn Fn(LoadProgress) + Send + Sync>;

/// Maximum number of accounts to fetch in a single `getMultipleAccounts` call.
const BALANCE_BATCH_SIZE: usize = 100;

/// Async loader that discovers pools from on-chain program accounts.
pub struct PoolLoader {
    rpc: Arc<RpcClient>,
}

impl PoolLoader {
    pub fn new(rpc_url: &str) -> Self {
        let rpc = Arc::new(RpcClient::new_with_commitment(
            rpc_url.to_string(),
            CommitmentConfig::confirmed(),
        ));
        Self { rpc }
    }

    /// Load all pools from all DEXs concurrently. Calls `progress_cb` with updates.
    pub async fn load_all(
        &self,
        progress_cb: &ProgressCallback,
    ) -> Result<PoolIndex, GenericError> {
        let mut index = PoolIndex::new();

        let (r1, r2, r3, r4, r5, r6) = tokio::join!(
            self.load_raydium_v4(progress_cb),
            self.load_raydium_clmm(progress_cb),
            self.load_meteora_damm_v1(progress_cb),
            self.load_meteora_damm_v2(progress_cb),
            self.load_meteora_dlmm(progress_cb),
            self.load_pumpfun(progress_cb),
        );

        for result in [r1, r2, r3, r4, r5, r6] {
            match result {
                Ok(pools) => {
                    for (addr, entry) in pools {
                        let _ = index.add_pool(addr, entry);
                    }
                }
                Err(e) => eprintln!("Warning: loader error: {e}"),
            }
        }

        Ok(index)
    }

    // =========================================================================
    // Raydium AMM V4
    // =========================================================================

    async fn load_raydium_v4(
        &self,
        cb: &ProgressCallback,
    ) -> Result<Vec<(String, PoolEntry)>, GenericError> {
        let dex = "Raydium AMM V4";
        cb(progress(dex, LoadPhase::FetchingPools));

        let program = Pubkey::from_str_const(RAYDIUM_LIQUIDITY_POOL_V4);
        let accounts =
            self.fetch_program_accounts(&program, Some(752)).await?;

        let total = accounts.len();
        cb(progress(dex, LoadPhase::Deserializing { done: 0, total }));

        // Deserialize pools (no discriminator skip for Raydium V4).
        let mut pools: Vec<(String, RaydiumAMMV4)> = Vec::with_capacity(total);
        let mut errors = 0usize;
        for (i, (pubkey, account)) in accounts.iter().enumerate() {
            match RaydiumAMMV4::try_from_slice(&account.data) {
                Ok(pool) => pools.push((pubkey.to_string(), pool)),
                Err(_) => errors += 1,
            }
            if (i + 1) % 500 == 0 || i + 1 == total {
                cb(progress(dex, LoadPhase::Deserializing { done: i + 1, total }));
            }
        }
        if errors > 0 {
            eprintln!("{dex}: {errors} deserialization failures skipped");
        }

        // Collect vault pubkeys: (base_vault, quote_vault) per pool.
        let vault_keys: Vec<Pubkey> = pools
            .iter()
            .flat_map(|(_, p)| [p.base_vault, p.quote_vault])
            .collect();

        cb(progress(dex, LoadPhase::FetchingBalances { done: 0, total: pools.len() }));
        let balances = self.batch_fetch_balances(&vault_keys, dex, cb).await?;

        // Build market entries. Each pool uses 2 consecutive balance slots.
        let mut entries = Vec::with_capacity(pools.len());
        for (i, (addr, pool)) in pools.into_iter().enumerate() {
            let base_bal = balances[i * 2];
            let quote_bal = balances[i * 2 + 1];
            let market = RaydiumAmmV4Market::new(pool, addr.clone(), quote_bal, base_bal);
            entries.push((addr, PoolEntry {
                market: Box::new(market),
                dex_name: dex.to_string(),
            }));
            if (i + 1) % 500 == 0 || i + 1 == entries.len() {
                cb(progress(dex, LoadPhase::BuildingMarkets { done: i + 1, total: entries.len() }));
            }
        }

        cb(progress(dex, LoadPhase::Complete { pool_count: entries.len() }));
        Ok(entries)
    }

    // =========================================================================
    // Raydium CLMM
    // =========================================================================

    async fn load_raydium_clmm(
        &self,
        cb: &ProgressCallback,
    ) -> Result<Vec<(String, PoolEntry)>, GenericError> {
        let dex = "Raydium CLMM";
        cb(progress(dex, LoadPhase::FetchingPools));

        let program = Pubkey::from_str_const(RAYDIUM_CLMM);
        let accounts =
            self.fetch_program_accounts(&program, Some(1544)).await?;

        let total = accounts.len();
        cb(progress(dex, LoadPhase::Deserializing { done: 0, total }));

        let mut pools: Vec<(String, RaydiumCLMMPool)> = Vec::with_capacity(total);
        let mut errors = 0usize;
        for (i, (pubkey, account)) in accounts.iter().enumerate() {
            match deserialize_with_discriminator::<RaydiumCLMMPool>(&account.data) {
                Ok(pool) => pools.push((pubkey.to_string(), pool)),
                Err(_) => errors += 1,
            }
            if (i + 1) % 500 == 0 || i + 1 == total {
                cb(progress(dex, LoadPhase::Deserializing { done: i + 1, total }));
            }
        }
        if errors > 0 {
            eprintln!("{dex}: {errors} deserialization failures skipped");
        }

        // Vaults: token_vault_0, token_vault_1
        let vault_keys: Vec<Pubkey> = pools
            .iter()
            .flat_map(|(_, p)| [p.token_vault_0, p.token_vault_1])
            .collect();

        cb(progress(dex, LoadPhase::FetchingBalances { done: 0, total: pools.len() }));
        let balances = self.batch_fetch_balances(&vault_keys, dex, cb).await?;

        let mut entries = Vec::with_capacity(pools.len());
        for (i, (addr, pool)) in pools.into_iter().enumerate() {
            let mut market = RaydiumClmmMarket::new(pool, addr.clone());
            market.vault_0_balance = balances[i * 2];
            market.vault_1_balance = balances[i * 2 + 1];
            entries.push((addr, PoolEntry {
                market: Box::new(market),
                dex_name: dex.to_string(),
            }));
        }

        cb(progress(dex, LoadPhase::Complete { pool_count: entries.len() }));
        Ok(entries)
    }

    // =========================================================================
    // Meteora DAMM V1
    // =========================================================================

    async fn load_meteora_damm_v1(
        &self,
        cb: &ProgressCallback,
    ) -> Result<Vec<(String, PoolEntry)>, GenericError> {
        let dex = "Meteora DAMM V1";
        cb(progress(dex, LoadPhase::FetchingPools));

        let program = Pubkey::from_str_const(METEORA_DYNAMIC_AMM);

        // V1 has two possible data sizes: 944 and 952 bytes.
        let (accounts_944, accounts_952) = tokio::join!(
            self.fetch_program_accounts(&program, Some(944)),
            self.fetch_program_accounts(&program, Some(952)),
        );

        let mut raw_accounts = accounts_944?;
        raw_accounts.extend(accounts_952?);

        let total = raw_accounts.len();
        cb(progress(dex, LoadPhase::Deserializing { done: 0, total }));

        let mut pools: Vec<(String, MeteoraDAMMPool)> = Vec::with_capacity(total);
        let mut errors = 0usize;
        for (i, (pubkey, account)) in raw_accounts.iter().enumerate() {
            match deserialize_with_discriminator::<MeteoraDAMMPool>(&account.data) {
                Ok(pool) => pools.push((pubkey.to_string(), pool)),
                Err(_) => errors += 1,
            }
            if (i + 1) % 500 == 0 || i + 1 == total {
                cb(progress(dex, LoadPhase::Deserializing { done: i + 1, total }));
            }
        }
        if errors > 0 {
            eprintln!("{dex}: {errors} deserialization failures skipped");
        }

        // DAMM V1 vaults: derive token vault PDAs from the vault addresses.
        let vault_keys: Vec<Pubkey> = pools
            .iter()
            .flat_map(|(_, p)| {
                [
                    derive_token_vault_address(p.a_vault).0,
                    derive_token_vault_address(p.b_vault).0,
                ]
            })
            .collect();

        cb(progress(dex, LoadPhase::FetchingBalances { done: 0, total: pools.len() }));
        let balances = self.batch_fetch_balances(&vault_keys, dex, cb).await?;

        let mut entries = Vec::with_capacity(pools.len());
        for (i, (addr, pool)) in pools.into_iter().enumerate() {
            let mut market = MeteoraDAMMMarket::new(pool, addr.clone());
            market.a_vault_balance = balances[i * 2];
            market.b_vault_balance = balances[i * 2 + 1];
            entries.push((addr, PoolEntry {
                market: Box::new(market),
                dex_name: dex.to_string(),
            }));
        }

        cb(progress(dex, LoadPhase::Complete { pool_count: entries.len() }));
        Ok(entries)
    }

    // =========================================================================
    // Meteora DAMM V2
    // =========================================================================

    async fn load_meteora_damm_v2(
        &self,
        cb: &ProgressCallback,
    ) -> Result<Vec<(String, PoolEntry)>, GenericError> {
        let dex = "Meteora DAMM V2";
        cb(progress(dex, LoadPhase::FetchingPools));

        let program = Pubkey::from_str_const(METEORA_DYNAMIC_AMM_V2);
        let accounts =
            self.fetch_program_accounts(&program, Some(1112)).await?;

        let total = accounts.len();
        cb(progress(dex, LoadPhase::Deserializing { done: 0, total }));

        let mut pools: Vec<(String, MeteoraDAMMV2Pool)> = Vec::with_capacity(total);
        let mut errors = 0usize;
        for (i, (pubkey, account)) in accounts.iter().enumerate() {
            match deserialize_with_discriminator::<MeteoraDAMMV2Pool>(&account.data) {
                Ok(pool) => pools.push((pubkey.to_string(), pool)),
                Err(_) => errors += 1,
            }
            if (i + 1) % 500 == 0 || i + 1 == total {
                cb(progress(dex, LoadPhase::Deserializing { done: i + 1, total }));
            }
        }
        if errors > 0 {
            eprintln!("{dex}: {errors} deserialization failures skipped");
        }

        // V2 has direct vault pubkeys on the pool struct.
        let vault_keys: Vec<Pubkey> = pools
            .iter()
            .flat_map(|(_, p)| [p.token_a_vault, p.token_b_vault])
            .collect();

        cb(progress(dex, LoadPhase::FetchingBalances { done: 0, total: pools.len() }));
        let balances = self.batch_fetch_balances(&vault_keys, dex, cb).await?;

        let mut entries = Vec::with_capacity(pools.len());
        for (i, (addr, pool)) in pools.into_iter().enumerate() {
            let mut market = MeteoraDAMMV2Market::new(pool, addr.clone());
            market.a_vault_balance = balances[i * 2];
            market.b_vault_balance = balances[i * 2 + 1];
            entries.push((addr, PoolEntry {
                market: Box::new(market),
                dex_name: dex.to_string(),
            }));
        }

        cb(progress(dex, LoadPhase::Complete { pool_count: entries.len() }));
        Ok(entries)
    }

    // =========================================================================
    // Meteora DLMM
    // =========================================================================

    async fn load_meteora_dlmm(
        &self,
        cb: &ProgressCallback,
    ) -> Result<Vec<(String, PoolEntry)>, GenericError> {
        let dex = "Meteora DLMM";
        cb(progress(dex, LoadPhase::FetchingPools));

        let program = Pubkey::from_str_const(METEORA_DYNAMIC_LMM);
        let accounts =
            self.fetch_program_accounts(&program, Some(904)).await?;

        let total = accounts.len();
        cb(progress(dex, LoadPhase::Deserializing { done: 0, total }));

        let mut pools: Vec<(String, MeteoraDLMMPool)> = Vec::with_capacity(total);
        let mut errors = 0usize;
        for (i, (pubkey, account)) in accounts.iter().enumerate() {
            match deserialize_with_discriminator::<MeteoraDLMMPool>(&account.data) {
                Ok(pool) => pools.push((pubkey.to_string(), pool)),
                Err(_) => errors += 1,
            }
            if (i + 1) % 500 == 0 || i + 1 == total {
                cb(progress(dex, LoadPhase::Deserializing { done: i + 1, total }));
            }
        }
        if errors > 0 {
            eprintln!("{dex}: {errors} deserialization failures skipped");
        }

        // DLMM vaults: reserve_x, reserve_y
        let vault_keys: Vec<Pubkey> = pools
            .iter()
            .flat_map(|(_, p)| [p.reserve_x, p.reserve_y])
            .collect();

        cb(progress(dex, LoadPhase::FetchingBalances { done: 0, total: pools.len() }));
        let balances = self.batch_fetch_balances(&vault_keys, dex, cb).await?;

        let mut entries = Vec::with_capacity(pools.len());
        for (i, (addr, pool)) in pools.into_iter().enumerate() {
            let mut market = MeteoraDlmmMarket::new(pool, addr.clone());
            market.reserve_x_balance = balances[i * 2];
            market.reserve_y_balance = balances[i * 2 + 1];
            entries.push((addr, PoolEntry {
                market: Box::new(market),
                dex_name: dex.to_string(),
            }));
        }

        cb(progress(dex, LoadPhase::Complete { pool_count: entries.len() }));
        Ok(entries)
    }

    // =========================================================================
    // Pumpfun AMM
    // =========================================================================

    async fn load_pumpfun(
        &self,
        cb: &ProgressCallback,
    ) -> Result<Vec<(String, PoolEntry)>, GenericError> {
        let dex = "Pumpfun AMM";
        cb(progress(dex, LoadPhase::FetchingPools));

        let program = Pubkey::from_str_const(PUMPFUN_AMM_PROGRAM);

        // Pumpfun uses Anchor; filter by the 8-byte discriminator for PumpfunAmmPool.
        // Anchor discriminator = first 8 bytes of SHA-256("account:PoolV2").
        // We use dataSize filter based on actual account size instead.
        // Determine the expected account size: 8 (disc) + borsh size of PumpfunAmmPool.
        // From the struct: u8 + u16 + 6*Pubkey(32) + u64 + Pubkey(32) + bool + bool = 1+2+192+8+32+1+1 = 237, + 8 disc = 245
        // But borsh may pad differently. Use no dataSize filter and rely on deserialization.
        let accounts = self
            .fetch_program_accounts(&program, None)
            .await?;

        let total = accounts.len();
        cb(progress(dex, LoadPhase::Deserializing { done: 0, total }));

        let mut pools: Vec<(String, PumpfunAmmPool)> = Vec::with_capacity(total);
        let mut errors = 0usize;
        for (i, (pubkey, account)) in accounts.iter().enumerate() {
            match deserialize_with_discriminator::<PumpfunAmmPool>(&account.data) {
                Ok(pool) => pools.push((pubkey.to_string(), pool)),
                Err(_) => errors += 1,
            }
            if (i + 1) % 500 == 0 || i + 1 == total {
                cb(progress(dex, LoadPhase::Deserializing { done: i + 1, total }));
            }
        }
        if errors > 0 {
            eprintln!("{dex}: {errors} deserialization failures skipped");
        }

        // Pumpfun uses bonding curve virtual reserves — no separate vault balance fetch needed.
        // The pool's pool_base_token_account and pool_quote_token_account are embedded,
        // and the market uses bonding curve data (fetched separately if needed).
        let pool_count = pools.len();
        let mut entries = Vec::with_capacity(pool_count);
        for (i, (addr, pool)) in pools.into_iter().enumerate() {
            let market = PumpfunAmmMarket::new(pool, addr.clone());
            entries.push((addr, PoolEntry {
                market: Box::new(market),
                dex_name: dex.to_string(),
            }));
            if (i + 1) % 500 == 0 || i + 1 == pool_count {
                cb(progress(dex, LoadPhase::BuildingMarkets { done: i + 1, total: pool_count }));
            }
        }

        cb(progress(dex, LoadPhase::Complete { pool_count: entries.len() }));
        Ok(entries)
    }

    // =========================================================================
    // Shared helpers
    // =========================================================================

    /// Fetch all program accounts with an optional dataSize filter.
    async fn fetch_program_accounts(
        &self,
        program_id: &Pubkey,
        data_size: Option<u64>,
    ) -> Result<Vec<(Pubkey, solana_sdk::account::Account)>, GenericError> {
        let filters = data_size
            .map(|size| vec![RpcFilterType::DataSize(size)])
            .unwrap_or_default();

        let config = RpcProgramAccountsConfig {
            filters: Some(filters),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(CommitmentConfig::confirmed()),
                ..Default::default()
            },
            ..Default::default()
        };

        let accounts = self
            .rpc
            .get_program_accounts_with_config(program_id, config)
            .await?;

        Ok(accounts)
    }

    /// Batch-fetch SPL token balances for a list of vault pubkeys.
    ///
    /// Returns a Vec of u64 balances in the same order as `keys`.
    /// Missing or unreadable accounts yield a balance of 0.
    async fn batch_fetch_balances(
        &self,
        keys: &[Pubkey],
        dex: &str,
        cb: &ProgressCallback,
    ) -> Result<Vec<u64>, GenericError> {
        let mut balances = vec![0u64; keys.len()];
        let total_pools = keys.len() / 2; // 2 vaults per pool

        for (chunk_idx, chunk) in keys.chunks(BALANCE_BATCH_SIZE).enumerate() {
            let accounts = self.rpc.get_multiple_accounts(chunk).await?;

            let base_offset = chunk_idx * BALANCE_BATCH_SIZE;
            for (j, maybe_account) in accounts.into_iter().enumerate() {
                if let Some(account) = maybe_account {
                    balances[base_offset + j] = read_token_balance(&account.data);
                }
            }

            // Report progress in terms of pools processed.
            let keys_done = (base_offset + chunk.len()).min(keys.len());
            let pools_done = (keys_done / 2).min(total_pools);
            cb(progress(dex, LoadPhase::FetchingBalances {
                done: pools_done,
                total: total_pools,
            }));
        }

        Ok(balances)
    }
}

// =============================================================================
// Free helpers
// =============================================================================

/// Read the SPL token balance (amount field) from raw account data.
///
/// SPL Token Account layout: bytes 64..72 contain the `amount` as a little-endian u64.
fn read_token_balance(data: &[u8]) -> u64 {
    if data.len() < 72 {
        return 0;
    }
    u64::from_le_bytes(data[64..72].try_into().unwrap())
}

/// Deserialize an Anchor-style account, skipping the 8-byte discriminator.
fn deserialize_with_discriminator<T: BorshDeserialize>(data: &[u8]) -> Result<T, GenericError> {
    if data.len() < 8 {
        return Err("Account data too short for discriminator".into());
    }
    T::try_from_slice(&data[8..]).map_err(|e| e.into())
}

/// Convenience constructor for `LoadProgress`.
fn progress(dex: &str, phase: LoadPhase) -> LoadProgress {
    LoadProgress {
        dex_name: dex.to_string(),
        phase,
    }
}
