use std::env;

use thunder_aggregator::{cli, loader, price, stats};

#[tokio::main]
async fn main() {
    println!("Solana Thunder Aggregator");
    println!("========================\n");

    let rpc_url =
        env::var("RPC_URL").unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());

    println!("RPC: {rpc_url}\n");

    // Progress bars while loading pools from all DEXes.
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

    // Fetch SOL/USD price from Jupiter API (more reliable than pool-derived).
    let sol_usd_price = match price::fetch_sol_usd_price_api().await {
        Some(p) => {
            println!("SOL/USD: ${p:.2} (Jupiter API)\n");
            Some(p)
        }
        None => {
            let p = price::get_sol_usd_price(&index);
            if let Some(p) = p {
                println!("SOL/USD: ${p:.2} (pool-derived)\n");
            } else {
                println!("Warning: Could not determine SOL/USD price\n");
            }
            p
        }
    };

    // Interactive command loop.
    let mut stats_collector = stats::StatsCollector::new();
    cli::run_repl(&index, &mut stats_collector, sol_usd_price).await;
}
