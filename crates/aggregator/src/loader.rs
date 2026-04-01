#![allow(deprecated)] // get_program_accounts_with_config — successor returns UI-encoded data

//! Async pool loading from Solana RPC for all supported DEXs.
//!
//! Uses `getProgramAccounts` with dataSize + discriminator + mint memcmp
//! filters to discover pools, batch-fetches vault balances, and constructs
//! `PoolEntry` values for the `PoolIndex`.
//!
//! Loading strategy:
//! 1. For each DEX, query pools paired with hub mints (WSOL, USDC, USDT)
//!    using memcmp on the mint field. This keeps response sizes bounded.
//! 2. Deduplicate across queries (same pool can match on mint_a or mint_b).
//! 3. Batch-fetch vault balances via getMultipleAccounts.
//! 4. Build Market structs and insert into the PoolIndex.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use borsh::BorshDeserialize;
use solana_account_decoder_client_types::UiAccountEncoding;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_rpc_client_api::filter::{Memcmp, RpcFilterType};
use solana_sdk::account::Account;

use meteora_damm::{
    derive_token_vault_address, MeteoraDAMMMarket, MeteoraDAMMPool, MeteoraDAMMV2Market,
    MeteoraDAMMV2Pool, METEORA_DYNAMIC_AMM, METEORA_DYNAMIC_AMM_V2,
};
use meteora_dlmm::{MeteoraDlmmMarket, MeteoraDLMMPool, METEORA_DYNAMIC_LMM};
use pumpfun_amm::{PumpfunAmmMarket, PumpfunAmmPool, PUMPFUN_AMM_PROGRAM};
use raydium_amm_v4::{RaydiumAmmV4Market, RaydiumAMMV4, RAYDIUM_LIQUIDITY_POOL_V4};
use raydium_clmm::{RaydiumClmmMarket, RaydiumCLMMPool, RAYDIUM_CLMM};
use thunder_core::{GenericError, USDC, USDT, WSOL};

use crate::pool_index::PoolIndex;
use crate::types::{LoadPhase, LoadProgress, PoolEntry};

/// Callback invoked with progress updates during loading.
pub type ProgressCallback = Box<dyn Fn(LoadProgress) + Send + Sync>;

/// Maximum accounts per `getMultipleAccounts` batch.
const BALANCE_BATCH_SIZE: usize = 100;

/// Number of balance batches to run concurrently.
const BALANCE_CONCURRENCY: usize = 10;

/// Per-DEX cap on loaded pools. Prevents runaway loading for DEXs with
/// hundreds of thousands of pools (e.g., DAMM V2, Pumpfun).
const MAX_POOLS_PER_DEX: usize = 20_000;

/// Hub mints to query pools for. Order matters: WSOL first (most pairs).
const HUB_MINTS: [&str; 3] = [WSOL, USDC, USDT];

// Anchor discriminators (SHA256("account:<Name>")[0..8])
const DISC_POOL_STATE: [u8; 8] = [247, 237, 227, 245, 215, 195, 222, 70]; // Raydium CLMM "PoolState"
const DISC_POOL: [u8; 8] = [241, 154, 109, 4, 17, 177, 109, 188]; // "Pool" — DAMM V1/V2, Pumpfun
const DISC_LB_PAIR: [u8; 8] = [33, 11, 49, 98, 181, 101, 177, 13]; // Meteora DLMM "LbPair"

/// Description of a DEX's on-chain pool layout, enough to discover and
/// deserialize pools via RPC filters.
struct DexDescriptor {
    name: &'static str,
    program_id: &'static str,
    /// Account sizes to query (some DEXs have multiple, e.g. DAMM V1: 944+952).
    data_sizes: &'static [u64],
    /// Anchor discriminator (first 8 bytes of account data). None for Raydium V4.
    discriminator: Option<[u8; 8]>,
    /// Byte offsets of the two mint pubkeys in the raw account data.
    mint_a_offset: u64,
    mint_b_offset: u64,
}

