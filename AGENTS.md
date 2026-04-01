# Repository Guidelines

## Project Overview

Solana Thunder is a Rust DEX aggregator for Solana. It consists of 6 pure DEX crates that parse on-chain account data and build swap instructions through a unified `Market` trait, plus an aggregator crate that loads all pools from RPC, finds multi-hop routes, builds versioned transactions, and exposes an interactive CLI. The DEX crates are pure -- no RPC calls, no async, no I/O. The aggregator crate handles all I/O. No external APIs -- all data is on-chain.

## Architecture

Each DEX is an independent crate implementing `thunder_core::Market`. There is no dispatch enum or polymorphic wrapper -- callers work with concrete market types directly.

```
thunder-core          Market trait, shared types, constants
    ^
    |  (only dependency each DEX crate has on the workspace)
    |
    +-- raydium-amm-v4    Constant product AMM
    +-- raydium-clmm      Concentrated liquidity (Q64.64 sqrt_price)
    +-- meteora-damm      Dynamic AMM V1 (constant product + stable) and V2 (sqrt_price)
    +-- meteora-dlmm      Dynamic liquidity bins
    +-- pumpfun-amm       Bonding curve (virtual reserves)

thunder-aggregator    Aggregator: pool loading, routing, pricing, caching, CLI
    depends on: all DEX crates + thunder-core + solana-rpc-client + tokio

solana-thunder        Root crate: re-exports all DEX crates
```

No DEX crate imports another DEX crate. Adding a new DEX means creating a new crate and adding it to the workspace -- zero changes to existing code.

### Data Flow

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
                                       +-- required_accounts()
                                       +-- build_swap_instruction(SwapContext, SwapArgs, SwapDirection)
                                                    |
                                                    v
                                              Vec<Instruction>
```

The caller is responsible for:
1. Fetching raw account data (via RPC, gRPC, cache)
2. Deserializing it into the pool model struct (`BorshDeserialize`)
3. Fetching token balances for the pool vaults
4. Constructing the market struct with those values
5. Calling `required_accounts()` to learn what extra data to fetch
6. Populating `SwapContext` and calling `build_swap_instruction()`

### Aggregator Data Flow

```
Cache file (pools.cache)                  RPC getProgramAccounts
     |                                         (disc + dataSize filters)
     v                                         |
Load from disk (6s)  -- OR --  Load from RPC (3-4 min) + save cache
     |                                         |
     +------- PoolIndex (token graph) ---------+
                      |
                      v
SOL/USD price <-- on-chain CLMM sqrt_price (highest-liquidity SOL/USDC pool)
                      |
                      v
User query (A->B, amount) --> Router (BFS, 1-4 hops, bidirectional) --> simulate all paths
                                                       |
                                                  best route
                                                       |
                          TransactionBuilder --> VersionedTransaction (v0)
