use std::env;

use solana_commitment_config::CommitmentConfig;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use thunder_aggregator::{cli, loader, price, stats};

#[tokio::main]
async fn main() {
    println!("Solana Thunder Aggregator");
    println!("========================\n");

    let rpc_url =
        env::var("RPC_URL").unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());

    println!("RPC: {rpc_url}\n");

    // Fetch SOL/USD from on-chain CLMM sqrt_price before loading pools.
    let rpc = RpcClient::new_with_commitment(rpc_url.clone(), CommitmentConfig::confirmed());
    let sol_usd_price = match price::fetch_sol_usd_onchain(&rpc).await {
        Some(p) => {
            println!("SOL/USD: ${p:.2} (on-chain CLMM)\n");
            Some(p)
        }
        None => {
            println!("Warning: Could not fetch SOL/USD from CLMM, will derive from pools\n");
            None
        }
    };

    // Load all pools with progress bars.
    let display = cli::LoadingDisplay::new();
    let progress_cb = display.progress_callback();

    let loader = loader::PoolLoader::new(&rpc_url);
    let index = match loader.load_all(&progress_cb).await {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("Fatal: Failed to load pools: {e}");
            std::process::exit(1);
        }
    };

    println!(
        "\nLoaded {} pools across {} tokens\n",
        index.pool_count(),
        index.unique_mints()
    );

    // Fall back to pool-derived price if CLMM fetch failed.
    let sol_usd_price = sol_usd_price.or_else(|| {
        let p = price::get_sol_usd_price(&index);
        if let Some(p) = p {
            println!("SOL/USD: ${p:.2} (pool-derived)\n");
        }
        p
    });

    // Interactive command loop.
    let mut stats_collector = stats::StatsCollector::new();
    cli::run_repl(&index, &mut stats_collector, sol_usd_price).await;
}
