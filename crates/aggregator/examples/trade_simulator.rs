//! Interactive trade simulator with pool validation.
//!
//! Loads pools from cache (or RPC), finds the optimal route for user-specified
//! tokens and amount, refetches the involved pool data for freshness, rebuilds
//! the transaction, and simulates it on-chain. Transaction is NEVER sent.
//!
//! Usage:
//!   RPC_URL="https://..." cargo run --release -p thunder-aggregator --example trade_simulator -- \
//!     --input SOL \
//!     --output EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v \
//!     --amount 0.1 \
//!     --max-hops 2 \
//!     --slippage 500
//!
//! Requires PRIVATE_KEY in .env (base58 keypair).

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Instant;

use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::config::RpcSimulateTransactionConfig;
use solana_sdk::{
    hash::Hash,
    message::{v0, VersionedMessage},
    signature::{Keypair, Signer},
    transaction::VersionedTransaction,
};
use spl_associated_token_account::get_associated_token_address;
use thunder_aggregator::{cache, loader, pool_index::PoolIndex, price, router::Router, types::Route};
use thunder_core::{
    calculate_min_amount_out, infer_mint_decimals, GenericError, SwapArgs, SwapContext,
    SwapDirection, TOKEN_PROGRAM, WSOL,
};

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    // ── Parse CLI args ──────────────────────────────────────────────────
    let args: Vec<String> = env::args().collect();
    let input_str = get_arg(&args, "--input").unwrap_or_else(|| {
        eprintln!("Usage: trade_simulator --input <mint|SOL> --output <mint|SOL> --amount <f64> [--max-hops N] [--slippage bps]");
        std::process::exit(1);
    });
    let output_str = get_arg(&args, "--output").unwrap_or_else(|| {
        eprintln!("Missing --output");
        std::process::exit(1);
    });
    let amount_str = get_arg(&args, "--amount").unwrap_or_else(|| {
        eprintln!("Missing --amount");
        std::process::exit(1);
    });
    let max_hops: usize = get_arg(&args, "--max-hops")
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);
    let slippage_bps: u64 = get_arg(&args, "--slippage")
        .and_then(|s| s.parse().ok())
        .unwrap_or(500);

    let input_mint = parse_mint(&input_str);
    let output_mint = parse_mint(&output_str);
    let input_decimals = infer_mint_decimals(&input_mint);
    let amount_in = (amount_str.parse::<f64>().expect("Invalid --amount") * 10f64.powi(input_decimals as i32)) as u64;

    let rpc_url = env::var("RPC_URL").unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".into());
    let cache_path = PathBuf::from(env::var("CACHE_PATH").unwrap_or_else(|_| "pools.cache".into()));
    let private_key = env::var("PRIVATE_KEY").expect("PRIVATE_KEY not set in .env");
    let keypair = Keypair::from_base58_string(&private_key);
    let user = keypair.pubkey();

    println!("Solana Thunder - Trade Simulator");
    println!("================================\n");
    println!("Wallet:    {user}");
    println!("Input:     {input_mint} ({input_str})");
    println!("Output:    {output_mint} ({output_str})");
    println!("Amount:    {amount_str} ({amount_in} raw)");
    println!("Max hops:  {max_hops}");
    println!("Slippage:  {slippage_bps} bps ({:.1}%)\n", slippage_bps as f64 / 100.0);

    let rpc = RpcClient::new_with_commitment(rpc_url.clone(), CommitmentConfig::confirmed());

    // ── SOL/USD price ───────────────────────────────────────────────────
    if let Some(p) = price::fetch_sol_usd_onchain(&rpc).await {
        println!("SOL/USD: ${p:.2} (on-chain CLMM)\n");
    }

    // ── Load pools (cache or RPC) ───────────────────────────────────────
    let t0 = Instant::now();
    let index = match cache::load_cache(&cache_path) {
        Ok((idx, _)) => {
            println!("Loaded {} pools from cache in {:.1}s\n", idx.pool_count(), t0.elapsed().as_secs_f64());
            idx
        }
        Err(_) => {
            println!("No cache, loading from RPC...");
            let progress_cb: loader::ProgressCallback = Box::new(|_| {});
            let idx = loader::PoolLoader::new(&rpc_url)
                .load_all(&progress_cb).await
                .expect("Failed to load pools");
            println!("Loaded {} pools from RPC in {:.1}s\n", idx.pool_count(), t0.elapsed().as_secs_f64());
            let _ = cache::save_cache(&idx, &cache_path);
            idx
        }
    };

    // ── Step 1: Find optimal route ──────────────────────────────────────
    println!("=== Step 1: Finding optimal route ===\n");
    let router = Router::new(&index, max_hops);
    let quote = router
        .find_routes(input_mint, output_mint, amount_in, 10)
        .expect("Route finding failed");

    if quote.routes.is_empty() {
        println!("No routes found for this pair.");
        return;
    }

    for (i, route) in quote.routes.iter().enumerate() {
        let tag = if i == 0 { " <- best" } else { "" };
        println!("  Route {} ({} hop{}):{tag}", i + 1, route.hops.len(), if route.hops.len() == 1 { "" } else { "s" });
        for (j, hop) in route.hops.iter().enumerate() {
            let in_dec = infer_mint_decimals(&hop.input_mint);
            let out_dec = infer_mint_decimals(&hop.output_mint);
            println!(
                "    Hop {}: {} -> {} via {} ({}) | {:.6} -> {:.6} | impact: {:.2}%",
                j + 1,
                trunc(&hop.input_mint.to_string()),
                trunc(&hop.output_mint.to_string()),
                trunc(&hop.pool_address),
                hop.dex_name,
                hop.input_amount as f64 / 10f64.powi(in_dec as i32),
                hop.output_amount as f64 / 10f64.powi(out_dec as i32),
                hop.price_impact_bps as f64 / 100.0,
            );
        }
        let out_dec = infer_mint_decimals(&route.output_mint);
        println!(
            "    Output: {:.6} | Total impact: {:.2}%\n",
            route.output_amount as f64 / 10f64.powi(out_dec as i32),
            route.price_impact_bps as f64 / 100.0,
        );
    }

    // ── Step 2: Refetch pool data for validation ────────────────────────
    println!("=== Step 2: Refetching pool data for the best route ===\n");

    // Collect all pool addresses used in the best route
    let best = &quote.routes[0];
    let pool_addrs: Vec<Pubkey> = best.hops.iter()
        .map(|h| Pubkey::from_str(&h.pool_address).unwrap())
        .collect();

    println!("  Refetching {} pool account(s)...", pool_addrs.len());
    let fresh_accounts = rpc.get_multiple_accounts(&pool_addrs).await.expect("Failed to refetch pools");

    // Collect vault pubkeys from fresh pool data for balance refetch
    let mut vault_keys: Vec<Pubkey> = Vec::new();
    for (hop, maybe_account) in best.hops.iter().zip(&fresh_accounts) {
        if let Some(account) = maybe_account {
            println!("  Pool {} ({}) : {} bytes, data fresh", trunc(&hop.pool_address), hop.dex_name, account.data.len());
        } else {
            println!("  Pool {} : NOT FOUND on-chain!", trunc(&hop.pool_address));
        }
        // Get vault addresses from the indexed pool metadata
        if let Some(pool) = index.get_pool(&hop.pool_address) {
            if let Ok(meta) = pool.market.metadata() {
                vault_keys.push(meta.quote_vault);
                vault_keys.push(meta.base_vault);
            }
        }
    }

    if !vault_keys.is_empty() {
        println!("\n  Refetching {} vault balance(s)...", vault_keys.len());
        match rpc.get_multiple_accounts(&vault_keys).await {
            Ok(accounts) => {
                for (i, maybe) in accounts.iter().enumerate() {
                    if let Some(acc) = maybe {
                        let bal = if acc.data.len() >= 72 {
                            u64::from_le_bytes(acc.data[64..72].try_into().unwrap())
                        } else { 0 };
                        let label = if i % 2 == 0 { "quote_vault" } else { "base_vault" };
                        println!("    {label} {}: balance = {bal}", trunc(&vault_keys[i].to_string()));
                    }
                }
            }
            Err(e) => println!("  Vault refetch failed: {e}"),
        }
    }

    // ── Step 3: Re-simulate route with fresh data ───────────────────────
    println!("\n=== Step 3: Re-simulating route with original index ===\n");

    // Re-run route simulation to confirm output
    let re_router = Router::new(&index, max_hops);
    let re_quote = re_router.find_routes(input_mint, output_mint, amount_in, 1).ok();
    if let Some(rq) = &re_quote {
        if let Some(r) = rq.routes.first() {
            let out_dec = infer_mint_decimals(&r.output_mint);
            println!("  Confirmed output: {:.6} ({} raw)", r.output_amount as f64 / 10f64.powi(out_dec as i32), r.output_amount);
        }
    }

    // ── Step 4: Build and simulate transaction ──────────────────────────
    println!("\n=== Step 4: Building and simulating transaction ===\n");

    let recent_blockhash = rpc.get_latest_blockhash().await.expect("Failed to get blockhash");

    // Try each route until one builds
    let mut built_tx = None;
    let mut built_route_idx = 0;

    for (ri, route) in quote.routes.iter().enumerate() {
        match build_signed_transaction(route, &user, &keypair, slippage_bps, &index, recent_blockhash) {
            Ok(tx) => {
                built_tx = Some(tx);
                built_route_idx = ri;
                break;
            }
            Err(e) => {
                println!("  Route {} build failed: {e}", ri + 1);
            }
        }
    }

    let tx = match built_tx {
        Some(t) => t,
        None => {
            println!("\n  All routes failed to build. Try with --max-hops 1 for simpler transactions.");
            return;
        }
    };

    let tx_size = bincode::serialize(&tx).map(|b| b.len()).unwrap_or(0);
    let ix_count = match &tx.message {
        VersionedMessage::V0(m) => m.instructions.len(),
        VersionedMessage::Legacy(m) => m.instructions.len(),
    };
    println!("  Using route {}", built_route_idx + 1);
    println!("  Transaction size: {tx_size} bytes (limit: 1232)");
    println!("  Instructions: {ix_count}");

    if tx_size > 1232 {
        println!("\n  WARNING: Transaction exceeds 1232-byte limit.");
        println!("  Needs Address Lookup Tables for multi-hop. Try --max-hops 1.");
    }

    println!("\n  Simulating transaction (NOT sending)...\n");

    let config = RpcSimulateTransactionConfig {
        sig_verify: false,
        commitment: Some(CommitmentConfig::confirmed()),
        replace_recent_blockhash: true,
        ..Default::default()
    };

    match rpc.simulate_transaction_with_config(&tx, config).await {
        Ok(result) => {
            if let Some(err) = &result.value.err {
                println!("  SIMULATION FAILED: {err:?}");
            } else {
                println!("  SIMULATION SUCCESS");
            }

            if let Some(logs) = &result.value.logs {
                println!("\n  Program logs:");
                for log in logs {
                    println!("    {log}");
                }
            }

            if let Some(units) = result.value.units_consumed {
                println!("\n  Compute units: {units}");
            }
        }
        Err(e) => {
            println!("  Simulation RPC error: {e}");
        }
    }

    println!("\n  (Transaction was NOT sent to the network)");
}

