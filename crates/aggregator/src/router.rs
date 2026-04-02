//! Multi-hop route discovery: finds optimal swap paths between any two tokens.
//!
//! Searches 1-hop through 4-hop routes using hub mints and bidirectional
//! neighbor exploration. All candidate paths are simulated end-to-end with
//! the actual input amount, then ranked by output amount descending.

use std::collections::HashSet;
use std::sync::Arc;

use solana_pubkey::Pubkey;
use thunder_core::{GenericError, SwapDirection, JITOSOL, MSOL, USDC, USDT, WSOL};

use crate::pool_index::PoolIndex;
use crate::types::{Quote, Route, RouteHop};

/// Hub mints — high-liquidity tokens used as intermediate routing nodes.
const HUB_MINTS: [&str; 5] = [WSOL, USDC, USDT, JITOSOL, MSOL];

/// Smaller hub set for higher-hop routes to bound search space.
const HUB_MINTS_CORE: [&str; 3] = [WSOL, USDC, USDT];

/// Max neighbors explored per side in bidirectional search.
const MAX_NEIGHBOR_CANDIDATES: usize = 50;

/// Minimum vault balance (raw units) for a pool to be routable.
const MIN_VAULT_BALANCE: u64 = 10_000_000; // 0.01 SOL

pub struct Router<'a> {
    index: &'a PoolIndex,
    max_hops: usize,
    swappable_set: Option<Arc<HashSet<String>>>,
}

impl<'a> Router<'a> {
    pub fn new(index: &'a PoolIndex, max_hops: usize) -> Self {
        Self {
            index,
            max_hops,
            swappable_set: None,
        }
    }

    /// Restrict routing to only the given pool addresses.
    pub fn with_swappable_set(mut self, set: Arc<HashSet<String>>) -> Self {
        self.swappable_set = Some(set);
        self
    }

    /// Find the best routes from `input_mint` to `output_mint` for `amount_in`.
    ///
    /// Returns up to `max_routes` routes sorted by output amount descending.
    pub fn find_routes(
        &self,
        input_mint: Pubkey,
        output_mint: Pubkey,
        amount_in: u64,
        max_routes: usize,
    ) -> Result<Quote, GenericError> {
        if input_mint == output_mint || amount_in == 0 {
            return Ok(Quote { routes: vec![] });
        }

        let mut candidates: Vec<Route> = Vec::new();

        let hubs: Vec<Pubkey> = HUB_MINTS
            .iter()
            .map(|s| Pubkey::from_str_const(s))
            .filter(|h| *h != input_mint && *h != output_mint)
            .collect();

        let core_hubs: Vec<Pubkey> = HUB_MINTS_CORE
            .iter()
            .map(|s| Pubkey::from_str_const(s))
            .filter(|h| *h != input_mint && *h != output_mint)
            .collect();

        // === 1-hop: direct pools ===
        if self.max_hops >= 1 {
            self.find_direct(&input_mint, &output_mint, amount_in, &mut candidates);
        }

        // === 2-hop ===
        if self.max_hops >= 2 {
            // Via hub mints
            for hub in &hubs {
                self.try_2hop(&input_mint, hub, &output_mint, amount_in, &mut candidates);
            }

            // Via neighbors of input_mint (forward search)
            self.neighbor_2hop_forward(
                &input_mint,
                &output_mint,
                amount_in,
                &hubs,
                &mut candidates,
            );

            // Via neighbors of output_mint (reverse search)
            self.neighbor_2hop_reverse(
                &input_mint,
                &output_mint,
                amount_in,
                &hubs,
                &mut candidates,
            );
        }

        // === 3-hop ===
        if self.max_hops >= 3 {
            // Hub-hub: input → hub1 → hub2 → output
            for (i, h1) in core_hubs.iter().enumerate() {
                for h2 in &core_hubs[i + 1..] {
                    self.try_3hop(&input_mint, h1, h2, &output_mint, amount_in, &mut candidates);
                    self.try_3hop(&input_mint, h2, h1, &output_mint, amount_in, &mut candidates);
                }
            }

            // Neighbor-hub: input → neighbor → hub → output
            // and: input → hub → neighbor → output
            self.neighbor_3hop(&input_mint, &output_mint, amount_in, &core_hubs, &mut candidates);
        }

        // === 4-hop ===
        if self.max_hops >= 4 {
            // input → neighbor_in → hub → neighbor_out → output
            self.neighbor_4hop(&input_mint, &output_mint, amount_in, &core_hubs, &mut candidates);
        }

        // Sort by output amount descending, truncate to max_routes.
        candidates.sort_unstable_by(|a, b| b.output_amount.cmp(&a.output_amount));
        candidates.truncate(max_routes);

        Ok(Quote { routes: candidates })
    }

    // =====================================================================
    // Search strategies
    // =====================================================================