```

## Key Directories

```
solana-thunder/
+-- Cargo.toml                          # Workspace root, all dep versions here
+-- src/lib.rs                          # Root crate: re-exports all DEX crates
+-- crates/
|   +-- core/src/
|   |   +-- lib.rs                      # Exports traits + constants
|   |   +-- traits.rs                   # Market trait, SwapArgs, SwapContext, etc.
|   |   +-- constants.rs                # WSOL, USDC, USDT, quote_priority, infer_mint_decimals
|   +-- raydium-amm-v4/src/
|   |   +-- lib.rs                      # RaydiumAMMV4 model + RaydiumAmmV4Market
|   +-- raydium-clmm/src/
|   |   +-- lib.rs                      # RaydiumCLMMPool model + RaydiumClmmMarket
|   |   +-- tick_arrays.rs              # Tick array bitmap computation + tests
|   +-- meteora-damm/src/
|   |   +-- lib.rs                      # V1 MeteoraDAMMMarket + V2 MeteoraDAMMV2Market
|   |   +-- models.rs                   # Pool models for V1, V2, VaultAuthority
|   |   +-- utils.rs                    # PDA derivation (vault, LP mint)
|   +-- meteora-dlmm/src/
|   |   +-- lib.rs                      # MeteoraDLMMPool model + MeteoraDlmmMarket
|   +-- pumpfun-amm/src/
|   |   +-- lib.rs                      # PumpfunAmmPool model + PumpfunAmmMarket
|   |   +-- pda.rs                      # 10 PDA derivation functions
|   +-- aggregator/src/
|       +-- lib.rs                      # Public module exports
|       +-- main.rs                     # CLI binary entry point (thunder-agg)
|       +-- types.rs                    # PoolEntry, Route, RouteHop, Quote, TokenPrice, etc.
|       +-- pool_index.rs              # In-memory token-pair graph (mint adjacency list)
|       +-- loader.rs                   # Async RPC pool loading for all 6 DEXs
|       +-- cache.rs                    # Disk cache: save/load pools as binary (bincode)
|       +-- router.rs                   # Multi-hop route finding (BFS, 1-4 hops, bidirectional)
|       +-- transaction.rs              # Versioned transaction builder (v0)
|       +-- price.rs                    # SOL/USD on-chain via CLMM sqrt_price, per-token via pools
|       +-- stats.rs                    # Pool and system resource statistics
|       +-- cli.rs                      # Progress bars + interactive REPL
+-- tests/
    +-- trade_stream.rs                 # Live DEX swap streaming via Yellowstone gRPC
    +-- creation_stream.rs              # Live token + pool creation streaming
    +-- pool_financials.rs              # Live pool update streaming
    +-- validate_prices.rs              # Price validation across DEXs
```

## Development Commands

```bash
cargo check                        # Type-check all crates
cargo build                        # Build all crates
cargo test                         # Run unit tests (5 tick array tests in raydium-clmm)
cargo test -p raydium-clmm         # Run tests for one crate
cargo check -p thunder-core        # Check a single crate
cargo build --release -p thunder-aggregator  # Build aggregator binary (release)
```

### Running the Aggregator

```bash
# Run with an RPC endpoint (first run loads from RPC, saves cache)
RPC_URL="https://your-rpc-endpoint.com" cargo run --release -p thunder-aggregator

# Subsequent runs load from cache (~6s instead of ~4min)
RPC_URL="https://your-rpc-endpoint.com" cargo run --release -p thunder-aggregator

# Or run the built binary directly
RPC_URL="https://your-rpc-endpoint.com" ./target/release/thunder-agg
```

Environment variables:

| Variable | Default | Description |
|---|---|---|
| `RPC_URL` | `https://api.mainnet-beta.solana.com` | Solana RPC endpoint |
| `CACHE_PATH` | `pools.cache` | Pool cache file location |
| `CACHE_MAX_AGE` | `3600` | Max cache age in seconds before forcing RPC reload. Set to `0` to always reload. |
| `PRIVATE_KEY` | (none) | Base58 keypair for swap simulation (never sent, only `simulateTransaction`) |

The aggregator will:
1. Fetch SOL/USD price on-chain from the highest-liquidity Raydium CLMM SOL/USDC pool's `sqrt_price_x64`
2. Load pools from cache if fresh, otherwise from RPC (all pools, no mint filter)
3. Save cache to disk for next startup
4. Enter an interactive REPL

### Pool Loading

Fetches ALL pools from all 6 DEXs using `getProgramAccounts` with discriminator + dataSize filters only (no mint filter). Every pool across every token pair is loaded. Typical counts:

| DEX | Pools |
|---|---|
| Meteora DAMM V2 | ~874K |
| Pumpfun AMM | ~829K |
| Raydium CLMM | ~170K |
| Meteora DLMM | ~140K |
| Meteora DAMM V1 | ~16K |
| **Total** | **~2M** |

Vault balances fetched via `getMultipleAccounts` in batches of 100, 20 concurrent.

### Pool Cache

