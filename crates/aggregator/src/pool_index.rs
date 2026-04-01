//! In-memory pool index: token graph for route discovery.
//!
//! Stores pools as `Box<dyn Market>` and maintains an adjacency list
//! mapping each token mint to the pools it participates in.

use std::collections::HashMap;

use solana_pubkey::Pubkey;
use thunder_core::GenericError;

use crate::types::PoolEntry;

/// Edge in the token graph: connects to `other_mint` via `pool_address`.
#[derive(Debug, Clone)]
struct Edge {
    other_mint: Pubkey,
    pool_address: String,
}

/// In-memory index of all loaded pools, organized as a token-pair graph.
pub struct PoolIndex {
    /// Pool address -> PoolEntry (owns the Market trait object).
    pools: HashMap<String, PoolEntry>,
    /// Mint -> list of edges to other mints via pools.
    edges: HashMap<Pubkey, Vec<Edge>>,
    /// Per-DEX pool counts for statistics.
    dex_counts: HashMap<String, usize>,
}

impl PoolIndex {
    pub fn new() -> Self {
        Self {
            pools: HashMap::new(),
            edges: HashMap::new(),
            dex_counts: HashMap::new(),
        }
    }

    /// Insert a pool into the index. Extracts mint pair from pool metadata
    /// and adds bidirectional edges in the token graph.
    pub fn add_pool(&mut self, address: String, entry: PoolEntry) -> Result<(), GenericError> {
        let meta = entry.market.metadata()?;

        // Bidirectional edges: quote_mint <-> base_mint via this pool.
        self.edges
            .entry(meta.quote_mint)
            .or_default()
            .push(Edge {
                other_mint: meta.base_mint,
                pool_address: address.clone(),
            });
        self.edges
            .entry(meta.base_mint)
            .or_default()
            .push(Edge {
                other_mint: meta.quote_mint,
                pool_address: address.clone(),
            });

        *self.dex_counts.entry(entry.dex_name.clone()).or_insert(0) += 1;
        self.pools.insert(address, entry);
        Ok(())
    }

    /// Look up a pool by address.
    pub fn get_pool(&self, address: &str) -> Option<&PoolEntry> {
        self.pools.get(address)
    }

    /// All (other_mint, pool_address) pairs reachable from `mint` in one hop.
    pub fn neighbors(&self, mint: &Pubkey) -> Vec<(Pubkey, String)> {
        self.edges
            .get(mint)
            .map(|edges| {
                edges
                    .iter()
                    .map(|e| (e.other_mint, e.pool_address.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// All pools that directly connect `mint_a` and `mint_b`.
    pub fn direct_pools(&self, mint_a: &Pubkey, mint_b: &Pubkey) -> Vec<String> {
        self.edges
            .get(mint_a)
            .map(|edges| {
                edges
                    .iter()
                    .filter(|e| e.other_mint == *mint_b)
                    .map(|e| e.pool_address.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Total number of pools in the index.
    pub fn pool_count(&self) -> usize {
        self.pools.len()
    }

    /// Number of unique token mints in the graph.
    pub fn unique_mints(&self) -> usize {
        self.edges.len()
    }

    /// Per-DEX pool counts.
    pub fn dex_counts(&self) -> &HashMap<String, usize> {
        &self.dex_counts
    }

    /// All mints that have at least one pool (for iteration).
    pub fn all_mints(&self) -> Vec<Pubkey> {
        self.edges.keys().copied().collect()
    }
}
