//! Token price discovery using the pool graph.
//!
//! Derives SOL and USD prices by finding the highest-liquidity direct pool
//! for each pair. Falls back to Jupiter Price API for SOL/USD when no
//! on-chain USDC/WSOL pool qualifies.

use solana_pubkey::Pubkey;
use thunder_core::{GenericError, USDC, WSOL};

use crate::pool_index::PoolIndex;
use crate::types::TokenPrice;

/// Minimum combined vault balance for a pool to be used in pricing.
/// Set high enough to exclude dust pools that produce unreliable prices.
const MIN_PRICING_LIQUIDITY: u64 = 10_000_000_000; // ~10 SOL equivalent

/// Get the price of a token in SOL and USD.
///
/// `sol_usd_override` lets the caller inject a known SOL/USD price
/// (e.g., from Jupiter API) so the sync function doesn't need async.
pub fn get_token_price(
    index: &PoolIndex,
    mint: &Pubkey,
    sol_usd_override: Option<f64>,
) -> Result<TokenPrice, GenericError> {
    let wsol = Pubkey::from_str_const(WSOL);
    let usdc = Pubkey::from_str_const(USDC);

    let sol_usd = sol_usd_override.or_else(|| get_sol_usd_price(index));

    if *mint == wsol {
        return Ok(TokenPrice {
            mint: *mint,
            price_sol: None,
            price_usd: sol_usd,
        });
    }

    let price_sol = best_pool_price(index, mint, &wsol);

    // Try direct USDC pool first; fall back to SOL-denominated price * SOL/USD.
    let price_usd = if let Some(direct_usd) = best_pool_price(index, mint, &usdc) {
        Some(direct_usd)
    } else if let Some(sol_price) = &price_sol {
        sol_usd.map(|usd| sol_price * usd)
    } else {
        None
    };

    Ok(TokenPrice {
        mint: *mint,
        price_sol,
        price_usd,
    })
}

/// SOL price in USD from the highest-liquidity USDC/WSOL pool.
pub fn get_sol_usd_price(index: &PoolIndex) -> Option<f64> {
    let wsol = Pubkey::from_str_const(WSOL);
    let usdc = Pubkey::from_str_const(USDC);

    let pool_addrs = index.direct_pools(&wsol, &usdc);
    let mut best_price: Option<f64> = None;
    let mut best_liquidity = 0u64;

    for addr in pool_addrs {
        let Some(pool) = index.get_pool(&addr) else { continue };
        let Ok(fin) = pool.market.financials() else { continue };
        let liquidity = fin.quote_balance.saturating_add(fin.base_balance);
        if liquidity < MIN_PRICING_LIQUIDITY || liquidity <= best_liquidity {
            continue;
        }
        let Ok(meta) = pool.market.metadata() else { continue };
        let Ok(raw_price) = pool.market.current_price() else { continue };

        // current_price() returns quote_per_base.
        // If quote=USDC, base=WSOL → price is already USD/SOL.
        // If quote=WSOL, base=USDC → invert to get USD/SOL.
        let price = if meta.quote_mint == usdc { raw_price } else { 1.0 / raw_price };

        if !price.is_finite() || price <= 0.0 {
            continue;
        }

        best_price = Some(price);
        best_liquidity = liquidity;
    }

    best_price
}

/// Price of `mint_a` denominated in `mint_b`, from the highest-liquidity
/// direct pool connecting the two mints.
fn best_pool_price(index: &PoolIndex, mint_a: &Pubkey, mint_b: &Pubkey) -> Option<f64> {
    let pool_addrs = index.direct_pools(mint_a, mint_b);
    let mut best_price: Option<f64> = None;
    let mut best_liquidity = 0u64;

    for addr in pool_addrs {
        let Some(pool) = index.get_pool(&addr) else { continue };
        let Ok(fin) = pool.market.financials() else { continue };
        let liquidity = fin.quote_balance.saturating_add(fin.base_balance);
        if liquidity < MIN_PRICING_LIQUIDITY || liquidity <= best_liquidity {
            continue;
        }
        let Ok(meta) = pool.market.metadata() else { continue };
        let Ok(raw_price) = pool.market.current_price() else { continue };

        // current_price() = quote_per_base.
        // If base=mint_a → price is already mint_b/mint_a.
        // If base=mint_b → invert to get mint_b/mint_a.
        let adjusted = if meta.base_mint == *mint_a { raw_price } else { 1.0 / raw_price };

        if !adjusted.is_finite() || adjusted <= 0.0 {
            continue;
        }

        best_price = Some(adjusted);
        best_liquidity = liquidity;
    }

    best_price
}

/// Fetch SOL/USD price from CoinGecko free API.
pub async fn fetch_sol_usd_price_api() -> Option<f64> {
    let url = "https://api.coingecko.com/api/v3/simple/price?ids=solana&vs_currencies=usd";
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("thunder-aggregator/0.1.0")
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Price API client build failed: {e}");
            return None;
        }
    };
    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Price API request failed: {e}");
            return None;
        }
    };
    let text = match resp.text().await {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Price API read body failed: {e}");
            return None;
        }
    };
    let json: serde_json::Value = match serde_json::from_str(&text) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("Price API parse failed: {e}, body: {}", &text[..text.len().min(200)]);
            return None;
        }
    };
    let price = json["solana"]["usd"].as_f64();
    if price.is_none() {
        eprintln!("Price API: 'solana.usd' not found in response: {}", &text[..text.len().min(200)]);
    }
    price
}