On first run, pools are saved to `pools.cache` (~1.6 GB for 2M pools). On subsequent runs, the cache is loaded in ~6 seconds instead of ~4 minutes from RPC. The cache stores serialized pool structs + vault balances via bincode -- everything needed to reconstruct `Box<dyn Market>` with zero RPC calls.

### REPL Commands

```
thunder> help                                           # Show available commands
thunder> price SOL                                      # SOL price in USD
thunder> price <mint_address>                           # Token price in SOL + USD
thunder> route SOL <to_mint> 1.0                        # Find best routes for 1 SOL
thunder> route <from_mint> <to_mint> <amount>           # Route between any tokens
thunder> stats                                          # Pool counts, memory, uptime
thunder> exit                                           # Exit
```

### Examples

```bash
# Test on-chain SOL/USD price
RPC_URL="https://..." cargo run -p thunder-aggregator --example test_price

# Simulate a swap (requires PRIVATE_KEY in .env, NEVER sends)
RPC_URL="https://..." cargo run --release -p thunder-aggregator --example simulate_swap
```

### Integration Tests (require Geyser endpoint)

```bash
# Stream live swap transactions across all DEXs
cargo test --test trade_stream -- --nocapture

# Stream live token mints and pool creations across all DEXs
cargo test --test creation_stream -- --nocapture
```

Requires `GEYSER_ENDPOINT` (and optionally `GEYSER_TOKEN`) in `.env` or environment.

## Code Conventions

### Market Struct Pattern

Every DEX follows the same structure:

```rust
pub struct SomeDexMarket {
    pub pool: SomeDexPool,       // BorshDeserialize'd on-chain data
    pub pool_address: String,    // Pool account address
    pub quote_balance: u64,      // Cached vault balance
    pub base_balance: u64,       // Cached vault balance
}

impl SomeDexMarket {
    pub fn new(pool: SomeDexPool, pool_address: String, quote_balance: u64, base_balance: u64) -> Self { ... }
}

impl Market for SomeDexMarket {
    fn metadata(&self) -> Result<PoolMetadata, GenericError> { ... }
    fn financials(&self) -> Result<PoolFinancials, GenericError> { ... }
    fn calculate_output(&self, amount_in: u64, direction: SwapDirection) -> Result<u64, GenericError> { ... }
    fn calculate_price_impact(&self, amount_in: u64, direction: SwapDirection) -> Result<u64, GenericError> { ... }
    fn current_price(&self) -> Result<f64, GenericError> { ... }
    fn build_swap_instruction(&self, context: SwapContext, args: SwapArgs, direction: SwapDirection) -> Result<Vec<Instruction>, GenericError> { ... }
    fn required_accounts(&self, user: Pubkey, direction: SwapDirection) -> Result<RequiredAccounts, GenericError> { ... }
}
```

### Error Handling

`type GenericError = Box<dyn Error + Send + Sync>` -- defined in `thunder-core`, used everywhere. String errors via `.into()`. No `thiserror` or `anyhow`.

### Serialization

|Layer|Crate|Usage|
|---|---|---|
|On-chain account data|`borsh`|`BorshDeserialize` on pool model structs. 8-byte discriminator skip for Anchor programs.|
|Instruction args|`borsh`|`BorshSerialize` with hand-crafted discriminator prefix.|
|Disk cache|`bincode`|`CachedPool` enum with all DEX pool variants. Saved/loaded via `cache::save_cache`/`load_cache`.|
|Cache/API|`serde`|`Serialize, Deserialize` derives on all public types.|

### Constants

- DEX-specific program IDs live in each DEX crate (e.g., `raydium_amm_v4::RAYDIUM_LIQUIDITY_POOL_V4`).
- Shared constants (WSOL, USDC, USDT, PYUSD, JITOSOL, MSOL, BSOL, JUPSOL, TOKEN_PROGRAM, TOKEN_PROGRAM_2022) live in `thunder_core`.
- Quote currency ordering: `thunder_core::quote_priority()` returns rank (WSOL=0, USDC=1, ...).
- Token decimal inference: `thunder_core::infer_mint_decimals()` for well-known tokens + pump.fun heuristic.
- All constants are `pub const &str` -- convert to `Pubkey` via `Pubkey::from_str_const()`.

