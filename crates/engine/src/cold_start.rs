#![allow(deprecated)]

use std::collections::HashMap;
use std::str::FromStr;

use futures::future::join_all;
use solana_account_decoder_client_types::UiAccountEncoding;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_rpc_client_api::filter::RpcFilterType;

use crate::account_store::AccountStore;
use crate::pool_registry::PoolRegistry;

const BATCH_SIZE: usize = 100;
const BATCH_CONCURRENCY: usize = 20;

const CLMM_PROGRAM_ID: &str = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";
const DLMM_PROGRAM_ID: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";

/// Fetch all vault accounts from the registry and store them in the AccountStore.
pub async fn fetch_all_vaults(
    rpc: &RpcClient,
    registry: &PoolRegistry,
    store: &AccountStore,
) {
    let vault_keys: Vec<Pubkey> = registry
        .iter_pools()
        .flat_map(|(_, info)| [info.quote_vault, info.base_vault])
        .collect();

    let total = vault_keys.len();
    println!("[cold_start] fetching {} vault accounts", total);

    let chunks: Vec<(usize, &[Pubkey])> = vault_keys.chunks(BATCH_SIZE).enumerate().collect();
    let mut fetched = 0usize;

    for window in chunks.chunks(BATCH_CONCURRENCY) {
        let futures: Vec<_> = window
            .iter()
            .map(|(_, chunk)| rpc.get_multiple_accounts(chunk))
            .collect();
        let results = join_all(futures).await;

        for ((_, chunk), result) in window.iter().zip(results) {
            match result {
                Ok(accounts) => {
                    for (pubkey, maybe_account) in chunk.iter().zip(accounts) {
                        if let Some(account) = maybe_account {
                            store.upsert(
                                *pubkey,
                                account.data,
                                account.owner,
                                account.lamports,
                                0,
                            );
                            fetched += 1;
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[cold_start] vault batch error: {e}");
                }
            }
        }

        println!(
            "[cold_start] vaults: {}/{} fetched",
            fetched, total
        );
    }

    println!("[cold_start] vaults done: {fetched}/{total} stored");
}

/// Fetch all CLMM tick arrays in one getProgramAccounts call, then assign to top pools.
///
/// Strategy: single GPA for all tick arrays owned by the CLMM program (discriminated
/// by the 8-byte account discriminator implicitly via dataSize or memcmp). We request
/// full account data so we can store them directly. Then we group by pool_id (offset 8)
/// and keep only tick arrays belonging to the top 10,000 CLMM pools by vault balance.
pub async fn fetch_tick_arrays(
    rpc: &RpcClient,
    registry: &mut PoolRegistry,
    store: &AccountStore,
) {
    let clmm_program = match Pubkey::from_str(CLMM_PROGRAM_ID) {
        Ok(pk) => pk,
        Err(_) => return,
    };

    // Collect CLMM pools and sort by vault balance descending.
    let mut clmm_pools: Vec<(&str, u64)> = registry
        .iter_pools()
        .filter(|(_, info)| info.dex_name == "Raydium CLMM")
        .map(|(addr, info)| {
            let balance = store.read_token_balance(&info.quote_vault)
                + store.read_token_balance(&info.base_vault);
            (addr, balance)
        })
        .collect();

    clmm_pools.sort_unstable_by(|a, b| b.1.cmp(&a.1));
    clmm_pools.truncate(10_000);

    if clmm_pools.is_empty() {
        println!("[cold_start] no CLMM pools found, skipping tick arrays");
        return;
    }

    // Build a set of top pool pubkeys for fast lookup.
    let top_pool_set: HashMap<Pubkey, String> = clmm_pools
        .iter()
        .filter_map(|(addr, _)| {
            Pubkey::from_str(addr).ok().map(|pk| (pk, addr.to_string()))
        })
        .collect();

    println!(
        "[cold_start] fetching tick arrays for {} CLMM pools (single GPA)",
        top_pool_set.len()
    );

    // Single GPA: fetch all tick array accounts. Tick arrays are 10240 bytes.
    let config = RpcProgramAccountsConfig {
        filters: Some(vec![RpcFilterType::DataSize(10240)]),
        account_config: RpcAccountInfoConfig {
            encoding: Some(UiAccountEncoding::Base64),
            commitment: Some(CommitmentConfig::confirmed()),
            ..Default::default()
        },
        ..Default::default()
    };

    let all_tick_arrays = match rpc
        .get_program_accounts_with_config(&clmm_program, config)
        .await
    {
        Ok(accounts) => accounts,
        Err(e) => {
            eprintln!("[cold_start] tick array GPA error: {e}");
            return;
        }
    };

    println!(
        "[cold_start] GPA returned {} tick array accounts, filtering to top pools",
        all_tick_arrays.len()
    );

    // Group tick arrays by pool_id and keep only those for top pools.
    // pool_id is at offset 8 in tick array account data (32 bytes).
    let mut pool_tick_arrays: HashMap<Pubkey, Vec<Pubkey>> = HashMap::new();
    let mut stored = 0usize;

    for (pubkey, account) in all_tick_arrays {
        if account.data.len() < 40 {
            continue;
        }

        let pool_id = Pubkey::try_from(&account.data[8..40]).unwrap();

        if !top_pool_set.contains_key(&pool_id) {
            continue;
        }

        store.upsert(pubkey, account.data, account.owner, account.lamports, 0);
        pool_tick_arrays
            .entry(pool_id)
            .or_default()
            .push(pubkey);
        stored += 1;
    }

    // Assign tick arrays to pool infos.
    for (pool_id, tick_keys) in &pool_tick_arrays {
        if let Some(addr) = top_pool_set.get(pool_id) {
            if let Some(pool_info) = registry.get_pool_mut(addr) {
                pool_info.tick_arrays = tick_keys.clone();
            }
        }
    }

    println!(
        "[cold_start] tick arrays: {stored} stored across {} pools",
        pool_tick_arrays.len()
    );
}

/// Fetch all DLMM bitmap extension accounts (dataSize=12488) and assign to pools.
pub async fn fetch_bitmap_extensions(
    rpc: &RpcClient,
    registry: &mut PoolRegistry,
    store: &AccountStore,
) {
    let dlmm_program = match Pubkey::from_str(DLMM_PROGRAM_ID) {
        Ok(pk) => pk,
        Err(_) => return,
    };

    let config = RpcProgramAccountsConfig {
        filters: Some(vec![RpcFilterType::DataSize(12488)]),
        account_config: RpcAccountInfoConfig {
            encoding: Some(UiAccountEncoding::Base64),
            commitment: Some(CommitmentConfig::confirmed()),
            ..Default::default()
        },
        ..Default::default()
    };

    let accounts = match rpc
        .get_program_accounts_with_config(&dlmm_program, config)
        .await
    {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[cold_start] bitmap extension GPA error: {e}");
            return;
        }
    };

    println!(
        "[cold_start] fetched {} bitmap extension accounts",
        accounts.len()
    );

    // Build a reverse lookup: pool_pubkey -> pool address string.
    let pool_lookup: HashMap<Pubkey, String> = registry
        .iter_pools()
        .filter(|(_, info)| info.dex_name == "Meteora DLMM")
        .filter_map(|(addr, _)| Pubkey::from_str(addr).ok().map(|pk| (pk, addr.to_string())))
        .collect();

    let mut matched = 0usize;

    for (pubkey, account) in accounts {
        if account.data.len() < 40 {
            continue;
        }

        // lb_pair (pool address) at offset 8, 32 bytes.
        let lb_pair = Pubkey::try_from(&account.data[8..40]).unwrap();

        store.upsert(pubkey, account.data, account.owner, account.lamports, 0);

        if let Some(pool_addr) = pool_lookup.get(&lb_pair) {
            if let Some(pool_info) = registry.get_pool_mut(pool_addr) {
                pool_info.bitmap_ext = Some(pubkey);
                matched += 1;
            }
        }
    }

    println!("[cold_start] bitmap extensions: {matched} matched to pools");
}

/// Cold-start orchestrator: fetch all on-chain data and validate pools.
pub async fn cold_start(
    rpc: &RpcClient,
    registry: &mut PoolRegistry,
    store: &AccountStore,
) {
    println!("[cold_start] starting cold start sequence");

    // 1. Vault balances first — everything else depends on these.
    fetch_all_vaults(rpc, registry, store).await;

    // 2. Bitmap extensions (fast, ~43 accounts).
    fetch_bitmap_extensions(rpc, registry, store).await;

    // 3. Tick arrays (depends on vault balances for sorting).
    fetch_tick_arrays(rpc, registry, store).await;

    // 4. Validate all pools against the now-populated store.
    registry.validate_all(store);

    // 5. Summary.
    println!("[cold_start] === cold start complete ===");
    println!("[cold_start] total accounts in store: {}", store.len());
    println!(
        "[cold_start] swappable pools: {}/{}",
        registry.swappable_count(),
        registry.pool_count()
    );

    for (dex, count) in registry.dex_counts() {
        // Count swappable per DEX.
        let swappable = registry
            .iter_pools()
            .filter(|(_, info)| info.dex_name == *dex && info.swappable)
            .count();
        println!("[cold_start]   {dex}: {swappable}/{count} swappable");
    }
}
