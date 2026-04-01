use std::env;

use thunder_aggregator::{cli, loader, stats};

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

    // Interactive command loop.
    let mut stats_collector = stats::StatsCollector::new();
    cli::run_repl(&index, &mut stats_collector).await;
}
