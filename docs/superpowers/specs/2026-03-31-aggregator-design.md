# Solana Thunder Aggregator - Design Spec

## Goal

Build a Jupiter-like DEX aggregator as a new workspace crate (`crates/aggregator/`) that loads all pools across 6 supported DEXs, finds optimal multi-hop routes between any token pair, builds versioned transactions (v0), provides token pricing, and exposes an interactive CLI with progress bars and statistics.

## Architecture

New workspace member `crates/aggregator/` with both `lib.rs` (library API) and `main.rs` (CLI binary). Existing DEX crates remain untouched. The aggregator treats all pools uniformly via `Box<dyn Market>`.

```
RPC getProgramAccounts --> deserialize per-DEX --> PoolIndex (token graph)
                                                       |
User query (A->B, amount) --> Router (BFS, max 3 hops) --> simulate all paths
                                                       |
                                                  best route
                                                       |
                          TransactionBuilder --> VersionedTransaction (v0)
```

## File Structure

```
crates/aggregator/
  Cargo.toml
  src/
    lib.rs              Public API re-exports
    main.rs             CLI entry point (tokio async main)
    types.rs            Route, RouteHop, Quote, PoolEntry
    pool_index.rs       In-memory token graph (mint adjacency list + pool storage)
    router.rs           Multi-hop route finding (BFS + simulation)
    transaction.rs      Versioned transaction builder (v0 + ALT support)
    price.rs            Token price oracle (SOL/USD via pool graph)
    loader.rs           Async pool loading from RPC (all 6 DEXs)
    stats.rs            Pool and system resource statistics
    cli.rs              Interactive CLI loop with progress bars
```

## Core Types

```rust
pub struct PoolEntry {
    pub market: Box<dyn Market>,
    pub dex_name: String,
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

BFS with depth limit (max 3 hops). Hub-first strategy: for 2+ hop routes, tries WSOL/USDC/USDT as intermediaries before full graph scan. For each candidate path, simulates the full swap chain via `calculate_output` to get actual output amount. Returns routes sorted by output amount descending.

## Transaction Builder

1. For each hop, compute `required_accounts` and `build_swap_instruction`
2. Deduplicate ATA creation instructions across hops
3. For WSOL intermediate hops: create shared temp WSOL account, route through it, close once
4. Wrap all instructions into `VersionedMessage::V0` with address lookup tables
5. Return unsigned `VersionedTransaction`

## Pool Loading

Parallel `getProgramAccounts` calls per DEX with appropriate filters:
- Raydium V4: dataSize=752
- Raydium CLMM: dataSize=1544
- Meteora DAMM V1: dataSize=944 + dataSize=952 (two calls)
- Meteora DAMM V2: dataSize=1112
- Meteora DLMM: dataSize=904
- Pumpfun AMM: discriminator-based filter

After fetching pool accounts, batch-fetch vault balances via `getMultipleAccounts` (100 accounts per batch). Progress reported per DEX via callback.

## Price Oracle

Token price in SOL: find best 1-hop pool pairing token with WSOL, use `current_price()`.
SOL price in USD: find best USDC/WSOL pool, use its price. Fallback: Jupiter Price API v2.
Token price in USD: token_price_sol * sol_price_usd.

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
solana-rpc-client = "3.0"
solana-rpc-client-api = "3.0"
solana-account-decoder-client-types = "3.0"
solana-commitment-config = "3.0"

# CLI
indicatif = "0.17"
clap = { version = "4", features = ["derive"] }
rustyline = "15"

# Stats
sysinfo = "0.34"

# Price API fallback
reqwest = { version = "0.12", features = ["json"] }
serde_json = "1.0"
```