const DESCRIPTORS: [DexDescriptor; 6] = [
    DexDescriptor {
        name: "Raydium AMM V4",
        program_id: RAYDIUM_LIQUIDITY_POOL_V4,
        data_sizes: &[752],
        discriminator: None,
        mint_a_offset: 400,
        mint_b_offset: 432,
    },
    DexDescriptor {
        name: "Raydium CLMM",
        program_id: RAYDIUM_CLMM,
        data_sizes: &[1544],
        discriminator: Some(DISC_POOL_STATE),
        mint_a_offset: 73,
        mint_b_offset: 105,
    },
    DexDescriptor {
        name: "Meteora DAMM V1",
        program_id: METEORA_DYNAMIC_AMM,
        data_sizes: &[944],
        discriminator: Some(DISC_POOL),
        mint_a_offset: 40,
        mint_b_offset: 72,
    },
    DexDescriptor {
        name: "Meteora DAMM V2",
        program_id: METEORA_DYNAMIC_AMM_V2,
        data_sizes: &[1112],
        discriminator: Some(DISC_POOL),
        mint_a_offset: 168,
        mint_b_offset: 200,
    },
    DexDescriptor {
        name: "Meteora DLMM",
        program_id: METEORA_DYNAMIC_LMM,
        data_sizes: &[904],
        discriminator: Some(DISC_LB_PAIR),
        mint_a_offset: 88,
        mint_b_offset: 120,
    },
    DexDescriptor {
        name: "Pumpfun AMM",
        program_id: PUMPFUN_AMM_PROGRAM,
        // Pumpfun pools don't have a fixed size we can rely on from AGENTS.md,
        // but all pools share the "Pool" discriminator. We use disc + mint filter only.
        data_sizes: &[],
        discriminator: Some(DISC_POOL),
        mint_a_offset: 43, // base_mint after 8-byte disc + 1 (bump) + 2 (index) + 32 (creator)
        mint_b_offset: 75, // quote_mint after base_mint
    },
];

/// Async loader that discovers pools from on-chain program accounts.
pub struct PoolLoader {
    rpc: Arc<RpcClient>,
}

impl PoolLoader {
    pub fn new(rpc_url: &str) -> Self {
        let rpc = Arc::new(RpcClient::new_with_timeout_and_commitment(
            rpc_url.to_string(),
            Duration::from_secs(300),
            CommitmentConfig::confirmed(),
        ));
        Self { rpc }
    }

