# Repository Guidelines

## Project Overview

Solana Thunder is a Rust DEX aggregator for Solana with an on-chain router program. 6 pure DEX crates parse on-chain account data and compute swap outputs through a unified `Market` trait. An aggregator crate loads all pools from RPC, finds multi-hop routes via BFS, and provides pricing/caching. An engine crate provides a persistent HTTP API with live data streaming and builds compact router instructions. A separate on-chain BPF program (`thunder-router`) executes multi-hop swaps via CPI, chaining balance deltas between hops with a final slippage check. No external APIs -- all data is on-chain.

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
thunder-engine        HTTP API, AccountStore, PoolRegistry, cold_start, streaming, swap.rs
solana-thunder        Root crate: re-exports all DEX crates

thunder-router        On-chain BPF program (solana-program 2.2, excluded from workspace)
                      Compact ExecuteRouteArgs instruction -> per-DEX CPI adapters
                      Balance-delta chaining between hops, final slippage check
```

No DEX crate imports another DEX crate. `thunder-router` is a standalone crate with no workspace dependency -- it uses `solana-program 2.2` while the workspace uses `solana-sdk 3.0`.

### Swap Execution Flow

```
Pool cache / RPC  -->  PoolIndex (2M pools, token graph)
                            |
                            v
                    Router (BFS, 1-4 hops, bidirectional)
                            |
                            v
                    engine/swap.rs (compact ExecuteRouteArgs, per-DEX account collectors)
                            |
                            v
                    Single router instruction -> thunder-router on-chain program
                            |
                            v
                    Per-hop CPI into DEX programs (balance-delta chaining)
                            |
                            v
                    Final slippage check on last hop output
```

The engine does NOT build per-DEX swap instructions directly. Instead, `swap.rs` collects accounts in adapter order and serializes a single compact `ExecuteRouteArgs` (~30 bytes for a 2-hop route). The on-chain router program deserializes this, CPIs into each DEX, reads destination token balances before/after each hop, and chains the actual output amount as input to the next hop.

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
+-- Cargo.toml                          # Workspace root (excludes router-program)
+-- src/lib.rs                          # Root crate: re-exports all DEX crates
+-- crates/
|   +-- core/src/
|   |   +-- traits.rs                   # Market trait, AccountDataProvider, calculate_output_live
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
|   |   +-- router.rs                   # Multi-hop routing (live data via AccountDataProvider, cached swappable set)
|   |   +-- swap_builder.rs             # Swap instructions for all 6 DEXs
|   |   +-- price.rs                    # SOL/USD on-chain via CLMM sqrt_price
|   |   +-- pool_index.rs               # In-memory token-pair graph
|   |   +-- cli.rs                      # Progress bars + interactive REPL
|   |   +-- main.rs                     # CLI binary (thunder-agg)
|   +-- engine/src/                     # Persistent service (library only, binary in bin/)
|   |   +-- account_store.rs            # DashMap store, implements AccountDataProvider
|   |   +-- pool_registry.rs            # Swappable validation (cached Arc<HashSet>, vault-to-pool reverse index)
|   |   +-- cold_start.rs               # Background: vault fetch (100 concurrent), tick arrays, bin arrays, bitmap exts
|   |   +-- streaming.rs                # Yellowstone gRPC: live updates + vault re-validation (Token + Token-2022)
|   |   +-- api.rs                      # Axum HTTP: GET /quote, POST /swap, GET /price, GET /health
|   |   +-- swap.rs                     # Compact router instruction builder (ExecuteRouteArgs, per-DEX account collectors)
|   +-- router-program/                 # On-chain BPF program (excluded from workspace)
|       +-- Cargo.toml                  # edition 2021, solana-program 2.2
|       +-- src/
|           +-- lib.rs                  # Entrypoint, DexType/SwapHop/ExecuteRouteArgs, hop dispatch loop
|           +-- adapters/
|               +-- mod.rs              # Adapter module declarations
|               +-- common.rs           # read_token_balance, shared CPI helpers
|               +-- meteora_damm_v1.rs  # DAMM V1 CPI adapter
|               +-- meteora_damm_v2.rs  # DAMM V2 CPI adapter
|               +-- meteora_dlmm.rs     # DLMM CPI adapter
|               +-- raydium_clmm.rs     # Raydium CLMM CPI adapter
|               +-- raydium_v4.rs       # Raydium AMM V4 CPI adapter
|               +-- pumpfun.rs          # Pumpfun buy/sell CPI adapter
+-- tests/
|   +-- helpers/mod.rs                  # Shared test helpers
|   +-- surfpool_swap.rs                # Surfpool: deploy router, create ALT, execute multi-hop swap
|   +-- simulate_swap.rs                # Swap simulation test
|   +-- trade_stream.rs                 # Live DEX swap streaming via Yellowstone gRPC
|   +-- creation_stream.rs              # Live token + pool creation streaming
|   +-- pool_financials.rs              # Live pool update streaming
|   +-- validate_prices.rs              # Price validation across DEXs
+-- .surfpool/                          # Surfpool local fork state/logs
```

