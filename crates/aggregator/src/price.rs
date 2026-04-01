//! Token price discovery using the pool graph.
//!
//! Derives SOL and USD prices by finding the highest-liquidity direct pool
//! for each pair. Falls back to Jupiter Price API for SOL/USD when no
//! on-chain USDC/WSOL pool is loaded.

use solana_pubkey::Pubkey;
use thunder_core::{GenericError, USDC, WSOL};

use crate::pool_index::PoolIndex;
use crate::types::TokenPrice;

/// Get the price of a token in SOL and USD.
///
/// - If `mint` is WSOL, `price_sol` is `None` (it *is* SOL).
/// - USD price prefers a direct USDC pool for `mint`; otherwise derives it
///   via `price_sol * sol_usd_price`.
pub fn get_token_price(index: &PoolIndex, mint: &Pubkey) -> Result<TokenPrice, GenericError> {
    let wsol = Pubkey::from_str_const(WSOL);
    let usdc = Pubkey::from_str_const(USDC);

    if *mint == wsol {
        let usd_price = get_sol_usd_price(index);
        return Ok(TokenPrice {
            mint: *mint,
            price_sol: None,
            price_usd: usd_price,
        });
    }

    let price_sol = best_pool_price(index, mint, &wsol);

    // Try direct USDC pool first; fall back to SOL-denominated price * SOL/USD.
    let price_usd = if let Some(direct_usd) = best_pool_price(index, mint, &usdc) {
        Some(direct_usd)
    } else if let Some(sol_price) = &price_sol {
        get_sol_usd_price(index).map(|usd| sol_price * usd)
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
fn get_sol_usd_price(index: &PoolIndex) -> Option<f64> {
    let wsol = Pubkey::from_str_const(WSOL);
    let usdc = Pubkey::from_str_const(USDC);

    let pool_addrs = index.direct_pools(&wsol, &usdc);
    let mut best_price: Option<f64> = None;
    let mut best_liquidity = 0u64;

    for addr in pool_addrs {
        let pool = index.get_pool(&addr)?;
        let fin = pool.market.financials().ok()?;
        let liquidity = fin.quote_balance;
        if liquidity <= best_liquidity {
            continue;
        }
        let meta = pool.market.metadata().ok()?;
        let raw_price = pool.market.current_price().ok()?;

        // current_price() returns quote_per_base.
        // If quote=USDC, base=WSOL → price is already USD/SOL.
        // If quote=WSOL, base=USDC → invert to get USD/SOL.
        let price = if meta.quote_mint == usdc { raw_price } else { 1.0 / raw_price };

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
        let pool = index.get_pool(&addr)?;
        let fin = pool.market.financials().ok()?;
        let liquidity = fin.quote_balance.saturating_add(fin.base_balance);
        if liquidity <= best_liquidity {
            continue;
        }
        let meta = pool.market.metadata().ok()?;
        let raw_price = pool.market.current_price().ok()?;

        // current_price() = quote_per_base.
        // If base=mint_a → price is already mint_b/mint_a.
        // If base=mint_b → invert to get mint_b/mint_a.
        let adjusted = if meta.base_mint == *mint_a { raw_price } else { 1.0 / raw_price };

        best_price = Some(adjusted);
        best_liquidity = liquidity;
    }

    best_price
}

/// Fetch SOL/USD price from Jupiter Price API v2 (async fallback).
pub async fn fetch_sol_usd_price_api() -> Option<f64> {
    let url = "https://api.jup.ag/price/v2?ids=So11111111111111111111111111111111111111112";
    let resp = reqwest::get(url).await.ok()?;
    let json: serde_json::Value = resp.json().await.ok()?;
    json["data"]["So11111111111111111111111111111111111111112"]["price"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
}
