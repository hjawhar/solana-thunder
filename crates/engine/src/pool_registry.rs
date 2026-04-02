use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};

use solana_pubkey::Pubkey;
use thunder_aggregator::cache::CachedPool;
use thunder_aggregator::pool_index::PoolIndex;
use thunder_core::Market;

use crate::account_store::AccountStore;


/// Pool with metadata, swappable flag, and DEX-specific auxiliary accounts.
pub struct PoolInfo {
    pub address: String,
    pub dex_name: String,
    pub market: Box<dyn Market>,
    pub swappable: bool,
    pub quote_mint: Pubkey,
    pub base_mint: Pubkey,
    pub quote_vault: Pubkey,
    pub base_vault: Pubkey,
    /// CLMM tick array accounts, populated by cold_start.
    pub tick_arrays: Vec<Pubkey>,
    /// DLMM bitmap extension account, populated by cold_start.
    pub bitmap_ext: Option<Pubkey>,
    /// DLMM active bin array PDA, populated by cold_start.
    pub bin_array: Option<Pubkey>,
    /// Serialized pool data for disk cache.
    pub cached_data: Vec<u8>,
}

/// Registry of all pools with graph-based lookup and per-pool swappable validation.
pub struct PoolRegistry {
    pools: HashMap<String, PoolInfo>,
    /// mint -> [(other_mint, pool_address)]
    edges: HashMap<Pubkey, Vec<(Pubkey, String)>>,
    dex_counts: HashMap<String, usize>,
    swappable_count: AtomicUsize,
}

impl PoolRegistry {
    pub fn new() -> Self {
        Self {
            pools: HashMap::new(),
            edges: HashMap::new(),
            dex_counts: HashMap::new(),
            swappable_count: AtomicUsize::new(0),
        }
    }

    /// Build a registry from an existing PoolIndex.
    ///
    /// Reconstructs owned `Market` trait objects by deserializing each entry's
    /// `cached_data` through `CachedPool`. All pools start with `swappable: false`;
    /// call `validate_all` to set flags from an `AccountStore`.
    pub fn from_pool_index(index: &PoolIndex) -> Self {
        let mut registry = Self::new();

        for (_addr, entry) in index.iter_pools() {
            if entry.cached_data.is_empty() {
                continue;
            }

            // Reconstruct an owned Market from the serialized cache bytes.
            let cached: CachedPool = match bincode::deserialize(&entry.cached_data) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let (address, owned_entry) = cached.into_pool_entry();

            let meta = match owned_entry.market.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };

            let info = PoolInfo {
                address: address.clone(),
                dex_name: owned_entry.dex_name,
                market: owned_entry.market,
                swappable: false,
                quote_mint: meta.quote_mint,
                base_mint: meta.base_mint,
                quote_vault: meta.quote_vault,
                base_vault: meta.base_vault,
                tick_arrays: Vec::new(),
                bitmap_ext: None,
                bin_array: None,
                cached_data: owned_entry.cached_data,
            };

            registry.add_pool(address, info);
        }

