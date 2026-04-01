# Solana Thunder

A Rust DEX aggregator for Solana. Loads all pools across 6 DEX protocols, finds optimal multi-hop swap routes, builds swap instructions, and executes them on a local Surfpool fork of mainnet -- all without external APIs.

## Features

- **2M+ pools loaded** across 6 DEXs (Raydium V4, Raydium CLMM, Meteora DAMM V1/V2, Meteora DLMM, Pumpfun AMM)
- **Multi-hop routing** (1-4 hops) with bidirectional neighbor search
- **On-chain SOL/USD pricing** from Raydium CLMM `sqrt_price_x64` -- no external APIs
- **Centralized swap instruction builder** for all DEXs with correct Anchor account layouts
- **Surfpool integration** -- execute real swaps against forked mainnet state, zero cost
- **On-chain router program** for CPI-based multi-hop swaps
- **Disk cache** -- first load ~4 min from RPC, subsequent loads ~6s from cache
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

### Run the Aggregator CLI

```bash
cargo build --release -p thunder-aggregator

# First run loads all pools from RPC and saves cache
RPC_URL="https://your-rpc-endpoint.com" ./target/release/thunder-agg

# Subsequent runs load from cache (~6s)
RPC_URL="https://your-rpc-endpoint.com" ./target/release/thunder-agg
```

### Execute Swaps on Surfpool

Surfpool forks mainnet state locally. Swaps execute against real pool data at zero cost.

```bash
# Install Surfpool
curl -sL https://run.surfpool.run/ | bash

# Install Solana CLI (for program deployment)
sh -c "$(curl -sSfL https://release.anza.xyz/stable/install)"

# Start Surfpool with mainnet fork
surfpool start --rpc-url "https://your-rpc-endpoint.com" \
  --no-tui --no-deploy --no-studio \
  --airdrop <YOUR_WALLET> --airdrop-amount 100000000000 &

# Airdrop SOL on local network
solana -u http://127.0.0.1:8899 airdrop 100 <YOUR_WALLET>

# Build and deploy the router program
cd crates/router-program && cargo-build-sbf && cd ../..
solana -u http://127.0.0.1:8899 program deploy \
  crates/router-program/target/deploy/thunder_router.so \
  --program-id crates/router-program/target/deploy/thunder_router-keypair.json

# Run the 2-hop swap test (SOL -> USDC -> TRUMP)
cargo test --release --test surfpool_swap -- --nocapture
```

### Configuration

| Variable | Default | Description |
|---|---|---|
| `RPC_URL` | `https://api.mainnet-beta.solana.com` | Solana RPC endpoint |
| `CACHE_PATH` | `pools.cache` | Pool cache file location |
| `CACHE_MAX_AGE` | `3600` | Max cache age (seconds) before RPC reload |
| `PRIVATE_KEY` | (none) | Base58 keypair (in `.env`, never committed) |
| `SURFPOOL_URL` | `http://127.0.0.1:8899` | Surfpool local RPC |

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

thunder-aggregator        Pool loading, routing, pricing, swap building, caching, CLI
thunder-router            On-chain program for CPI multi-hop swaps (Surfpool)
solana-thunder            Root crate: re-exports all DEX crates
```

### Swap Execution Flow

```
Pool cache / RPC  -->  PoolIndex (2M pools, token graph)
                            |
                            v
                    Router (BFS, 1-4 hops, bidirectional)
                            |
                            v
                    swap_builder (correct Anchor account layouts per DEX)
                            |
                            v
                    Surfpool (forked mainnet) --> real swap execution
```

### Project Structure

```
solana-thunder/
+-- crates/
|   +-- core/                       Market trait, SwapArgs, constants
|   +-- raydium-amm-v4/            RaydiumAMMV4 + RaydiumAmmV4Market
|   +-- raydium-clmm/              RaydiumCLMMPool + RaydiumClmmMarket
|   +-- meteora-damm/              MeteoraDAMMMarket + V2Market + models
|   +-- meteora-dlmm/              MeteoraDLMMPool + MeteoraDlmmMarket
|   +-- pumpfun-amm/               PumpfunAmmPool + PumpfunAmmMarket
|   +-- aggregator/                Aggregator binary + library
|   |   +-- src/
|   |       +-- loader.rs          RPC pool loading (all DEXs, no mint filter)
|   |       +-- cache.rs           Disk cache (bincode, ~1.6 GB for 2M pools)
|   |       +-- router.rs          Multi-hop routing (1-4 hops, bidirectional BFS)
|   |       +-- swap_builder.rs    Centralized swap instructions for all 6 DEXs
|   |       +-- price.rs           On-chain pricing (CLMM sqrt_price for SOL/USD)
|   |       +-- pool_index.rs     In-memory token-pair graph
|   |       +-- cli.rs             Progress bars + REPL
|   |       +-- stats.rs           Pool + system statistics
|   +-- router-program/            On-chain router (CPI multi-hop, Surfpool)
+-- tests/
    +-- surfpool_swap.rs           2-hop swap test on Surfpool (SOL->USDC->TRUMP)
    +-- trade_stream.rs            Live DEX swap streaming via Yellowstone gRPC
    +-- creation_stream.rs         Token + pool creation streaming
```

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
cargo check                    # Type-check everything
cargo build                    # Build all crates
cargo test                     # Unit tests (5 tick array tests)
cargo build --release -p thunder-aggregator    # Build aggregator
cargo test --release --test surfpool_swap -- --nocapture  # Surfpool swap test
```

## References

- [Raydium AMM](https://github.com/raydium-io/raydium-amm)
- [Raydium CLMM](https://github.com/raydium-io/raydium-clmm)
- [Meteora DAMM V1](https://github.com/MeteoraAg/damm-v1-sdk)
- [Meteora DAMM V2](https://github.com/MeteoraAg/damm-v2)
- [Meteora DLMM](https://github.com/MeteoraAg/dlmm-sdk)
- [Pumpfun AMM](https://solscan.io/account/pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA)
- [Surfpool](https://surfpool.run)

## License

MIT