## Development Commands

```bash
cargo check                        # Type-check all workspace crates
cargo build                        # Build all workspace crates
cargo test                         # Run unit tests
cargo build --release --bin thunder-engine  # Build engine binary
cargo build --release -p thunder-aggregator  # Build aggregator CLI

# Build router program (BPF, outside workspace)
cd crates/router-program && cargo build-sbf

# Run engine (persistent service with HTTP API)
RPC_URL="https://..." cargo run --release --bin thunder-engine

# Run aggregator CLI (interactive REPL)
RPC_URL="https://..." cargo run --release -p thunder-aggregator

# Engine API
curl http://localhost:8080/health
curl "http://localhost:8080/quote?inputMint=SOL&outputMint=6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN&amount=100000000&maxHops=2"
curl "http://localhost:8080/price?mint=SOL"
```

### Surfpool Setup

Surfpool is a local mainnet fork for testing. The `surfpool_swap` test deploys the router program and executes multi-hop swaps end-to-end.

```bash
# 1. Start Surfpool (local mainnet fork)
surfpool

# 2. Build the router program BPF
cd crates/router-program && cargo build-sbf

# 3. Deploy to Surfpool (program ID: 7WgM9BLWicvmxZwNsT5AUKqxsf6QqBSy2RxeEEwjzJFu)
solana program deploy target/deploy/thunder_router.so \
  --program-id 7WgM9BLWicvmxZwNsT5AUKqxsf6QqBSy2RxeEEwjzJFu \
  --url http://localhost:8899

# 4. Run the swap test
INPUT=SOL OUTPUT=6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN \
  AMOUNT=0.1 MAX_HOPS=2 \
  cargo test --release --test surfpool_swap -- --nocapture
```

The `surfpool_swap` test creates an Address Lookup Table (ALT) with 18 common addresses (DEX programs, authorities, token programs, memo program) to reduce transaction size. It loads pools from RPC, finds routes via BFS, builds the compact router instruction, and submits the transaction to the local validator.

### Environment Variables

| Variable | Default | Description |
|---|---|---|
| `RPC_URL` | `https://api.mainnet-beta.solana.com` | Solana RPC endpoint |
| `CACHE_PATH` | `pools.cache` | Pool cache file location |
| `CACHE_MAX_AGE` | `3600` | Max cache age (seconds) before RPC reload |
| `PRIVATE_KEY` | (none) | Base58 keypair in `.env` (never committed) |

### Pool Loading

Fetches ALL pools from all 6 DEXs using `getProgramAccounts` with discriminator + dataSize filters only (no mint filter). ~2M pools total. Vault balances cached per-pool during loading (used for instant startup validation).

### Engine Startup

The engine starts serving HTTP immediately after cache load (~6s). No blocking vault fetch.

