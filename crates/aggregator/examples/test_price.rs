//! Standalone test: fetch SOL/USD price from on-chain CLMM sqrt_price.
//!
//! Run: RPC_URL="https://..." cargo run -p thunder-aggregator --example test_price

use solana_commitment_config::CommitmentConfig;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use thunder_aggregator::price;

#[tokio::main]
async fn main() {
    let rpc_url = std::env::var("RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());

    println!("RPC: {rpc_url}");
    println!("Fetching SOL/USD from on-chain CLMM sqrt_price...\n");

    let rpc = RpcClient::new_with_commitment(rpc_url, CommitmentConfig::confirmed());

    match price::fetch_sol_usd_onchain(&rpc).await {
        Some(price) => println!("SOL/USD: ${price:.4} (on-chain CLMM)"),
        None => {
            eprintln!("Failed to fetch SOL/USD from CLMM pools");
            std::process::exit(1);
        }
    }
}
