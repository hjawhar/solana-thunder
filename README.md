# Solana Thunder

A Rust DEX aggregator for Solana with an on-chain multi-hop router program. Loads all pools across 6 DEX protocols, finds optimal multi-hop swap routes, and executes them through a single compact router instruction that CPI-chains through each DEX -- all without external APIs.

## Features

- **On-chain router program** (`thunder-router`) -- single compact `ExecuteRouteArgs` instruction (~30 bytes for 2-hop), CPI chaining through per-DEX adapters with balance-delta output tracking
- **2M+ pools loaded** across 6 DEXs (Raydium V4, Raydium CLMM, Meteora DAMM V1/V2, Meteora DLMM, Pumpfun AMM)
- **Multi-hop routing** (1-4 hops, default 2) with hub-based and bidirectional neighbor search
- **On-chain SOL/USD pricing** from Raydium CLMM `sqrt_price_x64` -- no external APIs
- **Address Lookup Table support** -- 18 common addresses pre-loaded for transaction size reduction
- **Instant startup** -- serves quotes immediately from cached vault balances, fresh data loads in background
- **Optimized routing** -- pre-resolved mints, cached swappable set (Arc<HashSet>), no allocations in hot path
- **Reserve-capped outputs** -- calculate_output never exceeds pool vault balance
- **Disk cache** -- first load ~4 min from RPC, subsequent loads ~6s from cache
- **SOL/USD price refresh** -- on-chain price from CLMM sqrt_price, refreshed every 15s
- **Interactive CLI** with progress bars and REPL
- **Pure DEX library** -- each DEX crate has zero I/O, usable independently

## Supported DEXs

| DEX | Crate | Pricing Model | Pools |
|-----|-------|---------------|-------|
| Raydium AMM V4 | `raydium-amm-v4` | Constant product (x*y=k) | ~50K* |
| Raydium CLMM | `raydium-clmm` | Concentrated liquidity (Q64.64 sqrt_price) | ~170K |
| Meteora DAMM V1 | `meteora-damm` | Constant product + stable curves | ~16K |
| Meteora DAMM V2 | `meteora-damm` | sqrt_price based | ~874K |
| Meteora DLMM | `meteora-dlmm` | Dynamic liquidity bins | ~140K |
| Pumpfun AMM | `pumpfun-amm` | Bonding curve (virtual reserves) | ~829K |

*Raydium V4 requires an RPC with secondary index support.

## Quick Start

### Run the Engine (Jupiter-like API)

The engine is a persistent service that keeps all pool data in memory and serves an HTTP API. Swap transactions target the on-chain router program.

```bash
# Start the engine (loads pools, fetches accounts, starts API on port 8080)
RPC_URL="https://your-rpc-endpoint.com" cargo run --release --bin thunder-engine

# In another terminal:
curl "http://localhost:8080/health"
curl "http://localhost:8080/price?mint=SOL"
curl "http://localhost:8080/quote?inputMint=SOL&outputMint=6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN&amount=100000000&maxHops=2"
```

### Run the Aggregator CLI

```bash
cargo build --release -p thunder-aggregator

# First run loads all pools from RPC and saves cache
RPC_URL="https://your-rpc-endpoint.com" ./target/release/thunder-agg

# Subsequent runs load from cache (~6s)
RPC_URL="https://your-rpc-endpoint.com" ./target/release/thunder-agg
```

### Run the Surfpool Swap Test

Surfpool is a local mainnet fork. The swap test deploys the router program, creates ALTs, and executes multi-hop swaps end-to-end.

```bash
# Build the router BPF program
cd crates/router-program && cargo build-sbf && cd ../..

# Run the swap test against a local Surfpool instance
INPUT=SOL OUTPUT=6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN \
AMOUNT=0.1 MAX_HOPS=2 \
cargo test --release --test surfpool_swap -- --nocapture
```

## Architecture

```
thunder-core              Market trait, shared types, constants
    ^
    |
    +-- raydium-amm-v4    Constant product AMM
    +-- raydium-clmm      Concentrated liquidity
    +-- meteora-damm      Dynamic AMM V1 + V2
    +-- meteora-dlmm      Dynamic liquidity bins
    +-- pumpfun-amm       Bonding curve

thunder-aggregator        Pool loading, routing, pricing, account layout helpers, caching, CLI
thunder-engine            Persistent service: AccountStore + gRPC streaming + HTTP API + swap tx builder
thunder-router            On-chain BPF program: compact CPI router (excluded from workspace, solana-program 2.2)
solana-thunder            Root crate: re-exports all DEX crates
```

### Swap Execution Flow

```
Client  --->  Engine HTTP API  (GET /quote, POST /swap)
                    |
                    v
              Router (BFS multi-hop route finding)
                    |
                    v
              build_swap_transaction()
                    |
                    +--- ATA creation (idempotent) for intermediate + output mints
                    +--- WSOL wrap (if input is SOL)
                    +--- Single compact ExecuteRouteArgs instruction
                    |      - Borsh-serialized: amount_in, min_amount_out, Vec<SwapHop>
                    |      - All per-DEX accounts flattened into one account list
                    +--- WSOL unwrap (if input was SOL)
                    |
                    v
              On-chain thunder-router program
                    |
                    +--- For each hop:
                    |      1. Read destination token balance (before)
                    |      2. CPI into DEX program via adapter
                    |      3. Read destination token balance (after)
                    |      4. Chain delta as next hop's input amount
                    |
                    +--- Slippage check on final output
```

