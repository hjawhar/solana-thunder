//! Multi-hop route discovery: finds optimal paths between any two tokens.
//!
//! Searches direct (1-hop), 2-hop (via hub or neighbor), and 3-hop routes,
//! then ranks by output amount descending.

use std::collections::HashSet;

use solana_pubkey::Pubkey;
use thunder_core::{GenericError, SwapDirection, JITOSOL, MSOL, USDC, USDT, WSOL};

use crate::pool_index::PoolIndex;
use crate::types::{Quote, Route, RouteHop};

/// Hub mints used for multi-hop routing (highest liquidity tokens).
const HUB_MINTS: [&str; 5] = [WSOL, USDC, USDT, JITOSOL, MSOL];

/// Tighter hub set for 3-hop routes to bound combinatorial explosion.
const HUB_MINTS_3HOP: [&str; 3] = [WSOL, USDC, USDT];

/// Max intermediate mints explored for neighbor-based 2-hop search.
const MAX_INTERMEDIATE_CANDIDATES: usize = 50;

/// Minimum vault balance (raw units) for a pool to be routable.
/// Pools with both vaults below this are skipped as dust.
const MIN_VAULT_BALANCE: u64 = 10_000_000; // 0.01 SOL / 10 USDC

/// Maximum acceptable price impact (bps) for a single hop.
/// Routes through pools with higher impact are discarded.
const MAX_HOP_IMPACT_BPS: u64 = 5000; // 50%

pub struct Router<'a> {
    index: &'a PoolIndex,
    max_hops: usize,
}

impl<'a> Router<'a> {
    pub fn new(index: &'a PoolIndex, max_hops: usize) -> Self {
        Self { index, max_hops }
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

        // --- 1-hop: direct pools ---
        if self.max_hops >= 1 {
            self.find_direct(&input_mint, &output_mint, amount_in, &mut candidates);
        }

        // --- 2-hop: via hub mints ---
        if self.max_hops >= 2 {
            let hub_pubkeys: Vec<Pubkey> = HUB_MINTS
                .iter()
                .map(|s| Pubkey::from_str_const(s))
                .collect();

            for hub in &hub_pubkeys {
                if *hub == input_mint || *hub == output_mint {
                    continue;
                }
                self.find_2hop(
                    &input_mint,
                    hub,
                    &output_mint,
                    amount_in,
                    &mut candidates,
                );
            }

            // --- 2-hop: via any neighbor of input_mint ---
            self.find_2hop_neighbors(
                &input_mint,
                &output_mint,
                amount_in,
                &hub_pubkeys,
                &mut candidates,
            );
        }

        // --- 3-hop: only if no shorter routes found ---
        if self.max_hops >= 3 && candidates.is_empty() {
            let hub3: Vec<Pubkey> = HUB_MINTS_3HOP
                .iter()
                .map(|s| Pubkey::from_str_const(s))
                .collect();

            for (i, h1) in hub3.iter().enumerate() {
                if *h1 == input_mint || *h1 == output_mint {
                    continue;
                }
                for h2 in &hub3[i + 1..] {
                    if *h2 == input_mint || *h2 == output_mint || h1 == h2 {
                        continue;
                    }
                    // Try both orderings: input→h1→h2→output and input→h2→h1→output
                    self.find_3hop(
                        &input_mint,
                        h1,
                        h2,
                        &output_mint,
                        amount_in,
                        &mut candidates,
                    );
                    self.find_3hop(
                        &input_mint,
                        h2,
                        h1,
                        &output_mint,
                        amount_in,
                        &mut candidates,
                    );
                }
            }
        }

        // Sort by output amount descending, truncate to max_routes.
        candidates.sort_unstable_by(|a, b| b.output_amount.cmp(&a.output_amount));
        candidates.truncate(max_routes);

        Ok(Quote { routes: candidates })
    }

    // -- private helpers --------------------------------------------------

