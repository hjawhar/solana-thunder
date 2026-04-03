//! Full flow: Engine /swap → sign → simulateTransaction on mainnet.
//!
//! The /swap endpoint builds per-DEX swap instructions for multi-hop execution.
//! This test calls the engine, signs the transaction, and simulates it against
//! mainnet state. No SOL is spent.
//!
//! Requires:
//!   - Engine running on localhost:8080
//!   - PRIVATE_KEY and RPC_URL in .env
//!
//! Run:
//!   cargo test --release --test simulate_swap -- --nocapture
//!
//! Custom params:
//!   INPUT=SOL OUTPUT=6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN AMOUNT=0.01 \
//!     cargo test --release --test simulate_swap -- --nocapture

use std::env;
use std::str::FromStr;

use serde::Deserialize;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::config::RpcSimulateTransactionConfig;
use solana_sdk::{
    message::VersionedMessage,
    signature::{Keypair, Signer},
    transaction::VersionedTransaction,
};
use thunder_core::{infer_mint_decimals, WSOL};

// ---------------------------------------------------------------------------
// /swap response types
// ---------------------------------------------------------------------------

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct SwapResponse {
    transaction: String,
    route: RouteJson,
    #[allow(dead_code)]
    blockhash: String,
    time_taken_ms: u64,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct RouteJson {
    hops: Vec<HopJson>,
    output_amount: String,
    #[allow(dead_code)]
    price_impact_bps: u64,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct HopJson {
    pool_address: String,
    dex_name: String,
    input_mint: String,
    output_mint: String,
    input_amount: String,
    output_amount: String,
}

#[tokio::test]
async fn test_simulate_swap_mainnet() {
    dotenvy::dotenv().ok();

    let keypair = Keypair::from_base58_string(
        &env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set"),
    );
    let user = keypair.pubkey();
    let rpc_url = env::var("RPC_URL").expect("RPC_URL must be set");
    let engine_url = env::var("ENGINE_URL").unwrap_or_else(|_| "http://localhost:8080".into());

    let input_str = env::var("INPUT").unwrap_or_else(|_| "SOL".into());
    let output_str = env::var("OUTPUT")
        .unwrap_or_else(|_| "6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN".into());
    let amount_str = env::var("AMOUNT").unwrap_or_else(|_| "0.01".into());
    let max_hops: usize = env::var("MAX_HOPS").ok().and_then(|s| s.parse().ok()).unwrap_or(2);
    let slippage_bps: u64 = env::var("SLIPPAGE").ok().and_then(|s| s.parse().ok()).unwrap_or(500);

    let input_mint = if input_str.eq_ignore_ascii_case("SOL") {
        Pubkey::from_str_const(WSOL)
    } else {
        Pubkey::from_str(&input_str).expect("invalid INPUT mint")
    };
    let input_decimals = infer_mint_decimals(&input_mint);
    let amount_in = (amount_str.parse::<f64>().expect("invalid AMOUNT")
        * 10f64.powi(input_decimals as i32)) as u64;

    let rpc = RpcClient::new_with_commitment(rpc_url, CommitmentConfig::confirmed());

    println!();
    println!("Thunder Swap Simulation (Mainnet)");
    println!("=================================");
    println!();
    println!("  Wallet:    {user}");
    println!("  Input:     {input_str}");
    println!("  Output:    {}...", &output_str[..12.min(output_str.len())]);
    println!("  Amount:    {amount_str} ({amount_in} raw)");
    println!("  MaxHops:   {max_hops}");
    println!("  Slippage:  {slippage_bps} bps");
    println!();

    let sol_balance = rpc.get_balance(&user).await.unwrap_or(0);
    println!("  SOL balance: {:.4}", sol_balance as f64 / 1e9);
    println!();

    // ── Step 1: Call engine POST /swap ──────────────────────────────────

    println!("Step 1: POST /swap → engine builds direct DEX swap instructions");
    println!();

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{engine_url}/swap"))
        .json(&serde_json::json!({
            "inputMint": input_str,
            "outputMint": output_str,
            "amount": amount_in,
            "userPublicKey": user.to_string(),
            "slippageBps": slippage_bps,
            "maxHops": max_hops,
        }))
        .send()
        .await
        .expect("failed to call /swap");

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        panic!("  /swap failed ({status}): {body}");
    }

    let swap: SwapResponse = resp.json().await.expect("invalid /swap response");

    println!("  Engine responded in {}ms", swap.time_taken_ms);
    println!("  Route: {} hops → {} output", swap.route.hops.len(), swap.route.output_amount);
    println!();

    for (i, hop) in swap.route.hops.iter().enumerate() {
        let out_dec = if i == swap.route.hops.len() - 1 {
            let out_mint = Pubkey::from_str(&hop.output_mint).unwrap_or_default();
            infer_mint_decimals(&out_mint)
        } else { 6 };
        let out_human = hop.output_amount.parse::<f64>().unwrap_or(0.0) / 10f64.powi(out_dec as i32);
        println!(
            "  Hop {}: {} → {:.6} via {}... ({})",
            i + 1,
            &hop.input_mint[..8],
            out_human,
            &hop.pool_address[..12],
            hop.dex_name,
        );
    }
    println!();

    // ── Step 2: Decode, replace blockhash, sign ─────────────────────────

    println!("Step 2: Decode → fresh blockhash → sign");
    println!();

    let tx_bytes = solana_sdk::bs58::decode(&swap.transaction)
        .into_vec()
        .expect("invalid base58");
    let mut tx: VersionedTransaction =
        bincode::deserialize(&tx_bytes).expect("invalid transaction");

    let blockhash = rpc.get_latest_blockhash().await.expect("blockhash fetch failed");
    match &mut tx.message {
        VersionedMessage::V0(msg) => msg.recent_blockhash = blockhash,
        _ => panic!("expected V0 message"),
    }

    let msg_bytes = tx.message.serialize();
    tx.signatures = vec![keypair.sign_message(&msg_bytes)];

    let tx_size = bincode::serialize(&tx).map(|b| b.len()).unwrap_or(0);
    println!("  Transaction: {} bytes ({} max)", tx_size, 1232);
    println!("  Num instructions: {}", match &tx.message {
        VersionedMessage::V0(m) => m.instructions.len(),
        _ => 0,
    });
    // Debug: print each instruction's program
    if let VersionedMessage::V0(m) = &tx.message {
        for (i, ix) in m.instructions.iter().enumerate() {
            let prog = m.account_keys[ix.program_id_index as usize];
            println!("  ix[{i}]: program={prog} accounts={}", ix.accounts.len());
        }
    }
    if tx_size > 1232 {
        panic!("  Transaction too large!");
    }
    println!();

    // ── Step 3: Simulate on mainnet ─────────────────────────────────────

    println!("Step 3: simulateTransaction on mainnet (no SOL spent)");
    println!();

    let sim_result = rpc
        .simulate_transaction_with_config(
            &tx,
            RpcSimulateTransactionConfig {
                sig_verify: false,
                commitment: Some(CommitmentConfig::confirmed()),
                replace_recent_blockhash: false,
                ..Default::default()
            },
        )
        .await;

    match sim_result {
        Ok(response) => {
            if let Some(err) = &response.value.err {
                println!("  ❌ SIMULATION FAILED");
                println!("  Error: {err:?}");
                println!();

                if let Some(logs) = &response.value.logs {
                    println!("  Logs ({} entries):", logs.len());
                    for (i, log) in logs.iter().enumerate() {
                        println!("    [{i:>2}] {log}");
                        if i >= 80 {
                            println!("    ... {} more", logs.len() - i - 1);
                            break;
                        }
                    }
                }
            } else {
                println!("  ✅ SIMULATION SUCCEEDED");
                println!("  Compute units: {:?}", response.value.units_consumed);
                println!();

                if let Some(logs) = &response.value.logs {
                    println!("  Program logs:");
                    for log in logs {
                        if log.contains("Program log") || log.contains("output=") {
                            println!("    {log}");
                        }
                    }
                }

                println!();
                println!("  Transaction is valid. To execute for real:");
                println!("  → Sign and send to mainnet RPC or Jito bundle");
            }
        }
        Err(e) => {
            println!("  RPC error: {e}");
        }
    }

    println!();
}