### Naming

- Pool models: `RaydiumAMMV4`, `RaydiumCLMMPool`, `MeteoraDAMMPool`, `MeteoraDLMMPool`, `PumpfunAmmPool`
- Market impls: `RaydiumAmmV4Market`, `RaydiumClmmMarket`, `MeteoraDAMMMarket`, `MeteoraDlmmMarket`, `PumpfunAmmMarket`
- PDA helpers: `derive_*_address()` or `get_*_pda()` -- return `Pubkey` or `(Pubkey, u8)`

### Swap Instruction Discriminators

|DEX|Discriminator|
|---|---|
|Raydium V4|`[9]` (swap_base_in) / `[11]` (swap_base_out)|
|Raydium CLMM|`[43, 4, 237, 11, 26, 201, 30, 98]`|
|Meteora DAMM V1/V2|`[248, 198, 158, 145, 225, 117, 135, 200]`|
|Meteora DLMM|`[65, 75, 63, 76, 235, 91, 91, 136]`|
|Pumpfun AMM Buy|`[102, 6, 61, 18, 1, 218, 235, 234]`|
|Pumpfun AMM Sell|`[51, 230, 133, 164, 1, 127, 131, 173]`|

### Pool Creation Discriminators

|DEX|Instruction|Discriminator|Pool Idx|Mint A Idx|Mint B Idx|
|---|---|---|---|---|---|
|Raydium V4|`Initialize2`|`[1]`|4|8|9|
|Raydium CLMM|`create_pool`|`[233,146,209,142,207,104,64,188]`|2|3|4|
|Meteora DAMM V1|`init_permissionless_pool`|`[118,173,41,157,173,72,97,103]`|0|2|3|
|Meteora DAMM V1|`init_cp_pool_config2`|`[48,149,220,130,61,11,9,178]`|0|3|4|
|Meteora DAMM V2|`initialize_pool`|`[95,180,10,172,84,174,232,40]`|6|8|9|
|Meteora DLMM|`initialize_lb_pair`|`[45,154,237,210,221,15,166,92]`|0|2|3|
|Meteora DLMM|`init_cust_perm_lb_pair2`|`[243,73,129,126,51,19,241,107]`|0|2|3|
|Pumpfun AMM|`create_pool`|`[233,146,209,142,207,104,64,188]`|0|3|4|

### Pool Discovery Filters

The aggregator fetches ALL pools using discriminator + dataSize filters only (no mint filter):

|DEX|Program ID|data_size|Anchor Discriminator|
|---|---|---|---|
|Raydium V4|`675kPX...`|752|None (no Anchor)|
|Raydium CLMM|`CAMMC...`|1544|`[247,237,227,245,215,195,222,70]` (PoolState)|
|Meteora DAMM V1|`Eo7Wj...`|944|`[241,154,109,4,17,177,109,188]` (Pool)|
|Meteora DAMM V2|`cpamd...`|1112|`[241,154,109,4,17,177,109,188]` (Pool)|
|Meteora DLMM|`LBUZKh...`|904|`[33,11,49,98,181,101,177,13]` (LbPair)|
|Pumpfun AMM|`pAMMB...`|N/A|`[241,154,109,4,17,177,109,188]` (Pool)|

## Important Files

