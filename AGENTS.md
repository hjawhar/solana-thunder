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
+-- bin/
|   +-- engine.rs                       # Engine binary: immediate serve, background cold start
+-- Cargo.toml                          # Workspace root
+-- src/lib.rs                          # Root crate: re-exports all DEX crates
+-- crates/
|   +-- core/src/
|   |   +-- traits.rs                   # Market trait, PoolMetadata, PoolFinancials, is_active()
|   |   +-- constants.rs                # WSOL, USDC, USDT, quote_priority, infer_mint_decimals
|   +-- raydium-amm-v4/src/lib.rs       # RaydiumAMMV4 + RaydiumAmmV4Market
|   +-- raydium-clmm/src/
|   |   +-- lib.rs                      # RaydiumCLMMPool + RaydiumClmmMarket
|   |   +-- tick_arrays.rs              # Tick array bitmap + PDA derivation (derive_pool_tick_array_pdas)
|   +-- meteora-damm/src/
|   |   +-- lib.rs                      # V1 MeteoraDAMMMarket + V2 MeteoraDAMMV2Market
|   |   +-- models.rs                   # Pool models for V1, V2
|   |   +-- utils.rs                    # PDA derivation (vault, LP mint)
|   +-- meteora-dlmm/src/lib.rs         # MeteoraDLMMPool + MeteoraDlmmMarket
|   +-- pumpfun-amm/src/
|   |   +-- lib.rs                      # PumpfunAmmPool + PumpfunAmmMarket
|   |   +-- pda.rs                      # 10 PDA derivation functions
|   +-- aggregator/src/                 # Pool loading, routing, swap building, caching, CLI
|   |   +-- loader.rs                   # Async RPC pool loading (all DEXs, no mint filter)
|   |   +-- cache.rs                    # Disk cache + PDA extraction (extract_clmm_tick_pdas, extract_dlmm_bin_pda)
|   |   +-- router.rs                   # Multi-hop routing (1-4 hops, swappable filter)
|   |   +-- swap_builder.rs             # Swap instructions for all 6 DEXs
|   |   +-- price.rs                    # SOL/USD on-chain via CLMM sqrt_price
|   |   +-- pool_index.rs               # In-memory token-pair graph
|   |   +-- cli.rs                      # Progress bars + interactive REPL
|   |   +-- main.rs                     # CLI binary (thunder-agg)
|   +-- engine/src/                     # Persistent service (library only, binary in bin/)
|   |   +-- account_store.rs            # DashMap store for all account data
|   |   +-- pool_registry.rs            # Pool index + swappable validation (is_active + vaults_funded, validate_from_cache)
|   |   +-- cold_start.rs               # Background: vault fetch (100 concurrent), tick arrays, bin arrays, bitmap exts
|   |   +-- streaming.rs                # Yellowstone gRPC subscriber
|   |   +-- api.rs                      # Axum HTTP: /quote, /swap, /price, /health
|   +-- router-program/                 # On-chain CPI router (excluded from workspace)
|       +-- src/lib.rs                   # CPI multi-hop swap program
+-- tests/
    +-- surfpool_swap.rs                # Dynamic multi-hop swap on Surfpool (any token pair)
    +-- trade_stream.rs                 # Live DEX swap streaming via Yellowstone gRPC
    +-- creation_stream.rs              # Live token + pool creation streaming
    +-- pool_financials.rs              # Live pool update streaming
    +-- validate_prices.rs              # Price validation across DEXs
```

## Development Commands

```bash
cargo check                        # Type-check all crates
cargo build                        # Build all crates
cargo test                         # Run unit tests
cargo build --release --bin thunder-engine  # Build engine binary
cargo build --release -p thunder-aggregator  # Build aggregator CLI

# Run engine (persistent service with HTTP API)
RPC_URL="https://..." cargo run --release --bin thunder-engine

# Run aggregator CLI (interactive REPL)
RPC_URL="https://..." cargo run --release -p thunder-aggregator

# Engine API
curl http://localhost:8080/health
curl "http://localhost:8080/quote?inputMint=SOL&outputMint=6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN&amount=100000000"
curl "http://localhost:8080/price?mint=SOL"

# Surfpool swap test
INPUT=SOL OUTPUT=6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN AMOUNT=0.1 MAX_HOPS=2 \
  cargo test --release --test surfpool_swap -- --nocapture
```

### Swap Examples

```bash
# SOL -> TRUMP
INPUT=SOL OUTPUT=6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN AMOUNT=0.1 MAX_HOPS=2 \
  cargo test --release --test surfpool_swap -- --nocapture

# SOL -> BONK
INPUT=SOL OUTPUT=DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263 AMOUNT=0.01 MAX_HOPS=2 \
  cargo test --release --test surfpool_swap -- --nocapture

# SOL -> JUP
INPUT=SOL OUTPUT=JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN AMOUNT=0.01 MAX_HOPS=2 \
  cargo test --release --test surfpool_swap -- --nocapture

# SOL -> WIF
INPUT=SOL OUTPUT=EKpQGSJtjMFqKZ9KQanSqYXRcF8fBopzLHYxdM65zcjm AMOUNT=0.01 MAX_HOPS=2 \
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

Fetches ALL pools from all 6 DEXs using `getProgramAccounts` with discriminator + dataSize filters only (no mint filter). ~2M pools total. Vault balances cached per-pool during loading (used for instant startup validation).