    /// All direct (1-hop) routes.
    fn find_direct(
        &self,
        input: &Pubkey,
        output: &Pubkey,
        amount_in: u64,
        out: &mut Vec<Route>,
    ) {
        for addr in self.index.direct_pools(input, output) {
            if let Some(route) = simulate_path(self.index, &[(addr, *input, *output)], amount_in, self.swappable_set.as_deref()) {
                out.push(route);
            }
        }
    }

    /// 2-hop through a specific intermediate mint. Picks best pool per leg.
    fn try_2hop(
        &self,
        input: &Pubkey,
        mid: &Pubkey,
        output: &Pubkey,
        amount_in: u64,
        out: &mut Vec<Route>,
    ) {
        let leg1 = self.index.direct_pools(input, mid);
        let leg2 = self.index.direct_pools(mid, output);
        if leg1.is_empty() || leg2.is_empty() {
            return;
        }

        // Try top N pools per leg to find viable routes (not just the single best).
        let top_leg1 = top_pools(self.index, &leg1, *input, amount_in, 3, self.swappable_set.as_deref());
        for (a1, mid_amount) in &top_leg1 {
            let top_leg2 = top_pools(self.index, &leg2, *mid, *mid_amount, 3, self.swappable_set.as_deref());
            for (a2, _) in &top_leg2 {
                if let Some(route) = simulate_path(
                    self.index,
                    &[(a1.clone(), *input, *mid), (a2.clone(), *mid, *output)],
                    amount_in,
                    self.swappable_set.as_deref(),
                ) {
                    out.push(route);
                }
            }
        }
    }

    /// 2-hop: explore neighbors of input_mint (forward search).
    fn neighbor_2hop_forward(
        &self,
        input: &Pubkey,
        output: &Pubkey,
        amount_in: u64,
        skip: &[Pubkey],
        out: &mut Vec<Route>,
    ) {
        let skip_set: HashSet<Pubkey> = skip.iter().copied().collect();
        let mut tried = 0usize;

        for (mid, pool_addr) in &self.index.neighbors(input) {
            if tried >= MAX_NEIGHBOR_CANDIDATES {
                break;
            }
            if *mid == *input || *mid == *output || skip_set.contains(mid) {
                continue;
            }

            let leg2 = self.index.direct_pools(mid, output);
            if leg2.is_empty() {
                tried += 1;
                continue;
            }

            let Some(hop1) = simulate_hop(self.index, pool_addr, *input, amount_in, self.swappable_set.as_deref()) else {
                tried += 1;
                continue;
            };
            if hop1.output_amount == 0 {
                tried += 1;
                continue;
            }

            let Some((a2, _)) = best_pool(self.index, &leg2, *mid, hop1.output_amount, self.swappable_set.as_deref()) else {
                tried += 1;
                continue;
            };

            if let Some(route) = simulate_path(
                self.index,
                &[(pool_addr.clone(), *input, *mid), (a2, *mid, *output)],
                amount_in,
                self.swappable_set.as_deref(),
            ) {
                out.push(route);
            }

            tried += 1;
        }
    }

    /// 2-hop: explore neighbors of output_mint (reverse search).
    /// For each neighbor `mid` of output, check if input → mid has a pool.
    fn neighbor_2hop_reverse(
        &self,
        input: &Pubkey,
        output: &Pubkey,
        amount_in: u64,
        skip: &[Pubkey],
        out: &mut Vec<Route>,
    ) {
        let skip_set: HashSet<Pubkey> = skip.iter().copied().collect();
        let mut tried = 0usize;

        for (mid, _pool_to_output) in &self.index.neighbors(output) {
            if tried >= MAX_NEIGHBOR_CANDIDATES {
                break;
            }
            if *mid == *input || *mid == *output || skip_set.contains(mid) {
                continue;
            }

            let leg1 = self.index.direct_pools(input, mid);
            if leg1.is_empty() {
                tried += 1;
                continue;
            }

            // Simulate: input → mid (best pool) → output (best pool)
            let Some((a1, mid_amount)) = best_pool(self.index, &leg1, *input, amount_in, self.swappable_set.as_deref()) else {
                tried += 1;
                continue;
            };

            let leg2 = self.index.direct_pools(mid, output);
            let Some((a2, _)) = best_pool(self.index, &leg2, *mid, mid_amount, self.swappable_set.as_deref()) else {
                tried += 1;
                continue;
            };

            if let Some(route) = simulate_path(
                self.index,
                &[(a1, *input, *mid), (a2, *mid, *output)],
                amount_in,
                self.swappable_set.as_deref(),
            ) {
                out.push(route);
            }

            tried += 1;
        }
    }

