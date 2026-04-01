# Solana Thunder

A Rust DEX aggregator for Solana. Loads all pools across 6 DEX protocols, finds optimal multi-hop swap routes, builds versioned transactions, and provides on-chain token pricing -- all without external APIs.

## Features

- **2M+ pools loaded** across 6 DEXs (Raydium V4, Raydium CLMM, Meteora DAMM V1/V2, Meteora DLMM, Pumpfun AMM)
- **Multi-hop routing** (1-4 hops) with bidirectional neighbor search
- **On-chain SOL/USD pricing** from Raydium CLMM `sqrt_price_x64` -- no external APIs
- **Versioned transactions** (v0) for multi-hop swaps
- **Disk cache** -- first load ~4 min from RPC, subsequent loads ~6s from cache
- **Interactive CLI** with progress bars and REPL
- **Swap simulation** via `simulateTransaction` (never sends)
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

### Run the Aggregator

```bash
# Build
cargo build --release -p thunder-aggregator

# Run (first start loads all pools from RPC, saves cache for next time)
RPC_URL="https://your-rpc-endpoint.com" ./target/release/thunder-agg
```

```
Solana Thunder Aggregator
========================

RPC:   https://your-rpc-endpoint.com
Cache: pools.cache

SOL/USD: $83.17 (on-chain CLMM)

Loading from cache (42s old)...
Loaded 2028823 pools across 1740410 tokens from cache in 6.7s

Thunder Aggregator ready. Type 'help' for commands.

thunder> price SOL
  Price (USD): $83.17

thunder> route SOL EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v 1.0
  Route 1 (2 hops):
    Hop 1: So1111..1112 -> J1toso..GCPn via BSLdeA..pGY2 (Meteora DLMM) | 1.0000 -> 2.4655
    Hop 2: J1toso..GCPn -> EPjFWd..Dt1v via DQ4FYZ..YJix (Meteora DLMM) | 2.4655 -> 330.8569
    Output: 330.8569 | Total impact: 0.00%

thunder> stats
  Pools:
              Meteora DAMM V2:   873940
                  Pumpfun AMM:   829114
                 Raydium CLMM:   169644
                 Meteora DLMM:   139756
              Meteora DAMM V1:    16369
                        TOTAL:  2028823
  Unique tokens: 1740410
  Memory: 5592.4 MB

thunder> exit
```

### Configuration

| Variable | Default | Description |
|---|---|---|
| `RPC_URL` | `https://api.mainnet-beta.solana.com` | Solana RPC endpoint |
| `CACHE_PATH` | `pools.cache` | Pool cache file location |
| `CACHE_MAX_AGE` | `3600` | Max cache age (seconds) before RPC reload. `0` = always reload. |
| `PRIVATE_KEY` | (none) | Base58 keypair for swap simulation (never sent) |

### Examples

```bash
# Test on-chain SOL/USD price (single RPC call)
RPC_URL="https://..." cargo run -p thunder-aggregator --example test_price

# Simulate a swap (builds tx, calls simulateTransaction, NEVER sends)
RPC_URL="https://..." cargo run --release -p thunder-aggregator --example simulate_swap
```

## Using as a Library

The DEX crates are pure -- no RPC, no async, no I/O. Use them directly:

```rust
use thunder_core::{Market, SwapDirection};

// 1. Deserialize pool account data (BorshDeserialize)
let pool: raydium_amm_v4::RaydiumAMMV4 = borsh::from_slice(&account_data)?;

// 2. Construct the market with cached vault balances
let market = raydium_amm_v4::RaydiumAmmV4Market::new(
    pool,
    pool_address.to_string(),
    quote_vault_balance,
    base_vault_balance,
);

// 3. Use the Market trait
let price = market.current_price()?;
let output = market.calculate_output(1_000_000_000, SwapDirection::Buy)?;
let impact = market.calculate_price_impact(1_000_000_000, SwapDirection::Buy)?;

// 4. Build swap instructions (pure, deterministic)
let instructions = market.build_swap_instruction(context, args, SwapDirection::Buy)?;
```

Add to your `Cargo.toml`:

```toml
[dependencies]
solana-thunder = { path = "." }
# Or individual crates:
thunder-core = { path = "crates/core" }
raydium-amm-v4 = { path = "crates/raydium-amm-v4" }
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

thunder-aggregator        Pool loading, routing, pricing, caching, CLI
solana-thunder            Root crate: re-exports all DEX crates
```

Each DEX is an independent crate depending only on `thunder-core`. No DEX crate imports another. Adding a new DEX means creating a new crate -- zero changes to existing code.

### Project Structure

```
solana-thunder/
+-- Cargo.toml                          Workspace root
+-- src/lib.rs                          Re-exports all DEX crates
+-- crates/
|   +-- core/src/                       Market trait, SwapArgs, constants
|   +-- raydium-amm-v4/src/            RaydiumAMMV4 + RaydiumAmmV4Market
|   +-- raydium-clmm/src/              RaydiumCLMMPool + RaydiumClmmMarket
|   +-- meteora-damm/src/              MeteoraDAMMMarket + V2Market + models
|   +-- meteora-dlmm/src/              MeteoraDLMMPool + MeteoraDlmmMarket
|   +-- pumpfun-amm/src/               PumpfunAmmPool + PumpfunAmmMarket
|   +-- aggregator/src/                Aggregator binary + library
|       +-- loader.rs                  RPC pool loading (all DEXs, no mint filter)
|       +-- cache.rs                   Disk cache (bincode, ~1.6 GB for 2M pools)
|       +-- router.rs                  Multi-hop routing (1-4 hops, bidirectional BFS)
|       +-- transaction.rs             Versioned transaction builder (v0)
|       +-- price.rs                   On-chain pricing (CLMM sqrt_price for SOL/USD)
|       +-- pool_index.rs             In-memory token-pair graph
|       +-- cli.rs                     Progress bars + REPL
|       +-- stats.rs                   Pool + system statistics
+-- tests/                             Yellowstone gRPC integration tests
```

## Integration Tests

The test suite demonstrates live Solana streaming via Yellowstone gRPC. Create a `.env` file:

```bash
GEYSER_ENDPOINT="https://your-geyser-endpoint:port"
GEYSER_TOKEN="your-auth-token"           # optional, depends on provider
```

```bash
cargo test --test trade_stream -- --nocapture      # Live swap streaming
cargo test --test creation_stream -- --nocapture   # Token + pool creation streaming
```

## Development

```bash
cargo check                    # Type-check everything
cargo build                    # Build all crates
cargo test                     # Run unit tests (5 tick array tests)
cargo build --release -p thunder-aggregator    # Build aggregator binary
```

## References

- [Raydium AMM](https://github.com/raydium-io/raydium-amm)
- [Raydium CLMM](https://github.com/raydium-io/raydium-clmm)
- [Meteora DAMM V1](https://github.com/MeteoraAg/damm-v1-sdk)
- [Meteora DAMM V2](https://github.com/MeteoraAg/damm-v2)
- [Meteora DLMM](https://github.com/MeteoraAg/dlmm-sdk)
- [Pumpfun Bonding Curve](https://solscan.io/account/6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P)
- [Pumpfun AMM](https://solscan.io/account/pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA)

## License

MIT