        registry
    }

    /// Add a pool, building bidirectional edges for graph traversal.
    pub fn add_pool(&mut self, address: String, info: PoolInfo) {
        let quote_mint = info.quote_mint;
        let base_mint = info.base_mint;

        self.edges
            .entry(quote_mint)
            .or_default()
            .push((base_mint, address.clone()));
        self.edges
            .entry(base_mint)
            .or_default()
            .push((quote_mint, address.clone()));

        *self.dex_counts.entry(info.dex_name.clone()).or_insert(0) += 1;

        if info.swappable {
            self.swappable_count.fetch_add(1, Ordering::Relaxed);
        }

        self.pools.insert(address, info);
    }

    pub fn get_pool(&self, address: &str) -> Option<&PoolInfo> {
        self.pools.get(address)
    }

    pub fn get_pool_mut(&mut self, address: &str) -> Option<&mut PoolInfo> {
        self.pools.get_mut(address)
    }

    /// All pool addresses that directly connect mint_a and mint_b.
    pub fn direct_pools(&self, mint_a: &Pubkey, mint_b: &Pubkey) -> Vec<String> {
        self.edges
            .get(mint_a)
            .map(|edges| {
                edges
                    .iter()
                    .filter(|(other, _)| other == mint_b)
                    .map(|(_, addr)| addr.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// All (other_mint, pool_address) pairs reachable from `mint` in one hop.
    pub fn neighbors(&self, mint: &Pubkey) -> Vec<(Pubkey, String)> {
        self.edges.get(mint).cloned().unwrap_or_default()
    }

    pub fn pool_count(&self) -> usize {
        self.pools.len()
    }

    pub fn swappable_count(&self) -> usize {
        self.swappable_count.load(Ordering::Relaxed)
    }

    pub fn unique_mints(&self) -> usize {
        self.edges.len()
    }

    pub fn dex_counts(&self) -> &HashMap<String, usize> {
        &self.dex_counts
    }

    pub fn iter_pools(&self) -> impl Iterator<Item = (&str, &PoolInfo)> {
        self.pools.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Set of all pool addresses where swappable is true.
    pub fn swappable_set(&self) -> HashSet<String> {
        self.pools
            .iter()
            .filter(|(_, info)| info.swappable)
            .map(|(addr, _)| addr.clone())
            .collect()
    }

    /// Validate a single pool's swappable status against on-chain account data.
    pub fn validate_pool(&mut self, address: &str, store: &AccountStore) {
        let info = match self.pools.get_mut(address) {
            Some(i) => i,
            None => return,
        };

        let was_swappable = info.swappable;
        let now_swappable = check_swappable(info, store);

        if was_swappable != now_swappable {
            info.swappable = now_swappable;
            if now_swappable {
                self.swappable_count.fetch_add(1, Ordering::Relaxed);
            } else {
                self.swappable_count.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }

    /// Validate all pools against the account store and recompute swappable_count.
    pub fn validate_all(&mut self, store: &AccountStore) {
        let mut count = 0usize;
        for info in self.pools.values_mut() {
            info.swappable = check_swappable(info, store);
            if info.swappable {
                count += 1;
            }
        }
        self.swappable_count.store(count, Ordering::Relaxed);
    }

    /// Initial validation using cached vault balances from the market objects.
    /// Call immediately after building the registry — no AccountStore needed.
    /// Re-validate with `validate_all(store)` once fresh vault data arrives.
    pub fn validate_from_cache(&mut self) {
        let mut count = 0usize;
        for info in self.pools.values_mut() {
            info.swappable = check_swappable_cached(info);
            if info.swappable {
                count += 1;
            }
        }
        self.swappable_count.store(count, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Per-DEX swappable validation
// ---------------------------------------------------------------------------

/// Determine whether a pool can participate in route discovery.
/// Checks on-chain status and vault liquidity only — NOT auxiliary accounts
/// (tick arrays, bin arrays, bitmap extensions) which are needed for swap
/// instruction building but irrelevant for quoting.
fn check_swappable(info: &PoolInfo, store: &AccountStore) -> bool {
    match info.dex_name.as_str() {
        "Pumpfun AMM" => true,

        "Raydium AMM V4" => vaults_funded(info, store),

        // All remaining DEXs: active + funded vaults.
        _ => info.market.is_active() && vaults_funded(info, store),
    }
}

/// Both vaults must have non-zero token balance in the store.
fn vaults_funded(info: &PoolInfo, store: &AccountStore) -> bool {
    store.read_token_balance(&info.quote_vault) > 0
        && store.read_token_balance(&info.base_vault) > 0
}

/// Same as `check_swappable` but uses cached vault balances from the market
/// object instead of the AccountStore. Used during startup before vault data
/// is fetched from RPC.
fn check_swappable_cached(info: &PoolInfo) -> bool {
    match info.dex_name.as_str() {
        "Pumpfun AMM" => true,

        "Raydium AMM V4" => {
            info.market.financials().is_ok_and(|f| f.quote_balance > 0 && f.base_balance > 0)
        }

        _ => {
            info.market.is_active()
                && info.market.financials().is_ok_and(|f| f.quote_balance > 0 && f.base_balance > 0)
        }
    }
}