|File|What it is|
|---|---|
|`crates/core/src/traits.rs`|`Market` trait, `SwapArgs`, `SwapDirection`, `SwapContext`, `RequiredAccounts`, shared math|
|`crates/core/src/constants.rs`|WSOL, USDC, USDT, PYUSD, LST tokens, `quote_priority()`, `infer_mint_decimals()`|
|`crates/raydium-clmm/src/tick_arrays.rs`|CLMM tick array bitmap computation + unit tests|
|`crates/meteora-damm/src/models.rs`|All Meteora DAMM model types (V1, V2, VaultAuthority)|
|`crates/pumpfun-amm/src/pda.rs`|10 PDA derivation functions for Pumpfun accounts|
|`crates/aggregator/src/loader.rs`|Async RPC pool loading for all 6 DEXs (discriminator + dataSize filters)|
|`crates/aggregator/src/cache.rs`|Disk cache: save/load 2M pools as bincode binary (~1.6 GB, loads in ~6s)|
|`crates/aggregator/src/router.rs`|Multi-hop route finding (BFS, 1-4 hops, bidirectional neighbor search)|
|`crates/aggregator/src/transaction.rs`|Versioned transaction builder (v0, multi-hop composition)|
|`crates/aggregator/src/price.rs`|SOL/USD on-chain via CLMM `sqrt_price_x64`, per-token via pool graph|
|`crates/aggregator/src/cli.rs`|Progress bars (indicatif) + interactive REPL (rustyline)|
|`crates/aggregator/examples/test_price.rs`|Standalone on-chain SOL/USD price test|
|`crates/aggregator/examples/simulate_swap.rs`|Swap simulation (build + simulateTransaction, never sends)|
|`tests/trade_stream.rs`|Live DEX swap streaming via Yellowstone gRPC|
|`tests/creation_stream.rs`|Live token + pool creation streaming via Yellowstone gRPC|

## Runtime / Tooling

- **Rust edition:** 2024 (requires rustc 1.85+)
- **Workspace resolver:** 3
- **No `rustfmt.toml`, `clippy.toml`, or `.cargo/config.toml`** -- default rules
- **All dependency versions** centralized in root `[workspace.dependencies]`
- **7 workspace dependencies total:** `serde`, `solana-sdk`, `solana-pubkey`, `solana-system-interface`, `spl-associated-token-account`, `spl-token`, `borsh`
- **Aggregator dependencies:** `tokio`, `futures`, `solana-rpc-client`, `solana-rpc-client-api`, `solana-account-decoder-client-types`, `solana-commitment-config`, `indicatif`, `rustyline`, `sysinfo`, `bincode`
- **Dev dependencies** (tests only): `tokio`, `futures`, `dotenvy`, `yellowstone-grpc-client`, `yellowstone-grpc-proto`, `solana-rpc-client`, `solana-rpc-client-api`, `solana-commitment-config`, `solana-account-decoder-client-types`

## Testing

### Unit Tests

5 tests in `crates/raydium-clmm/src/tick_arrays.rs` covering tick array bitmap operations. Run with `cargo test`.

### Integration Tests

Four Geyser/RPC-based tests in `tests/`. The streaming tests require `GEYSER_ENDPOINT` in `.env`:

- **`trade_stream`** -- Streams live swap transactions. Identifies swaps by discriminator, extracts trader/pool/amounts from token balance changes.
- **`creation_stream`** -- Streams live token creation (SPL + Token-2022) and pool creation events across all 6 DEXs. Matches pool creation by per-DEX instruction discriminators and extracts pool address, mint pair, and creator.
- **`pool_financials`** -- Streams live pool account updates across all DEXs, deserializes pool data, fetches mint decimals.
- **`validate_prices`** -- Validates price calculations across DEX pools.

### Test Pattern (unit tests)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use solana_pubkey::Pubkey;

    fn make_test_pool(tick_current: i32, tick_spacing: u16, bitmap: [u64; 16]) -> RaydiumCLMMPool {
        RaydiumCLMMPool {
            tick_current,
            tick_spacing,
            tick_array_bitmap: bitmap,
            token_mint_0: Pubkey::new_unique(),
            // ... zero/default all other fields
        }
    }

    #[test]
    fn test_something() {
        let pool = make_test_pool(0, 1, [0u64; 16]);
        let result = some_function(&pool);
        assert_eq!(result, expected);
    }
}
```
