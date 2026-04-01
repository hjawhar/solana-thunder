//! Simulate a buy transaction without sending it.
//!
//! Loads pools, finds the best route for SOL → USDC, builds the versioned
//! transaction, signs it, and calls `simulateTransaction` on the RPC.
//!
//! Run:
//!   RPC_URL="https://..." cargo run --release -p thunder-aggregator --example simulate_swap
//!
//! Requires PRIVATE_KEY in .env (base58 keypair). Transaction is NEVER sent.

use std::collections::HashMap;
use std::env;

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
use thunder_aggregator::{loader, price, router::Router};
use thunder_core::{
    calculate_min_amount_out, GenericError, SwapArgs, SwapContext, SwapDirection,
    TOKEN_PROGRAM, USDC, WSOL,
};

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let rpc_url =
        env::var("RPC_URL").unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    let private_key = env::var("PRIVATE_KEY").expect("PRIVATE_KEY not set in .env");

    let keypair = Keypair::from_base58_string(&private_key);
    let user = keypair.pubkey();

    println!("Solana Thunder - Swap Simulator");
    println!("===============================\n");
    println!("RPC:    {rpc_url}");
    println!("Wallet: {user}\n");

    let rpc = RpcClient::new_with_commitment(rpc_url.clone(), CommitmentConfig::confirmed());

    // 1. Fetch SOL/USD price
    let sol_usd = price::fetch_sol_usd_onchain(&rpc).await;
    if let Some(p) = sol_usd {
        println!("SOL/USD: ${p:.2} (on-chain CLMM)\n");
    }

    // 2. Load pools
    println!("Loading pools...");
    let loader = loader::PoolLoader::new(&rpc_url);
    let progress_cb: loader::ProgressCallback = Box::new(|_| {}); // silent
    let index = loader.load_all(&progress_cb).await.expect("Failed to load pools");
    println!("Loaded {} pools across {} tokens\n", index.pool_count(), index.unique_mints());

    // 3. Find best route: SOL → USDC, 0.01 SOL
    let input_mint = Pubkey::from_str_const(WSOL);
    let output_mint = Pubkey::from_str_const(USDC);
    let amount_in: u64 = 10_000_000; // 0.01 SOL

    println!("Finding route: 0.01 SOL -> USDC...");
    // Use max_hops=1 for simulation — multi-hop without Address Lookup Tables
    // exceeds the 1232-byte transaction limit. ALT support is the next step.
    let router = Router::new(&index, 1);
    let quote = router
        .find_routes(input_mint, output_mint, amount_in, 10)
        .expect("Route finding failed");

    if quote.routes.is_empty() {
        println!("No routes found!");
        return;
    }

    // Try each route until one builds successfully (some pools may be inactive).
    let recent_blockhash = rpc.get_latest_blockhash().await.expect("Failed to get blockhash");
    let slippage_bps = 500; // 5% slippage for simulation
    let mut tx = None;
    let mut chosen_route = None;

    for (ri, route) in quote.routes.iter().enumerate() {
        println!("Route {} ({} hops):", ri + 1, route.hops.len());
        for (i, hop) in route.hops.iter().enumerate() {
            println!(
                "  Hop {}: {} -> {} via {} ({})",
                i + 1,
                &hop.input_mint.to_string()[..8],
                &hop.output_mint.to_string()[..8],
                &hop.pool_address[..8],
                hop.dex_name,
            );
        }
        println!("  Expected output: {} raw\n", route.output_amount);

        match build_signed_transaction(route, &user, &keypair, slippage_bps, &index, recent_blockhash) {
            Ok(built) => {
                println!("  -> Transaction built successfully\n");
                tx = Some(built);
                chosen_route = Some(ri);
                break;
            }
            Err(e) => {
                println!("  -> Build failed: {e}, trying next route...\n");
            }
        }
    }

    let tx = match tx {
        Some(t) => t,
        None => {
            println!("All routes failed to build a transaction");
            return;
        }
    };
    println!("Using route {}\n", chosen_route.unwrap() + 1);

    let tx_size = bincode::serialize(&tx).map(|b| b.len()).unwrap_or(0);
    println!("Transaction size: {} bytes", tx_size);
    println!("Instructions: {}", match &tx.message {
        VersionedMessage::V0(m) => m.instructions.len(),
        VersionedMessage::Legacy(m) => m.instructions.len(),
    });
    println!();

    // 5. Simulate (NEVER SEND)
    println!("Simulating transaction (NOT sending)...");
    let config = RpcSimulateTransactionConfig {
        sig_verify: false,
        commitment: Some(CommitmentConfig::confirmed()),
        replace_recent_blockhash: true, // Use a fresh blockhash for simulation
        ..Default::default()
    };

    match rpc.simulate_transaction_with_config(&tx, config).await {
        Ok(result) => {
            if let Some(err) = &result.value.err {
                println!("Simulation FAILED: {err:?}");
            } else {
                println!("Simulation SUCCESS");
            }

            if let Some(logs) = &result.value.logs {
                println!("\nProgram logs:");
                for log in logs {
                    println!("  {log}");
                }
            }

            println!("\nUnits consumed: {:?}", result.value.units_consumed);
        }
        Err(e) => {
            println!("Simulation RPC error: {e}");
        }
    }

    println!("\n(Transaction was NOT sent to the network)");
}

/// Build and sign a versioned transaction from a route.
fn build_signed_transaction(
    route: &thunder_aggregator::types::Route,
    user: &Pubkey,
    keypair: &Keypair,
    slippage_bps: u64,
    index: &thunder_aggregator::pool_index::PoolIndex,
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

    // Sign the transaction
    let sig = keypair.sign_message(tx.message.serialize().as_slice());
    tx.signatures[0] = sig;

    Ok(tx)
}