    /// 3-hop through two specific intermediates.
    fn try_3hop(
        &self,
        input: &Pubkey,
        h1: &Pubkey,
        h2: &Pubkey,
        output: &Pubkey,
        amount_in: u64,
        out: &mut Vec<Route>,
    ) {
        let l1 = self.index.direct_pools(input, h1);
        let l2 = self.index.direct_pools(h1, h2);
        let l3 = self.index.direct_pools(h2, output);
        if l1.is_empty() || l2.is_empty() || l3.is_empty() {
            return;
        }

        let Some((a1, amt1)) = best_pool(self.index, &l1, *input, amount_in, self.swappable_set.as_deref()) else { return };
        let Some((a2, amt2)) = best_pool(self.index, &l2, *h1, amt1, self.swappable_set.as_deref()) else { return };
        let Some((a3, _)) = best_pool(self.index, &l3, *h2, amt2, self.swappable_set.as_deref()) else { return };

        if let Some(route) = simulate_path(
            self.index,
            &[(a1, *input, *h1), (a2, *h1, *h2), (a3, *h2, *output)],
            amount_in,
            self.swappable_set.as_deref(),
        ) {
            out.push(route);
        }
    }

    /// 3-hop via neighbor + hub.
    /// Tries: input → neighbor → hub → output  AND  input → hub → neighbor → output.
    fn neighbor_3hop(
        &self,
        input: &Pubkey,
        output: &Pubkey,
        amount_in: u64,
        hubs: &[Pubkey],
        out: &mut Vec<Route>,
    ) {
        // Forward: input → neighbor_of_input → hub → output
        let mut tried = 0usize;
        for (mid, _) in &self.index.neighbors(input) {
            if tried >= MAX_NEIGHBOR_CANDIDATES / 2 {
                break;
            }
            if *mid == *input || *mid == *output || hubs.contains(mid) {
                continue;
            }
            for hub in hubs {
                self.try_3hop(input, mid, hub, output, amount_in, out);
            }
            tried += 1;
        }

        // Reverse: input → hub → neighbor_of_output → output
        tried = 0;
        for (mid, _) in &self.index.neighbors(output) {
            if tried >= MAX_NEIGHBOR_CANDIDATES / 2 {
                break;
            }
            if *mid == *input || *mid == *output || hubs.contains(mid) {
                continue;
            }
            for hub in hubs {
                self.try_3hop(input, hub, mid, output, amount_in, out);
            }
            tried += 1;
        }
    }

    /// 4-hop: input → neighbor_in → hub → neighbor_out → output.
    /// Meets in the middle at a hub mint.
    fn neighbor_4hop(
        &self,
        input: &Pubkey,
        output: &Pubkey,
        amount_in: u64,
        hubs: &[Pubkey],
        out: &mut Vec<Route>,
    ) {
        // Collect neighbors of input that connect to any hub.
        let in_neighbors: Vec<(Pubkey, String)> = self
            .index
            .neighbors(input)
            .into_iter()
            .filter(|(mid, _)| *mid != *input && *mid != *output && !hubs.contains(mid))
            .take(MAX_NEIGHBOR_CANDIDATES / 4)
            .collect();

        // Collect neighbors of output that connect to any hub.
        let out_neighbors: Vec<(Pubkey, String)> = self
            .index
            .neighbors(output)
            .into_iter()
            .filter(|(mid, _)| *mid != *input && *mid != *output && !hubs.contains(mid))
            .take(MAX_NEIGHBOR_CANDIDATES / 4)
            .collect();

        for hub in hubs {
            for (n_in, _) in &in_neighbors {
                // Check n_in connects to hub
                if self.index.direct_pools(n_in, hub).is_empty() {
                    continue;
                }
                for (n_out, _) in &out_neighbors {
                    if n_in == n_out {
                        continue;
                    }
                    // Check hub connects to n_out
                    if self.index.direct_pools(hub, n_out).is_empty() {
                        continue;
                    }

                    // input → n_in → hub → n_out → output
                    let l1 = self.index.direct_pools(input, n_in);
                    let l2 = self.index.direct_pools(n_in, hub);
                    let l3 = self.index.direct_pools(hub, n_out);
                    let l4 = self.index.direct_pools(n_out, output);

                    if l1.is_empty() || l2.is_empty() || l3.is_empty() || l4.is_empty() {
                        continue;
                    }

                    let Some((a1, amt1)) = best_pool(self.index, &l1, *input, amount_in, self.swappable_set.as_deref())
                    else {
                        continue;
                    };
                    let Some((a2, amt2)) = best_pool(self.index, &l2, *n_in, amt1, self.swappable_set.as_deref()) else {
                        continue;
                    };
                    let Some((a3, amt3)) = best_pool(self.index, &l3, *hub, amt2, self.swappable_set.as_deref()) else {
                        continue;
                    };
                    let Some((a4, _)) = best_pool(self.index, &l4, *n_out, amt3, self.swappable_set.as_deref()) else {
                        continue;
                    };

                    if let Some(route) = simulate_path(
                        self.index,
                        &[
                            (a1, *input, *n_in),
                            (a2, *n_in, *hub),
                            (a3, *hub, *n_out),
                            (a4, *n_out, *output),
                        ],
                        amount_in,
                        self.swappable_set.as_deref(),
                    ) {
                        out.push(route);
                    }
                }
            }
        }
    }
}