### Engine Startup

```
1. Load cache              ~6s   pools.cache -> PoolIndex
2. validate_from_cache     instant  cached vault balances -> swappable set
3. Start HTTP server       instant  /quote works immediately
4. gRPC streaming          background  live account updates + vault re-validation
5. Vault fetch             background  4M+ accounts, 100 concurrent batches
6. SOL/USD price refresh   background  every 15s
```

### Engine Service Flow

```
Yellowstone gRPC  --->  AccountStore (DashMap, implements AccountDataProvider)
                              |
                              +--- on_vault_update (Token + Token-2022)
                              |
RPC cold start  ------------>|  vaults, tick arrays, bitmap extensions
                              v
                        PoolRegistry (2M pools, cached Arc<HashSet> swappable set)
                              |
                              v
                    Router (live data via AccountDataProvider, pre-resolved mints)
                              |
                              v
                    HTTP API:  GET /quote  (?maxHops=2&slippageBps=50)
                               POST /swap  (builds router instruction tx)
                               GET /price
                               GET /health
```

### Project Structure

```
solana-thunder/
+-- bin/
|   +-- engine.rs                     Engine binary entry point
+-- crates/
|   +-- core/                         Market trait, AccountDataProvider, calculate_output_live
|   +-- raydium-amm-v4/               RaydiumAMMV4 + RaydiumAmmV4Market
|   +-- raydium-clmm/                 RaydiumCLMMPool + RaydiumClmmMarket
|   +-- meteora-damm/                 MeteoraDAMMMarket + V2Market + models
|   +-- meteora-dlmm/                 MeteoraDLMMPool + MeteoraDlmmMarket
|   +-- pumpfun-amm/                  PumpfunAmmPool + PumpfunAmmMarket
|   +-- aggregator/                   Pool loading, routing, pricing, account layout helpers, caching, CLI
|   |   +-- src/
|   |       +-- loader.rs             RPC pool loading (all DEXs, no mint filter)
|   |       +-- cache.rs              Disk cache + PDA extraction + make_entry helper
|   |       +-- router.rs             Multi-hop routing (live data, pre-resolved mints)
|   |       +-- swap_builder.rs       Per-DEX account layout helpers (used by engine swap.rs)
|   |       +-- price.rs              On-chain pricing (CLMM sqrt_price)
|   |       +-- pool_index.rs         In-memory token-pair graph
|   |       +-- cli.rs                Progress bars + REPL
|   +-- engine/                       Persistent service (library)
|   |   +-- src/
|   |       +-- account_store.rs      DashMap store, implements AccountDataProvider
|   |       +-- pool_registry.rs      Swappable validation (cached Arc<HashSet>, vault-to-pool index)
|   |       +-- cold_start.rs         Background vault fetch (100 concurrent), tick arrays, bin arrays
|   |       +-- streaming.rs          Yellowstone gRPC: live updates + vault re-validation (Token + Token-2022)
|   |       +-- api.rs                Axum HTTP: /quote, /swap, /price, /health
|   |       +-- swap.rs               Router instruction builder: compact ExecuteRouteArgs + per-DEX account collection
|   +-- router-program/               On-chain BPF program (excluded from workspace, solana-program 2.2)
|       +-- src/
|           +-- lib.rs                Entrypoint, ExecuteRouteArgs, hop iteration, slippage check
|           +-- adapters/
|               +-- common.rs         read_token_balance, swap_authority PDA
|               +-- meteora_damm_v1.rs
|               +-- meteora_damm_v2.rs
|               +-- meteora_dlmm.rs
|               +-- raydium_clmm.rs
|               +-- raydium_v4.rs
|               +-- pumpfun.rs        buy + sell variants
+-- tests/
    +-- surfpool_swap.rs              End-to-end: deploy router, create ALT, execute multi-hop swaps on Surfpool
    +-- simulate_swap.rs              Swap simulation tests
    +-- trade_stream.rs               Live DEX swap streaming (gRPC)
    +-- creation_stream.rs            Token + pool creation streaming
    +-- pool_financials.rs            Pool financial analysis
    +-- validate_prices.rs            Price validation tests
    +-- helpers/
        +-- mod.rs                    Shared test utilities
```

## Router Program

The `thunder-router` crate (`crates/router-program/`) is an on-chain Solana BPF program that executes multi-hop swaps atomically. It is excluded from the workspace because it depends on `solana-program 2.2` while the rest of the workspace uses `solana-sdk 3.0`.

**Program ID:** `7WgM9BLWicvmxZwNsT5AUKqxsf6QqBSy2RxeEEwjzJFu`

### Instruction Format