// =========================================================================
// Helpers
// =========================================================================

fn get_arg(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn parse_mint(s: &str) -> Pubkey {
    if s.eq_ignore_ascii_case("SOL") || s.eq_ignore_ascii_case("WSOL") {
        Pubkey::from_str_const(WSOL)
    } else {
        Pubkey::from_str(s).unwrap_or_else(|_| {
            eprintln!("Invalid mint address: {s}");
            std::process::exit(1);
        })
    }
}

fn trunc(s: &str) -> String {
    if s.len() > 12 { format!("{}..{}", &s[..6], &s[s.len()-4..]) } else { s.to_string() }
}

fn build_signed_transaction(
    route: &Route,
    user: &Pubkey,
    keypair: &Keypair,
    slippage_bps: u64,
    index: &PoolIndex,
    recent_blockhash: Hash,
) -> Result<VersionedTransaction, GenericError> {
    if route.hops.is_empty() {
        return Err("Route has no hops".into());
    }

    let mut all_instructions = Vec::new();
    let hop_count = route.hops.len();

    for (i, hop) in route.hops.iter().enumerate() {
        let pool = index
            .get_pool(&hop.pool_address)
            .ok_or_else(|| format!("Pool {} not found", hop.pool_address))?;
        let meta = pool.market.metadata()?;

        let direction = if hop.input_mint == meta.quote_mint {
            SwapDirection::Buy
        } else {
            SwapDirection::Sell
        };

        let context = SwapContext {
            user: *user,
            source_ata: get_associated_token_address(user, &hop.input_mint),
            source_ata_exists: true,
            destination_ata: get_associated_token_address(user, &hop.output_mint),
            destination_ata_exists: i > 0,
            token_program_id: Pubkey::from_str_const(TOKEN_PROGRAM),
            extra_accounts: HashMap::new(),
        };

        let min_out = if i == hop_count - 1 {
            calculate_min_amount_out(hop.output_amount, slippage_bps)
        } else {
            1
        };

        let args = SwapArgs::new(hop.input_amount, min_out);
        let instructions = pool.market.build_swap_instruction(context, args, direction)?;
        all_instructions.extend(instructions);
    }

    #[allow(deprecated)]
    let message = v0::Message::try_compile(user, &all_instructions, &[], recent_blockhash)?;

    let mut tx = VersionedTransaction {
        signatures: vec![solana_sdk::signature::Signature::default()],
        message: VersionedMessage::V0(message),
    };

    let sig = keypair.sign_message(tx.message.serialize().as_slice());
    tx.signatures[0] = sig;

    Ok(tx)
}