    /// Load all pools from all DEXs concurrently.
    pub async fn load_all(
        &self,
        progress_cb: &ProgressCallback,
    ) -> Result<PoolIndex, GenericError> {
        let mut index = PoolIndex::new();

        // Load all 6 DEXs in parallel. Each returns its pools independently.
        let (r0, r1, r2, r3, r4, r5) = tokio::join!(
            self.load_dex(&DESCRIPTORS[0], progress_cb), // Raydium V4
            self.load_dex(&DESCRIPTORS[1], progress_cb), // Raydium CLMM
            self.load_dex(&DESCRIPTORS[2], progress_cb), // Meteora DAMM V1
            self.load_dex(&DESCRIPTORS[3], progress_cb), // Meteora DAMM V2
            self.load_dex(&DESCRIPTORS[4], progress_cb), // Meteora DLMM
            self.load_dex(&DESCRIPTORS[5], progress_cb), // Pumpfun AMM
        );

        for result in [r0, r1, r2, r3, r4, r5] {
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

    /// Generic per-DEX loader: queries pools paired with each hub mint,
    /// deduplicates, deserializes, fetches vault balances, builds markets.
    async fn load_dex(
        &self,
        desc: &DexDescriptor,
        cb: &ProgressCallback,
    ) -> Result<Vec<(String, PoolEntry)>, GenericError> {
        let dex = desc.name;
        cb(progress(dex, LoadPhase::FetchingPools));

        let program = Pubkey::from_str_const(desc.program_id);
        let mut seen = HashSet::new();
        let mut raw_accounts: Vec<(Pubkey, Account)> = Vec::new();

        // Query for pools paired with each hub mint, on both mint slots.
        'hub: for hub_mint_str in &HUB_MINTS {
            if raw_accounts.len() >= MAX_POOLS_PER_DEX {
                break;
            }
            let hub_mint = Pubkey::from_str_const(hub_mint_str);
            let hub_bytes = hub_mint.to_bytes().to_vec();

            for &mint_offset in &[desc.mint_a_offset, desc.mint_b_offset] {
                if raw_accounts.len() >= MAX_POOLS_PER_DEX {
                    break 'hub;
                }
                for &data_size in desc.data_sizes.iter().chain(
                    // If no data_sizes specified, run one query without dataSize filter
                    if desc.data_sizes.is_empty() { [0u64].iter() } else { [].iter() },
                ) {
                    let mut filters = Vec::new();

                    if data_size > 0 {
                        filters.push(RpcFilterType::DataSize(data_size));
                    }

                    // Anchor discriminator filter
                    if let Some(disc) = &desc.discriminator {
                        filters.push(RpcFilterType::Memcmp(Memcmp::new_raw_bytes(
                            0,
                            disc.to_vec(),
                        )));
                    }

                    // Mint filter
                    filters.push(RpcFilterType::Memcmp(Memcmp::new_raw_bytes(
                        mint_offset as usize,
                        hub_bytes.clone(),
                    )));

                    match self.fetch_filtered(&program, filters).await {
                        Ok(accounts) => {
                            for (pubkey, account) in accounts {
                                if seen.insert(pubkey) {
                                    raw_accounts.push((pubkey, account));
                                }
                            }
                        }
                        Err(e) => {
                            let msg = e.to_string();
                            if msg.contains("excluded from account secondary indexes") {
                                cb(progress(dex, LoadPhase::Error(
                                    "Program excluded from RPC secondary indexes".into(),
                                )));
                                return Ok(vec![]);
                            }
                            eprintln!("{dex}: query error (mint_offset={mint_offset}, hub={hub_mint_str}): {e}");
                        }
                    }
                }
            }

            // Report discovery progress after each hub mint
            cb(progress(dex, LoadPhase::Deserializing {
                done: raw_accounts.len(),
                total: raw_accounts.len(),
            }));
        }

        if raw_accounts.is_empty() {
            cb(progress(dex, LoadPhase::Complete { pool_count: 0 }));
            return Ok(vec![]);
        }

        // Cap pool count to avoid runaway loading for mega-large DEXs.
        if raw_accounts.len() > MAX_POOLS_PER_DEX {
            eprintln!("{dex}: capping at {MAX_POOLS_PER_DEX} pools (discovered {})", raw_accounts.len());
            raw_accounts.truncate(MAX_POOLS_PER_DEX);
        }

        let total = raw_accounts.len();
        cb(progress(dex, LoadPhase::Deserializing { done: 0, total }));

        // Deserialize and build market entries
        let entries = self.build_entries(desc, raw_accounts, cb).await?;

        cb(progress(dex, LoadPhase::Complete { pool_count: entries.len() }));
        Ok(entries)
    }

    /// Deserialize raw accounts and build PoolEntry values with vault balances.
    async fn build_entries(
        &self,
        desc: &DexDescriptor,
        raw_accounts: Vec<(Pubkey, Account)>,
        cb: &ProgressCallback,
    ) -> Result<Vec<(String, PoolEntry)>, GenericError> {
        let dex = desc.name;

        match dex {
            "Raydium AMM V4" => self.build_raydium_v4(raw_accounts, cb).await,
            "Raydium CLMM" => self.build_raydium_clmm(raw_accounts, cb).await,
            "Meteora DAMM V1" => self.build_meteora_damm_v1(raw_accounts, cb).await,
            "Meteora DAMM V2" => self.build_meteora_damm_v2(raw_accounts, cb).await,
            "Meteora DLMM" => self.build_meteora_dlmm(raw_accounts, cb).await,
            "Pumpfun AMM" => self.build_pumpfun(raw_accounts, cb).await,
            _ => Err(format!("Unknown DEX: {dex}").into()),
        }
    }

    // =========================================================================
    // Per-DEX builders
    // =========================================================================

    async fn build_raydium_v4(
        &self,
        accounts: Vec<(Pubkey, Account)>,
        cb: &ProgressCallback,
    ) -> Result<Vec<(String, PoolEntry)>, GenericError> {
        let dex = "Raydium AMM V4";
        let total = accounts.len();
        let mut pools = Vec::with_capacity(total);
        let mut errors = 0usize;

        for (pubkey, account) in &accounts {
            match RaydiumAMMV4::try_from_slice(&account.data) {
                Ok(pool) => pools.push((pubkey.to_string(), pool)),
                Err(_) => errors += 1,
            }
        }
        if errors > 0 {
            eprintln!("{dex}: {errors}/{total} deserialization failures");
        }

        // Vault keys: base_vault, quote_vault
        let vault_keys: Vec<Pubkey> = pools
            .iter()
            .flat_map(|(_, p)| [p.base_vault, p.quote_vault])
            .collect();

        cb(progress(dex, LoadPhase::FetchingBalances { done: 0, total: pools.len() }));
        let balances = self.batch_fetch_balances(&vault_keys, dex, cb).await?;

        let mut entries = Vec::with_capacity(pools.len());
        for (i, (addr, pool)) in pools.into_iter().enumerate() {
            let base_bal = balances[i * 2];
            let quote_bal = balances[i * 2 + 1];
            let market = RaydiumAmmV4Market::new(pool, addr.clone(), quote_bal, base_bal);
            entries.push((addr, PoolEntry {
                market: Box::new(market),
                dex_name: dex.to_string(),
            }));
        }
        Ok(entries)
    }

    async fn build_raydium_clmm(
        &self,
        accounts: Vec<(Pubkey, Account)>,
        cb: &ProgressCallback,
    ) -> Result<Vec<(String, PoolEntry)>, GenericError> {
        let dex = "Raydium CLMM";
        let total = accounts.len();
        let mut pools = Vec::with_capacity(total);
        let mut errors = 0usize;

        for (pubkey, account) in &accounts {
            match deser_anchor::<RaydiumCLMMPool>(&account.data) {
                Ok(pool) => pools.push((pubkey.to_string(), pool)),
                Err(_) => errors += 1,
            }
        }
        if errors > 0 {
            eprintln!("{dex}: {errors}/{total} deserialization failures");
        }

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
        Ok(entries)
    }

    async fn build_meteora_damm_v1(
        &self,
        accounts: Vec<(Pubkey, Account)>,
        cb: &ProgressCallback,
    ) -> Result<Vec<(String, PoolEntry)>, GenericError> {
        let dex = "Meteora DAMM V1";
        let total = accounts.len();
        let mut pools = Vec::with_capacity(total);
        let mut errors = 0usize;

        for (pubkey, account) in &accounts {
            match deser_anchor::<MeteoraDAMMPool>(&account.data) {
                Ok(pool) => pools.push((pubkey.to_string(), pool)),
                Err(_) => errors += 1,
            }
        }
        if errors > 0 {
            eprintln!("{dex}: {errors}/{total} deserialization failures");
        }

        // DAMM V1 vaults: derive token vault PDAs from the vault addresses
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
        Ok(entries)
    }

    async fn build_meteora_damm_v2(
        &self,
        accounts: Vec<(Pubkey, Account)>,
        cb: &ProgressCallback,
    ) -> Result<Vec<(String, PoolEntry)>, GenericError> {
        let dex = "Meteora DAMM V2";
        let total = accounts.len();
        let mut pools = Vec::with_capacity(total);
        let mut errors = 0usize;

        for (pubkey, account) in &accounts {
            match deser_anchor::<MeteoraDAMMV2Pool>(&account.data) {
                Ok(pool) => pools.push((pubkey.to_string(), pool)),
                Err(_) => errors += 1,
            }
        }
        if errors > 0 {
            eprintln!("{dex}: {errors}/{total} deserialization failures");
        }

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
        Ok(entries)
    }

    async fn build_meteora_dlmm(
        &self,
        accounts: Vec<(Pubkey, Account)>,
        cb: &ProgressCallback,
    ) -> Result<Vec<(String, PoolEntry)>, GenericError> {
        let dex = "Meteora DLMM";
        let total = accounts.len();
        let mut pools = Vec::with_capacity(total);
        let mut errors = 0usize;

        for (pubkey, account) in &accounts {
            match deser_anchor::<MeteoraDLMMPool>(&account.data) {
                Ok(pool) => pools.push((pubkey.to_string(), pool)),
                Err(_) => errors += 1,
            }
        }
        if errors > 0 {
            eprintln!("{dex}: {errors}/{total} deserialization failures");
        }

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
        Ok(entries)
    }

    async fn build_pumpfun(
        &self,
        accounts: Vec<(Pubkey, Account)>,
        cb: &ProgressCallback,
    ) -> Result<Vec<(String, PoolEntry)>, GenericError> {
        let dex = "Pumpfun AMM";
        let total = accounts.len();
        let mut pools = Vec::with_capacity(total);
        let mut errors = 0usize;

        for (pubkey, account) in &accounts {
            match deser_anchor::<PumpfunAmmPool>(&account.data) {
                Ok(pool) => pools.push((pubkey.to_string(), pool)),
                Err(_) => errors += 1,
            }
        }
        if errors > 0 {
            eprintln!("{dex}: {errors}/{total} deserialization failures");
        }

        // Pumpfun uses bonding curve virtual reserves — no vault fetch needed.
        let pool_count = pools.len();
        let mut entries = Vec::with_capacity(pool_count);
        for (i, (addr, pool)) in pools.into_iter().enumerate() {
            let market = PumpfunAmmMarket::new(pool, addr.clone());
            entries.push((addr, PoolEntry {
                market: Box::new(market),
                dex_name: dex.to_string(),
            }));
            if (i + 1) % 1000 == 0 || i + 1 == pool_count {
                cb(progress(dex, LoadPhase::BuildingMarkets { done: i + 1, total: pool_count }));
            }
        }
        Ok(entries)
    }

    // =========================================================================
    // RPC helpers
    // =========================================================================

    /// Run `getProgramAccounts` with the given filters.
    async fn fetch_filtered(
        &self,
        program_id: &Pubkey,
        filters: Vec<RpcFilterType>,
    ) -> Result<Vec<(Pubkey, Account)>, GenericError> {
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
    /// Returns a `Vec<u64>` in the same order as `keys`.
    async fn batch_fetch_balances(
        &self,
        keys: &[Pubkey],
        dex: &str,
        cb: &ProgressCallback,
    ) -> Result<Vec<u64>, GenericError> {
        let mut balances = vec![0u64; keys.len()];
        let total_vaults = keys.len();
        let chunks: Vec<(usize, &[Pubkey])> = keys
            .chunks(BALANCE_BATCH_SIZE)
            .enumerate()
            .collect();
        let done_counter = std::sync::atomic::AtomicUsize::new(0);

        // Process BALANCE_CONCURRENCY batches in parallel.
        for window in chunks.chunks(BALANCE_CONCURRENCY) {
            let futures: Vec<_> = window
                .iter()
                .map(|(_, chunk)| self.rpc.get_multiple_accounts(chunk))
                .collect();

            let results = futures::future::join_all(futures).await;

            for ((chunk_idx, _), result) in window.iter().zip(results) {
                let base = chunk_idx * BALANCE_BATCH_SIZE;
                match result {
                    Ok(accounts) => {
                        for (j, maybe_account) in accounts.into_iter().enumerate() {
                            if let Some(account) = maybe_account {
                                balances[base + j] = read_token_balance(&account.data);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("{dex}: balance batch {chunk_idx} failed: {e}");
                    }
                }
            }

            let done = done_counter.fetch_add(window.len(), std::sync::atomic::Ordering::Relaxed) + window.len();
            let pools_done = (done * BALANCE_BATCH_SIZE).min(total_vaults) / 2;
            cb(progress(dex, LoadPhase::FetchingBalances {
                done: pools_done,
                total: total_vaults / 2,
            }));
        }

        Ok(balances)
    }
}

// =============================================================================
// Free helpers
// =============================================================================

/// Read the SPL token balance from raw account data (bytes 64..72).
fn read_token_balance(data: &[u8]) -> u64 {
    if data.len() < 72 {
        return 0;
    }
    u64::from_le_bytes(data[64..72].try_into().unwrap())
}

/// Deserialize an Anchor-style account, skipping the 8-byte discriminator.
/// Tolerates trailing bytes (common on Solana — programs reserve extra space).
fn deser_anchor<T: BorshDeserialize>(data: &[u8]) -> Result<T, GenericError> {
    if data.len() < 8 {
        return Err("Account data too short for discriminator".into());
    }
    let mut slice = &data[8..];
    T::deserialize(&mut slice).map_err(|e| e.into())
}

fn progress(dex: &str, phase: LoadPhase) -> LoadProgress {
    LoadProgress {
        dex_name: dex.to_string(),
        phase,
    }
}