    /// Enumerate all direct (1-hop) routes between two mints.
    fn find_direct(
        &self,
        input_mint: &Pubkey,
        output_mint: &Pubkey,
        amount_in: u64,
        out: &mut Vec<Route>,
    ) {
        for addr in self.index.direct_pools(input_mint, output_mint) {
            if let Some(route) = simulate_path(
                self.index,
                &[(addr, *input_mint, *output_mint)],
                amount_in,
            ) {
                out.push(route);
            }
        }
    }

    /// Enumerate 2-hop routes through a specific intermediate mint.
    fn find_2hop(
        &self,
        input_mint: &Pubkey,
        mid: &Pubkey,
        output_mint: &Pubkey,
        amount_in: u64,
        out: &mut Vec<Route>,
    ) {
        let leg1_pools = self.index.direct_pools(input_mint, mid);
        if leg1_pools.is_empty() {
            return;
        }
        let leg2_pools = self.index.direct_pools(mid, output_mint);
        if leg2_pools.is_empty() {
            return;
        }

        // Pick best pool per leg to avoid combinatorial blowup.
        let best_leg1 = best_pool(self.index, &leg1_pools, *input_mint, amount_in);
        let Some((addr1, mid_amount)) = best_leg1 else {
            return;
        };

        let best_leg2 = best_pool(self.index, &leg2_pools, *mid, mid_amount);
        let Some((addr2, _)) = best_leg2 else { return };

        if let Some(route) = simulate_path(
            self.index,
            &[
                (addr1, *input_mint, *mid),
                (addr2, *mid, *output_mint),
            ],
            amount_in,
        ) {
            out.push(route);
        }
    }

    /// Explore neighbors of input_mint for 2-hop routes via non-hub intermediates.
    fn find_2hop_neighbors(
        &self,
        input_mint: &Pubkey,
        output_mint: &Pubkey,
        amount_in: u64,
        already_tried: &[Pubkey],
        out: &mut Vec<Route>,
    ) {
        let skip: HashSet<Pubkey> = already_tried.iter().copied().collect();
        let neighbors = self.index.neighbors(input_mint);
        let mut tried = 0usize;

        for (mid, pool_addr) in &neighbors {
            if tried >= MAX_INTERMEDIATE_CANDIDATES {
                break;
            }
            if *mid == *input_mint || *mid == *output_mint || skip.contains(mid) {
                continue;
            }

            let leg2_pools = self.index.direct_pools(mid, output_mint);
            if leg2_pools.is_empty() {
                tried += 1;
                continue;
            }

            // Simulate first hop through the known pool.
            let Some(hop1) = simulate_hop(self.index, pool_addr, *input_mint, amount_in) else {
                tried += 1;
                continue;
            };
            if hop1.output_amount == 0 {
                tried += 1;
                continue;
            }

            // Pick best second-leg pool.
            let Some((addr2, _)) = best_pool(self.index, &leg2_pools, *mid, hop1.output_amount)
            else {
                tried += 1;
                continue;
            };

            if let Some(route) = simulate_path(
                self.index,
                &[
                    (pool_addr.clone(), *input_mint, *mid),
                    (addr2, *mid, *output_mint),
                ],
                amount_in,
            ) {
                out.push(route);
            }

            tried += 1;
        }
    }

    /// Enumerate 3-hop routes through two intermediate hubs.
    fn find_3hop(
        &self,
        input_mint: &Pubkey,
        h1: &Pubkey,
        h2: &Pubkey,
        output_mint: &Pubkey,
        amount_in: u64,
        out: &mut Vec<Route>,
    ) {
        let leg1 = self.index.direct_pools(input_mint, h1);
        if leg1.is_empty() {
            return;
        }
        let leg2 = self.index.direct_pools(h1, h2);
        if leg2.is_empty() {
            return;
        }
        let leg3 = self.index.direct_pools(h2, output_mint);
        if leg3.is_empty() {
            return;
        }

        let Some((a1, amt1)) = best_pool(self.index, &leg1, *input_mint, amount_in) else {
            return;
        };
        let Some((a2, amt2)) = best_pool(self.index, &leg2, *h1, amt1) else {
            return;
        };
        let Some((a3, _)) = best_pool(self.index, &leg3, *h2, amt2) else {
            return;
        };

        if let Some(route) = simulate_path(
            self.index,
            &[
                (a1, *input_mint, *h1),
                (a2, *h1, *h2),
                (a3, *h2, *output_mint),
            ],
            amount_in,
        ) {
            out.push(route);
        }
    }
}