```
1. Load cache              ~6s   pools.cache -> PoolIndex
2. validate_from_cache     instant  uses market.financials() (cached vault balances)
3. Start HTTP server       instant  /quote works immediately
4. gRPC streaming          background  live account updates + vault re-validation
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

`calculate_output_live()` reads live pool state (sqrt_price, active_id, liquidity) and vault balances from the `AccountStore` via the `AccountDataProvider` trait on every quote request. Tick arrays, bin arrays, and bitmap extensions are only needed for building on-chain swap instructions, not for computing quotes. Streaming triggers `on_vault_update()` to re-validate swappable status when Token Program or Token-2022 vault balances change.

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
    fn calculate_output_live(&self, amount_in: u64, direction: SwapDirection,
        pool_data: Option<&[u8]>, quote_vault_balance: u64, base_vault_balance: u64,
    ) -> Result<u64, GenericError> { self.calculate_output(amount_in, direction) }
    fn calculate_price_impact(&self, amount_in: u64, direction: SwapDirection) -> Result<u64, GenericError>;
    fn current_price(&self) -> Result<f64, GenericError>;
}

pub trait AccountDataProvider: Send + Sync {
    fn pool_account_data(&self, pubkey: &Pubkey) -> Option<Vec<u8>>;
    fn token_balance(&self, vault_pubkey: &Pubkey) -> u64;
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

### Router Program (thunder-router)

The on-chain BPF program at `7WgM9BLWicvmxZwNsT5AUKqxsf6QqBSy2RxeEEwjzJFu`. Located in `crates/router-program/`, excluded from the workspace due to the `solana-program 2.2` vs `solana-sdk 3.0` incompatibility.

**Instruction format:**

```rust
#[repr(u8)]
enum DexType {
    MeteoraDAMMV1 = 0,
    MeteoraDAMMV2 = 1,
    MeteoraDLMM   = 2,
    RaydiumCLMM   = 3,
    RaydiumAMMV4  = 4,
    PumpfunBuy    = 5,
    PumpfunSell   = 6,
}

struct SwapHop {
    dex_type: DexType,     // which adapter to invoke
    num_accounts: u8,      // how many accounts this hop consumes
}

struct ExecuteRouteArgs {
    amount_in: u64,        // input amount for the first hop
    min_amount_out: u64,   // slippage threshold on final output
    hops: Vec<SwapHop>,    // ordered list of hops
}
```

A 2-hop route serializes to ~30 bytes of instruction data. All swap accounts are passed as a flat list; each hop consumes `num_accounts` from the current offset.

**Adapter pattern (OKX-style uniform prefix):**

Every adapter receives accounts with a uniform prefix at indices `[0..3]`:

| Index | Account |
|---|---|
| 0 | DEX program |
| 1 | Swap authority (user or PDA) |
| 2 | Swap source token account |
| 3 | Swap destination token account |

Remaining accounts are DEX-specific (pool state, vaults, oracles, etc.).

**Balance-delta chaining:**

The router reads the destination token balance before and after each CPI. The delta (`balance_after - balance_before`) becomes the `current_amount` for the next hop. This means the router uses actual on-chain output, not off-chain estimates.

**Account counts per DEX:**

| DEX | Accounts |
|---|---|
| Meteora DAMM V1 | 16 |
| Meteora DAMM V2 | 13 |
| Meteora DLMM | 19 |
| Raydium CLMM | 18 |
| Raydium AMM V4 | 19 |
| Pumpfun (buy/sell) | 13 |

### Engine swap.rs

`crates/engine/src/swap.rs` builds complete unsigned `VersionedTransaction` objects for the router path. It does NOT build per-DEX swap instructions directly.

**Duplicated types:** `DexType`, `SwapHop`, and `ExecuteRouteArgs` are duplicated from the router program to avoid a cross-crate dependency between `solana-program 2.x` and `solana-sdk 3.x`.

**Program/authority constants:** ~12 `const` strings for all DEX program IDs, authorities, and the router program ID.

**Per-DEX account collectors:** Six `collect_*_accounts` functions (`collect_damm_v1_accounts`, `collect_damm_v2_accounts`, `collect_dlmm_accounts`, `collect_clmm_accounts`, `collect_ray_v4_accounts`, `collect_pumpfun_accounts`) that deserialize pool data, resolve PDAs, and produce the correctly-ordered `Vec<AccountMeta>` matching each adapter's expected layout.

**Transaction structure:**

```
1. Pre-instructions:
   - ATA creation (idempotent) for intermediate + output mints
   - WSOL wrap if input is SOL (create ATA, transfer, sync_native)
