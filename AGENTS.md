# Repository Guidelines

## Project Overview

Solana Thunder is a Rust DEX aggregator for Solana. 6 pure DEX crates parse on-chain account data and compute swap outputs through a unified `Market` trait. An aggregator crate loads all pools from RPC, finds multi-hop routes, and builds swap instructions via a centralized `swap_builder`. Swaps execute on Surfpool (local mainnet fork) through an on-chain router program. No external APIs -- all data is on-chain.

## Architecture

```
thunder-core          Market trait, shared types, constants
    ^
    |
    +-- raydium-amm-v4    Constant product AMM
    +-- raydium-clmm      Concentrated liquidity (Q64.64 sqrt_price)
    +-- meteora-damm      Dynamic AMM V1 (constant product + stable) and V2 (sqrt_price)
    +-- meteora-dlmm      Dynamic liquidity bins
    +-- pumpfun-amm       Bonding curve (virtual reserves)

thunder-aggregator    Pool loading, routing, pricing, swap building, caching, CLI
thunder-router        On-chain program for CPI multi-hop swaps (deployed on Surfpool)
solana-thunder        Root crate: re-exports all DEX crates
```

No DEX crate imports another DEX crate.

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

### Data Flow (pure DEX crates)

```
Raw account bytes --BorshDeserialize--> Pool model struct
                                            |
                                            v
                                     DexMarket::new(pool, address, balances)
                                            |
                                            v
                                     impl Market { ... }
                                       +-- calculate_output()
                                       +-- calculate_price_impact()
                                       +-- current_price()
                                       +-- is_active()
```

## Key Directories

```
solana-thunder/
+-- Cargo.toml                          # Workspace root
+-- src/lib.rs                          # Root crate: re-exports all DEX crates
+-- crates/
|   +-- core/src/
|   |   +-- traits.rs                   # Market trait, SwapArgs, SwapContext, is_active()
|   |   +-- constants.rs                # WSOL, USDC, USDT, quote_priority, infer_mint_decimals
|   +-- raydium-amm-v4/src/lib.rs       # RaydiumAMMV4 + RaydiumAmmV4Market
|   +-- raydium-clmm/src/
|   |   +-- lib.rs                      # RaydiumCLMMPool + RaydiumClmmMarket
|   |   +-- tick_arrays.rs              # Tick array bitmap computation + tests
|   +-- meteora-damm/src/
|   |   +-- lib.rs                      # V1 MeteoraDAMMMarket + V2 MeteoraDAMMV2Market
|   |   +-- models.rs                   # Pool models for V1, V2
|   |   +-- utils.rs                    # PDA derivation (vault, LP mint)
|   +-- meteora-dlmm/src/lib.rs         # MeteoraDLMMPool + MeteoraDlmmMarket
|   +-- pumpfun-amm/src/
|   |   +-- lib.rs                      # PumpfunAmmPool + PumpfunAmmMarket
|   |   +-- pda.rs                      # 10 PDA derivation functions
|   +-- aggregator/src/
|   |   +-- loader.rs                   # Async RPC pool loading (all DEXs, no mint filter)
|   |   +-- cache.rs                    # Disk cache (bincode, ~1.6 GB for 2M pools)
|   |   +-- router.rs                   # Multi-hop route finding (1-4 hops, bidirectional BFS)
|   |   +-- swap_builder.rs            # Centralized swap instructions for all 6 DEXs
|   |   +-- price.rs                    # SOL/USD on-chain via CLMM sqrt_price
|   |   +-- pool_index.rs             # In-memory token-pair graph
|   |   +-- cli.rs                      # Progress bars + interactive REPL
|   |   +-- stats.rs                    # Pool and system statistics
|   |   +-- types.rs                    # PoolEntry, Route, RouteHop, Quote, etc.
|   |   +-- main.rs                     # CLI binary (thunder-agg)
|   +-- router-program/                # On-chain router (excluded from workspace)
|       +-- src/lib.rs                  # CPI multi-hop swap program
+-- tests/
    +-- surfpool_swap.rs                # 2-hop swap test on Surfpool (SOL->USDC->TRUMP)
    +-- trade_stream.rs                 # Live DEX swap streaming via Yellowstone gRPC
    +-- creation_stream.rs              # Live token + pool creation streaming
```

## Development Commands

```bash
cargo check                        # Type-check all crates
cargo build                        # Build all crates
cargo test                         # Run unit tests
cargo build --release -p thunder-aggregator  # Build aggregator binary

# Run aggregator CLI
RPC_URL="https://..." cargo run --release -p thunder-aggregator

# Run Surfpool swap test
cargo test --release --test surfpool_swap -- --nocapture
```

### Surfpool Setup

```bash
# Install
curl -sL https://run.surfpool.run/ | bash
sh -c "$(curl -sSfL https://release.anza.xyz/stable/install)"

# Start with mainnet fork
surfpool start --rpc-url "https://..." --no-tui --no-deploy --no-studio \
  --airdrop <WALLET> --airdrop-amount 100000000000 &

# Deploy router program
cd crates/router-program && cargo-build-sbf && cd ../..
solana -u http://127.0.0.1:8899 program deploy \
  crates/router-program/target/deploy/thunder_router.so \
  --program-id crates/router-program/target/deploy/thunder_router-keypair.json

# Give wallet USDC via cheatcode
# surfnet_setAccount with hex-encoded SPL token account data
```

