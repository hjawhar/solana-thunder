use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use solana_commitment_config::CommitmentConfig;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use tokio::sync::RwLock;

use thunder_aggregator::cache;
use thunder_aggregator::loader::PoolLoader;
use thunder_aggregator::price;

use thunder_engine::account_store::AccountStore;
use thunder_engine::api::{self, AppState};
use thunder_engine::cold_start;
use thunder_engine::pool_registry::PoolRegistry;
use thunder_engine::streaming;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let rpc_url = env::var("RPC_URL").unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".into());
    let cache_path = PathBuf::from(env::var("CACHE_PATH").unwrap_or_else(|_| "pools.cache".into()));
    let port: u16 = env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080);

    println!("Thunder Engine");
    println!("==============");

    // 1. Load pools from cache or RPC.
    let (pool_index, loaded_from_cache) = if cache_path.exists() {
        println!("Loading pools from cache: {}", cache_path.display());
        match cache::load_cache(&cache_path) {
            Ok((index, ts)) => {
                println!(
                    "Loaded {} pools from cache (timestamp {})",
                    index.pool_count(),
                    ts
                );
                (index, true)
            }
            Err(e) => {
                eprintln!("Cache load failed: {e}, falling back to RPC");
                let index = load_from_rpc(&rpc_url).await;
                (index, false)
            }
        }
    } else {
        println!("No cache found at {}, loading from RPC", cache_path.display());
        let index = load_from_rpc(&rpc_url).await;
        (index, false)
    };

    println!("Pool index: {} pools", pool_index.pool_count());

    // 2. Build PoolRegistry from index.
    let mut registry = PoolRegistry::from_pool_index(&pool_index);
    println!(
        "Registry built: {} pools, {} unique mints",
        registry.pool_count(),
        registry.unique_mints()
    );

    // 3. Cold start: fetch vaults, tick arrays, bitmap extensions, validate.
    let store = Arc::new(AccountStore::new());
    let rpc = RpcClient::new_with_timeout_and_commitment(
        rpc_url.clone(),
        std::time::Duration::from_secs(120),
        CommitmentConfig::confirmed(),
    );
    cold_start::cold_start(&rpc, &mut registry, &store).await;

    // 4. Save cache if loaded from RPC (so next start is faster).
    if !loaded_from_cache {
        println!("Saving pool cache to {}", cache_path.display());
        match cache::save_cache(&pool_index, &cache_path) {
            Ok(n) => println!("Saved {n} pools to cache"),
            Err(e) => eprintln!("Cache save failed: {e}"),
        }
    }

    // 5. Fetch SOL/USD price.
    let sol_usd = price::fetch_sol_usd_onchain(&rpc).await;
    match sol_usd {
        Some(p) => println!("SOL/USD price: ${p:.2}"),
        None => eprintln!("Warning: could not fetch SOL/USD price"),
    }

    // 6. Build shared state.
    let state = Arc::new(AppState {
        store: store.clone(),
        pool_index: Arc::new(pool_index),
        registry: Arc::new(RwLock::new(registry)),
        sol_usd_price: RwLock::new(sol_usd),
        start_time: Instant::now(),
    });

    // 7. Start gRPC streaming in background.
    // start_streaming holds a !Send future (Box<dyn Error> without Send bound),
    // so we run it on a dedicated single-threaded runtime.
    let store_bg = store.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build streaming runtime");
        rt.block_on(streaming::start_streaming(store_bg));
    });

    // 8. Start HTTP server.
    let app = api::create_router(state);
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .expect("failed to bind TCP listener");
    println!("Serving on http://0.0.0.0:{port}");
    thunder_engine::axum::serve(listener, app).await.expect("HTTP server error");
}

async fn load_from_rpc(rpc_url: &str) -> thunder_aggregator::pool_index::PoolIndex {
    let loader = PoolLoader::new(rpc_url);
    let cb: thunder_aggregator::loader::ProgressCallback = Box::new(|progress| {
        println!("[loader] {}: {:?}", progress.dex_name, progress.phase);
    });
    loader
        .load_all(&cb)
        .await
        .expect("failed to load pools from RPC")
}
