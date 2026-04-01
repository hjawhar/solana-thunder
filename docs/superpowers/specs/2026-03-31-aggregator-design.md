# Solana Thunder Aggregator - Design Spec

## Goal

Build a Jupiter-like DEX aggregator as a new workspace crate (`crates/aggregator/`) that loads all pools across 6 supported DEXs, finds optimal multi-hop routes between any token pair, builds versioned transactions (v0), provides token pricing, and exposes an interactive CLI with progress bars and statistics.

## Architecture

New workspace member `crates/aggregator/` with both `lib.rs` (library API) and `main.rs` (CLI binary). Existing DEX crates remain untouched. The aggregator treats all pools uniformly via `Box<dyn Market>`.

```
Cache file (pools.cache)                  RPC getProgramAccounts
     |                                         (disc + dataSize filters, no mint filter)
     v                                         |
Load from disk (6s)  -- OR --  Load from RPC (3-4 min) + save cache
     |                                         |
     +------- PoolIndex (token graph) ---------+
                      |
SOL/USD <-- on-chain CLMM sqrt_price
                      |
User query (A->B, amount) --> Router (BFS, 1-4 hops, bidirectional) --> simulate
                                                       |
                          TransactionBuilder --> VersionedTransaction (v0)

## File Structure

```
crates/aggregator/
  Cargo.toml
  src/
    lib.rs              Public API re-exports
    main.rs             CLI entry point (tokio async main)
    types.rs            Route, RouteHop, Quote, PoolEntry (+ cached_data)
    pool_index.rs       In-memory token graph (mint adjacency list + pool storage)
    loader.rs           Async pool loading from RPC (all 6 DEXs, no mint filter)
    cache.rs            Disk cache: save/load pools as bincode binary
    router.rs           Multi-hop route finding (BFS, 1-4 hops, bidirectional)
    transaction.rs      Versioned transaction builder (v0)
    price.rs            SOL/USD on-chain via CLMM sqrt_price, per-token via pools
    stats.rs            Pool and system resource statistics
    cli.rs              Interactive CLI loop with progress bars
```

## Core Types

```rust
pub struct PoolEntry {
    pub market: Box<dyn Market>,
    pub dex_name: String,
    pub cached_data: Vec<u8>,  // bincode of CachedPool for disk cache
}

pub struct RouteHop {
    pub pool_address: String,
    pub dex_name: String,
    pub input_mint: Pubkey,
    pub output_mint: Pubkey,
    pub input_amount: u64,
    pub output_amount: u64,
    pub price_impact_bps: u64,
}

pub struct Route {
    pub hops: Vec<RouteHop>,
    pub input_mint: Pubkey,
    pub output_mint: Pubkey,
    pub input_amount: u64,
    pub output_amount: u64,
    pub price_impact_bps: u64,
}

pub struct Quote {
    pub routes: Vec<Route>,
    pub best_route_index: usize,
}
```

## PoolIndex

Adjacency list graph: `HashMap<Pubkey, Vec<(Pubkey, String)>>` maps each mint to `(other_mint, pool_address)`. Pool data stored as `HashMap<String, PoolEntry>`. Methods: `add_pool`, `remove_pool`, `get_pool`, `pools_for_pair`, `direct_pools`, `pool_count`, `unique_mints`.

## Router

BFS with depth limit (1-4 hops). Bidirectional search: explores neighbors of BOTH input and output mints. Hub-first strategy for 2-hop (WSOL/USDC/USDT/JITOSOL/MSOL). 3-hop via neighbor+hub. 4-hop via input_neighbor->hub->output_neighbor. For each candidate path, simulates the full swap chain via `calculate_output` to get actual output amount. Returns routes sorted by output amount descending.

## Transaction Builder

1. For each hop, compute `required_accounts` and `build_swap_instruction`
2. Deduplicate ATA creation instructions across hops
3. For WSOL intermediate hops: create shared temp WSOL account, route through it, close once
4. Wrap all instructions into `VersionedMessage::V0` with address lookup tables
5. Return unsigned `VersionedTransaction`

## Pool Loading

Fetches ALL pools from all 6 DEXs using `getProgramAccounts` with discriminator + dataSize filters only (no mint filter). Every pool across every token pair is loaded (~2M pools). Vault balances fetched via `getMultipleAccounts` in batches of 100, 20 concurrent. Two-phase fallback if full fetch fails: discover addresses with dataSlice, then batch-fetch full data.

## Pool Cache

Pools saved to `pools.cache` (~1.6 GB) after first load. Subsequent startups load from cache in ~6s vs ~4min from RPC. Format: length-prefixed bincode-serialized CachedPool entries with a version header. Cache age checked on startup; stale caches trigger RPC reload.

## Price Oracle

SOL price in USD: fetched on-chain from the highest-liquidity Raydium CLMM SOL/USDC pool's `sqrt_price_x64`. Single `getProgramAccounts` call with WSOL+USDC memcmp filters, pick the pool with highest liquidity, convert sqrt_price to human-readable USDC-per-SOL.
Token price in SOL: find best 1-hop pool pairing token with WSOL, use `current_price()`.
Token price in USD: token_price_sol * sol_price_usd. No external APIs.

## CLI Interface

Interactive REPL after pool loading completes:
- `price <mint>` - token price in SOL + USD
- `route <from> <to> <amount>` - find and display best routes
- `quote <from> <to> <amount>` - simulate swap, show output
- `stats` - pool counts per DEX, unique tokens, system resources
- `exit` / `quit` - exit

Progress bars via `indicatif` MultiProgress during loading phase.

## Dependencies

```toml
# Library deps
thunder-core = { path = "../core" }
raydium-amm-v4 = { path = "../raydium-amm-v4" }
raydium-clmm = { path = "../raydium-clmm" }
meteora-damm = { path = "../meteora-damm" }
meteora-dlmm = { path = "../meteora-dlmm" }
pumpfun-amm = { path = "../pumpfun-amm" }
solana-sdk = { workspace = true }
solana-pubkey = { workspace = true }
serde = { workspace = true }
borsh = { workspace = true }

# Async + RPC
tokio = { version = "1", features = ["full"] }
futures = "0.3"
solana-rpc-client = "3.0"
solana-rpc-client-api = "3.0"
solana-account-decoder-client-types = "3.0"
solana-commitment-config = "3.0"

# CLI + serialization + stats
indicatif = "0.17"
rustyline = "15"
sysinfo = "0.34"
bincode = "1.3"
```