// =============================================================================
// Simulation helpers
// =============================================================================

/// Simulate a single hop: determine direction from pre-resolved mints, compute output.
/// Avoids metadata() allocation entirely — direction comes from PoolEntry.quote_mint/base_mint.
fn simulate_hop(
    index: &PoolIndex,
    pool_address: &str,
    input_mint: Pubkey,
    amount_in: u64,
    swappable: Option<&HashSet<String>>,
) -> Option<RouteHop> {
    if let Some(set) = swappable {
        if !set.contains(pool_address) {
            return None;
        }
    }

    let entry = index.get_pool(pool_address)?;

    // When no swappable set, fall back to per-hop checks.
    if swappable.is_none() {
        if !entry.market.is_active() {
            return None;
        }
        if let Ok(fin) = entry.market.financials() {
            if fin.quote_balance < MIN_VAULT_BALANCE && fin.base_balance < MIN_VAULT_BALANCE {
                return None;
            }
        }
    }

    // Direction from pre-resolved mints — no metadata() call needed.
    let (direction, output_mint) = if input_mint == entry.quote_mint {
        (SwapDirection::Buy, entry.base_mint)
    } else if input_mint == entry.base_mint {
        (SwapDirection::Sell, entry.quote_mint)
    } else {
        return None;
    };

    let output_amount = entry.market.calculate_output(amount_in, direction).ok()?;
    if output_amount == 0 {
        return None;
    }

    // Anti-dust: reject absurd output ratios.
    if output_amount > amount_in.saturating_mul(1_000_000) {
        return None;
    }

    Some(RouteHop {
        pool_address: pool_address.to_string(),
        dex_name: entry.dex_name.clone(),
        input_mint,
        output_mint,
        input_amount: amount_in,
        output_amount,
        price_impact_bps: 0,
    })
}

/// Simulate a full multi-hop path. Returns None if any hop fails or a cycle is detected.
fn simulate_path(
    index: &PoolIndex,
    hops: &[(String, Pubkey, Pubkey)],
    initial_amount: u64,
    swappable: Option<&HashSet<String>>,
) -> Option<Route> {
    if hops.is_empty() {
        return None;
    }

    // Cycle detection.
    let mut visited = HashSet::new();
    visited.insert(hops[0].1);
    for (_, _, out_mint) in hops {
        if !visited.insert(*out_mint) {
            return None;
        }
    }

    let mut result_hops = Vec::with_capacity(hops.len());
    let mut current_amount = initial_amount;
    let mut total_impact: u64 = 0;

    for (pool_address, input_mint, _) in hops {
        let hop = simulate_hop(index, pool_address, *input_mint, current_amount, swappable)?;
        current_amount = hop.output_amount;
        total_impact = total_impact.saturating_add(hop.price_impact_bps);
        result_hops.push(hop);
    }

    let first = result_hops.first()?;
    let last = result_hops.last()?;

    Some(Route {
        input_mint: first.input_mint,
        output_mint: last.output_mint,
        input_amount: initial_amount,
        output_amount: current_amount,
        price_impact_bps: total_impact,
        hops: result_hops,
    })
}

/// Among `pool_addresses`, pick the one yielding the highest output.
fn best_pool(
    index: &PoolIndex,
    pool_addresses: &[String],
    input_mint: Pubkey,
    amount_in: u64,
    swappable: Option<&HashSet<String>>,
) -> Option<(String, u64)> {
    pool_addresses
        .iter()
        .filter_map(|addr| {
            let hop = simulate_hop(index, addr, input_mint, amount_in, swappable)?;
            Some((addr.clone(), hop.output_amount))
        })
        .max_by_key(|(_, out)| *out)
}


/// Return the top N pools by output amount.
fn top_pools(
    index: &PoolIndex,
    pool_addresses: &[String],
    input_mint: Pubkey,
    amount_in: u64,
    n: usize,
    swappable: Option<&HashSet<String>>,
) -> Vec<(String, u64)> {
    let mut candidates: Vec<(String, u64)> = pool_addresses
        .iter()
        .filter_map(|addr| {
            let hop = simulate_hop(index, addr, input_mint, amount_in, swappable)?;
            Some((addr.clone(), hop.output_amount))
        })
        .collect();
    candidates.sort_unstable_by(|a, b| b.1.cmp(&a.1));
    candidates.truncate(n);
    candidates
}