### Environment Variables

| Variable | Default | Description |
|---|---|---|
| `RPC_URL` | `https://api.mainnet-beta.solana.com` | Solana RPC endpoint |
| `CACHE_PATH` | `pools.cache` | Pool cache file location |
| `CACHE_MAX_AGE` | `3600` | Max cache age (seconds) before RPC reload |
| `PRIVATE_KEY` | (none) | Base58 keypair in `.env` (never committed) |
| `SURFPOOL_URL` | `http://127.0.0.1:8899` | Surfpool local RPC |

### Pool Loading

Fetches ALL pools from all 6 DEXs using `getProgramAccounts` with discriminator + dataSize filters only (no mint filter). ~2M pools total. Vault balances fetched via `getMultipleAccounts` in batches of 100, 20 concurrent.

### Pool Cache

Saved to `pools.cache` (~1.6 GB) after first load. Subsequent startups load in ~6s vs ~4min from RPC. Uses bincode-serialized `CachedPool` enum with per-DEX pool variants.

### REPL Commands

```
thunder> price SOL                    # SOL price in USD
thunder> price <mint>                 # Token price in SOL + USD
thunder> route SOL <mint> 1.0         # Find best routes
thunder> stats                        # Pool counts, memory, uptime
thunder> exit
```

## Code Conventions

### Market Trait

```rust
pub trait Market: Send + Sync {
    fn is_active(&self) -> bool { true }
    fn metadata(&self) -> Result<PoolMetadata, GenericError>;
    fn financials(&self) -> Result<PoolFinancials, GenericError>;
    fn calculate_output(&self, amount_in: u64, direction: SwapDirection) -> Result<u64, GenericError>;
    fn calculate_price_impact(&self, amount_in: u64, direction: SwapDirection) -> Result<u64, GenericError>;
    fn current_price(&self) -> Result<f64, GenericError>;
    // ... default convenience methods
}
```

### Swap Builder

The centralized `swap_builder.rs` builds raw swap instructions with correct Anchor account layouts for each DEX. It does NOT handle WSOL wrapping or ATA creation -- those are separate pre-instructions.

```rust
// DLMM swap
let ix = swap_builder::build_dlmm_swap(&DlmmSwapAccounts { ... }, amount, min_out)?;

// DAMM V1 swap
let ix = swap_builder::build_damm_v1_swap(&DammV1SwapAccounts { ... }, amount, min_out)?;

// Raydium CLMM swap
let ix = swap_builder::build_clmm_swap(&ClmmSwapAccounts { ... }, amount, min_out, sqrt_price_limit)?;
```

### Error Handling

`type GenericError = Box<dyn Error + Send + Sync>` -- string errors via `.into()`. No `thiserror` or `anyhow`.

### Constants

- DEX-specific program IDs in each DEX crate
- Shared constants (WSOL, USDC, USDT, TOKEN_PROGRAM, etc.) in `thunder_core`
- Quote currency ordering: `thunder_core::quote_priority()`
- Token decimal inference: `thunder_core::infer_mint_decimals()`

### Pool Discovery Filters

| DEX | Program ID | data_size | Anchor Discriminator |
|---|---|---|---|
| Raydium V4 | `675kPX...` | 752 | None |
| Raydium CLMM | `CAMMC...` | 1544 | `[247,237,227,245,215,195,222,70]` |
| Meteora DAMM V1 | `Eo7Wj...` | 944 | `[241,154,109,4,17,177,109,188]` |
| Meteora DAMM V2 | `cpamd...` | 1112 | `[241,154,109,4,17,177,109,188]` |
| Meteora DLMM | `LBUZKh...` | 904 | `[33,11,49,98,181,101,177,13]` |
| Pumpfun AMM | `pAMMB...` | N/A | `[241,154,109,4,17,177,109,188]` |

## Important Files

| File | What it is |
|---|---|
| `crates/aggregator/src/swap_builder.rs` | Centralized swap instructions for all 6 DEXs |
| `crates/aggregator/src/loader.rs` | Async RPC pool loading (discriminator + dataSize filters) |
| `crates/aggregator/src/cache.rs` | Disk cache: save/load 2M pools as bincode (~1.6 GB) |
| `crates/aggregator/src/router.rs` | Multi-hop route finding (1-4 hops, bidirectional) |
| `crates/aggregator/src/price.rs` | SOL/USD on-chain via CLMM sqrt_price |
| `crates/core/src/traits.rs` | Market trait, SwapArgs, is_active() |
| `crates/router-program/src/lib.rs` | On-chain CPI router program |
| `tests/surfpool_swap.rs` | 2-hop swap test on Surfpool |

## Runtime / Tooling

- **Rust edition:** 2024 (requires rustc 1.85+)
- **Router program:** edition 2021, `solana-program` 2.2 (excluded from workspace)
- **Workspace resolver:** 3
- **7 workspace dependencies:** `serde`, `solana-sdk`, `solana-pubkey`, `solana-system-interface`, `spl-associated-token-account`, `spl-token`, `borsh`
- **Aggregator dependencies:** `tokio`, `futures`, `solana-rpc-client`, `indicatif`, `rustyline`, `sysinfo`, `bincode`
- **Surfpool:** local mainnet fork, `surfnet_setAccount` cheatcode for token balances