A single `ExecuteRouteArgs` instruction (Borsh-serialized):

```rust
struct ExecuteRouteArgs {
    amount_in: u64,          // Input lamports/tokens
    min_amount_out: u64,     // Slippage floor
    hops: Vec<SwapHop>,      // One per DEX hop
}

struct SwapHop {
    dex_type: DexType,       // Which adapter to invoke
    num_accounts: u8,        // How many accounts this hop consumes
}
```

For a 2-hop swap this is ~30 bytes of instruction data. All per-DEX accounts are flattened into a single account list; each hop slices its portion using `num_accounts`.

### DexType Enum

| Variant | Discriminant | Adapter |
|---------|-------------|---------|
| MeteoraDAMMV1 | 0 | `adapters::meteora_damm_v1` |
| MeteoraDAMMV2 | 1 | `adapters::meteora_damm_v2` |
| MeteoraDLMM | 2 | `adapters::meteora_dlmm` |
| RaydiumCLMM | 3 | `adapters::raydium_clmm` |
| RaydiumAMMV4 | 4 | `adapters::raydium_v4` |
| PumpfunBuy | 5 | `adapters::pumpfun::buy` |
| PumpfunSell | 6 | `adapters::pumpfun::sell` |

### Adapter Pattern

All 7 adapters follow a uniform account prefix (OKX-pattern):

| Index | Account | Purpose |
|-------|---------|---------|
| 0 | `dex_program` | Target DEX program (read-only) |
| 1 | `swap_authority` | User or PDA signer |
| 2 | `swap_source_token` | User's input token account |
| 3 | `swap_destination_token` | User's output token account |

Remaining accounts (index 4+) are DEX-specific. Account counts per DEX:

| DEX | Total Accounts |
|-----|---------------|
| Meteora DAMM V1 | 16 |
| Meteora DAMM V2 | 13 |
| Meteora DLMM | 19 |
| Raydium CLMM | 18 |
| Raydium AMM V4 | 19 |
| Pumpfun (buy/sell) | 13 |

### Balance-Delta Chaining

The router does not trust reported output amounts from DEX programs. Instead, it reads the destination token account balance before and after each CPI, computes the actual delta, and passes that as the input amount for the next hop. Slippage is checked once on the final output.

### Engine Integration

The engine (`crates/engine/src/swap.rs`) duplicates `DexType`, `SwapHop`, and `ExecuteRouteArgs` as Borsh types to avoid the `solana-program 2.2` / `solana-sdk 3.0` dependency conflict. It has ~12 program/authority constants and per-DEX `collect_*_accounts` functions that assemble the correct account list for each adapter.

## Configuration

| Variable | Default | Description |
|---|---|---|
| `RPC_URL` | `https://api.mainnet-beta.solana.com` | Solana RPC endpoint |
| `CACHE_PATH` | `pools.cache` | Pool cache file location |
| `CACHE_MAX_AGE` | `3600` | Max cache age (seconds) before RPC reload |
| `PRIVATE_KEY` | (none) | Base58 keypair (in `.env`, never committed) |
| `GEYSER_ENDPOINT` | (none) | Yellowstone gRPC endpoint for live streaming |
| `GEYSER_TOKEN` | (none) | Yellowstone gRPC auth token |
| `PORT` | `8080` | Thunder Engine HTTP API port |

## Using as a Library

The DEX crates are pure -- no RPC, no async, no I/O:

```rust
use thunder_core::{Market, SwapDirection};

let pool: raydium_amm_v4::RaydiumAMMV4 = borsh::from_slice(&account_data)?;
let market = raydium_amm_v4::RaydiumAmmV4Market::new(pool, address, quote_bal, base_bal);

let price = market.current_price()?;
let output = market.calculate_output(1_000_000_000, SwapDirection::Buy)?;
```

## Development

```bash
cargo check                    # Type-check workspace
cargo build                    # Build all workspace crates
cargo test                     # Unit tests

cargo build --release -p thunder-aggregator    # Build aggregator CLI
cargo build --release -p thunder-engine        # Build engine

# Router program (excluded from workspace, requires solana-program 2.2)
cd crates/router-program && cargo build-sbf    # Build BPF program

# Surfpool swap test (requires running Surfpool instance)
INPUT=SOL OUTPUT=<mint> AMOUNT=0.1 MAX_HOPS=2 \
cargo test --release --test surfpool_swap -- --nocapture
```

## References

- [Raydium AMM](https://github.com/raydium-io/raydium-amm)
- [Raydium CLMM](https://github.com/raydium-io/raydium-clmm)
- [Meteora DAMM V1](https://github.com/MeteoraAg/damm-v1-sdk)
- [Meteora DAMM V2](https://github.com/MeteoraAg/damm-v2)
- [Meteora DLMM](https://github.com/MeteoraAg/dlmm-sdk)
- [Pumpfun AMM](https://solscan.io/account/pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA)
- [OKX DEX Router](https://www.okx.com/web3/build/docs/waas/dex-swap) -- uniform account prefix pattern inspiration

## License

MIT