2. Single compact router instruction:
   - program_id = ROUTER_PROGRAM_ID
   - accounts = flat concatenation of all per-hop account lists
   - data = borsh-serialized ExecuteRouteArgs
3. Post-instructions:
   - WSOL unwrap (close_account) if input was SOL
```

### Swap Builder (aggregator)

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
| `crates/engine/src/account_store.rs` | DashMap store, implements `AccountDataProvider` for live routing |
| `crates/engine/src/pool_registry.rs` | Swappable validation (cached `Arc<HashSet>`, vault-to-pool reverse index) |
| `crates/engine/src/cold_start.rs` | Background: vault fetch (100 concurrent), tick arrays, bin arrays, bitmap exts |
| `crates/engine/src/streaming.rs` | Yellowstone gRPC: live updates + vault re-validation (Token + Token-2022) |
| `crates/engine/src/api.rs` | Axum HTTP: GET /quote, POST /swap, GET /price, GET /health |
| `crates/engine/src/swap.rs` | Compact router instruction builder (ExecuteRouteArgs, per-DEX account collectors) |
| `crates/router-program/src/lib.rs` | On-chain entrypoint: hop dispatch, balance-delta chaining, slippage check |
| `crates/router-program/src/adapters/common.rs` | Shared CPI helpers, `read_token_balance` |
| `crates/router-program/src/adapters/*.rs` | 7 per-DEX CPI adapters (DAMM V1/V2, DLMM, CLMM, V4, Pumpfun buy/sell) |
| `crates/aggregator/src/swap_builder.rs` | Swap instructions for all 6 DEXs (+ from_pool_data variants) |
| `crates/aggregator/src/router.rs` | Multi-hop routing (live data, pre-resolved mints, cached swappable set) |
| `crates/aggregator/src/loader.rs` | RPC pool loading (discriminator + dataSize filters) |
| `crates/aggregator/src/cache.rs` | Disk cache + PDA extraction helpers |
| `crates/core/src/traits.rs` | Market trait, AccountDataProvider, calculate_output_live |
| `tests/surfpool_swap.rs` | End-to-end: deploy router to Surfpool, create ALT, execute multi-hop swap |
| `tests/simulate_swap.rs` | Swap simulation test |
| `tests/trade_stream.rs` | Live DEX swap streaming via Yellowstone gRPC |
| `tests/creation_stream.rs` | Live token + pool creation streaming |
| `tests/pool_financials.rs` | Live pool update streaming |
| `tests/validate_prices.rs` | Price validation across DEXs |

## Runtime / Tooling

- **Rust edition:** 2024 (requires rustc 1.85+)
- **Router program edition:** 2021 (solana-program 2.2, excluded from workspace via `Cargo.toml` `exclude`)
- **Workspace resolver:** 3
- **7 workspace dependencies:** `serde`, `solana-sdk`, `solana-pubkey`, `solana-system-interface`, `spl-associated-token-account`, `spl-token`, `borsh`
- **Aggregator dependencies:** `tokio`, `futures`, `solana-rpc-client`, `indicatif`, `rustyline`, `sysinfo`, `bincode`
- **Engine dependencies:** `axum`, `dashmap`, `tower-http`, `yellowstone-grpc-client`, `yellowstone-grpc-proto`, `rustls`
- **Router program dependencies:** `solana-program 2.2`, `spl-token 9.0`, `spl-associated-token-account 8.0`, `borsh 1.6`