// =============================================================================
// Free helpers
// =============================================================================

/// Simulate a single hop: determine direction from pool metadata, compute output.
fn simulate_hop(
    index: &PoolIndex,
    pool_address: &str,
    input_mint: Pubkey,
    amount_in: u64,
) -> Option<RouteHop> {
    let entry = index.get_pool(pool_address)?;
    let meta = entry.market.metadata().ok()?;

    // Skip pools with negligible liquidity — they produce unrealistic outputs.
    if let Ok(fin) = entry.market.financials() {
        if fin.quote_balance < MIN_VAULT_BALANCE && fin.base_balance < MIN_VAULT_BALANCE {
            return None;
        }
    }

    let (direction, output_mint) = if input_mint == meta.quote_mint {
        (SwapDirection::Buy, meta.base_mint)
    } else if input_mint == meta.base_mint {
        (SwapDirection::Sell, meta.quote_mint)
    } else {
        return None; // input_mint not in this pool
    };

    let output_amount = entry.market.calculate_output(amount_in, direction).ok()?;
    if output_amount == 0 {
        return None;
    }

    // Skip if output exceeds a reasonable multiple of input (anti-dust filter).
    // A legitimate pool should not return more than 1000x the input value.
    if output_amount > amount_in.saturating_mul(1_000_000) {
        return None;
    }

    let price_impact_bps = entry
        .market
        .calculate_price_impact(amount_in, direction)
        .unwrap_or(0);

    // Skip hops with extreme price impact — the pool is too thin.
    if price_impact_bps > MAX_HOP_IMPACT_BPS {
        return None;
    }

    Some(RouteHop {
        pool_address: pool_address.to_string(),
        dex_name: entry.dex_name.clone(),
        input_mint,
        output_mint,
        input_amount: amount_in,
        output_amount,
        price_impact_bps,
    })
}

/// Simulate a full multi-hop path, chaining outputs. Returns None if any hop fails.
///
/// Also enforces no-cycle invariant: a route must not visit the same mint twice.
fn simulate_path(
    index: &PoolIndex,
    hops: &[(String, Pubkey, Pubkey)], // (pool_address, input_mint, output_mint)
    initial_amount: u64,
) -> Option<Route> {
    if hops.is_empty() {
        return None;
    }

    // Cycle detection: collect all mints that appear in the path.
    let mut visited = HashSet::new();
    visited.insert(hops[0].1); // initial input_mint
    for (_, _, out_mint) in hops {
        if !visited.insert(*out_mint) {
            return None; // cycle detected
        }
    }

    let mut result_hops = Vec::with_capacity(hops.len());
    let mut current_amount = initial_amount;
    let mut total_impact: u64 = 0;

    for (pool_address, input_mint, _output_mint) in hops {
        let hop = simulate_hop(index, pool_address, *input_mint, current_amount)?;
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

/// Among `pool_addresses`, pick the one yielding the highest output for `input_mint` / `amount_in`.
/// Returns `(best_pool_address, best_output_amount)`.
fn best_pool(
    index: &PoolIndex,
    pool_addresses: &[String],
    input_mint: Pubkey,
    amount_in: u64,
) -> Option<(String, u64)> {
    pool_addresses
        .iter()
        .filter_map(|addr| {
            let hop = simulate_hop(index, addr, input_mint, amount_in)?;
            Some((addr.clone(), hop.output_amount))
        })
        .max_by_key(|(_, out)| *out)
}