### Engine Startup

The engine starts serving HTTP immediately after cache load (~6s). No blocking vault fetch.

```
1. Load cache              ~6s   pools.cache -> PoolIndex
2. validate_from_cache     instant  uses market.financials() (cached vault balances)
3. Start HTTP server       instant  /quote works immediately
4. gRPC streaming          background  live account updates
5. Vault fetch             background  4M+ accounts, 100 concurrent batches
6. SOL/USD price refresh   background  every 15s
```

`validate_from_cache()` uses the vault balances already embedded in the deserialized market objects. No RPC calls. Once the background vault fetch completes, `validate_all(store)` re-validates with fresh on-chain data.

### Cold Start Auxiliary Fetches

After vault loading completes in the background, the engine fetches auxiliary accounts needed for swap instruction building (not for quoting):

1. **DLMM bitmap extensions** -- single GPA for `dataSize=12488` accounts
2. **CLMM tick arrays** -- PDA-derived from each pool's `tick_array_bitmap`, fetched via `getMultipleAccounts` (top 10k pools by vault balance, ~6 tick arrays each)
3. **DLMM bin arrays** -- PDA-derived from each pool's `active_id`, fetched via `getMultipleAccounts`

### Swappable Validation

Route discovery only gates on pool status and liquidity -- NOT auxiliary accounts:

| DEX | Swappable when |
|---|---|
| Pumpfun AMM | always (no status field) |
| Raydium AMM V4 | vaults funded |
| All others | `is_active()` AND vaults funded |

`calculate_output()` uses pool struct fields (sqrt_price, active_id, tick_current) which are in the deserialized pool data. Tick arrays, bin arrays, and bitmap extensions are only needed for building on-chain swap instructions, not for computing quotes.

### Pool Cache

Saved to `pools.cache` (~1.6 GB) after first load. Subsequent startups load in ~6s vs ~4min from RPC. Uses bincode-serialized `CachedPool` enum with per-DEX pool variants. `CachedPool` also serves as the source for PDA extraction (`extract_clmm_tick_pdas`, `extract_dlmm_bin_pda`) during cold start.

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

### Pool Status & `is_active()`

Each DEX has its own on-chain status semantics. `is_active()` overrides check the raw status field:

| DEX | Status field | Active condition | Source |
|---|---|---|---|
| Raydium V4 | `status: u64` | `status == 6` | Raydium AMM status enum |
| Raydium CLMM | `status: u8` (bitfield) | `status & (1 << 4) == 0` | Bit 4 = disable swap |
| Meteora DAMM V1 | `enabled: bool` | `enabled == true` | Direct boolean |
| Meteora DAMM V2 | `pool_status: u8` | `pool_status == 0` | `PoolStatus::Enable = 0` |
| Meteora DLMM | `status: u8` | `status == 0` | `PairStatus::Enabled = 0` |
| Pumpfun AMM | (none) | default `true` | No on-chain status field |

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
| `bin/engine.rs` | Engine binary: instant startup, background vault fetch, 15s SOL/USD refresh |
| `crates/engine/src/account_store.rs` | DashMap store for all account data (pools, vaults, tick arrays) |
| `crates/engine/src/pool_registry.rs` | Pool index + swappable validation (`validate_from_cache`, `validate_all`) |
| `crates/engine/src/cold_start.rs` | Background: vault fetch (100 concurrent), tick arrays (PDA-derived), bin arrays, bitmap exts |
| `crates/engine/src/streaming.rs` | Yellowstone gRPC subscriber for live account updates |
| `crates/engine/src/api.rs` | Axum HTTP: /quote (<50ms), /swap, /price (<5ms), /health |
| `crates/aggregator/src/swap_builder.rs` | Swap instructions for all 6 DEXs (+ from_pool_data variants) |
| `crates/aggregator/src/router.rs` | Multi-hop routing (1-4 hops, swappable filter) |
| `crates/aggregator/src/loader.rs` | RPC pool loading (discriminator + dataSize filters) |
| `crates/aggregator/src/cache.rs` | Disk cache + PDA extraction helpers (`extract_clmm_tick_pdas`, `extract_dlmm_bin_pda`) |
| `crates/core/src/traits.rs` | Market trait, PoolMetadata, PoolFinancials, is_active() |
| `crates/router-program/src/lib.rs` | On-chain CPI router with exact amount chaining |
| `tests/surfpool_swap.rs` | Dynamic multi-hop swap on Surfpool (any token pair) |

## Runtime / Tooling

- **Rust edition:** 2024 (requires rustc 1.85+)
- **Router program:** edition 2021, `solana-program` 2.2 (excluded from workspace)
- **Workspace resolver:** 3
- **7 workspace dependencies:** `serde`, `solana-sdk`, `solana-pubkey`, `solana-system-interface`, `spl-associated-token-account`, `spl-token`, `borsh`
- **Aggregator dependencies:** `tokio`, `futures`, `solana-rpc-client`, `indicatif`, `rustyline`, `sysinfo`, `bincode`
- **Engine dependencies:** `axum`, `dashmap`, `tower-http`, `yellowstone-grpc-client`, `yellowstone-grpc-proto`, `rustls`
- **Surfpool:** local mainnet fork, `surfnet_setAccount` cheatcode for token balances
