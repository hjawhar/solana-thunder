# Repository Guidelines

## Project Overview

Solana Thunder is a Rust DEX aggregator library for Solana. It parses on-chain account data and builds swap instructions across 6 DEX protocols through a unified `Market` trait. The library is pure -- no RPC calls, no async, no I/O. Data fetching belongs in the caller or test suite.

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

solana-thunder        Root crate: re-exports all of the above
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

## Key Directories

```
solana-thunder/
+-- Cargo.toml                          # Workspace root, all dep versions here
+-- src/lib.rs                          # Root crate: re-exports all DEX crates
+-- crates/
|   +-- core/src/
|   |   +-- lib.rs                      # Exports traits + constants
|   |   +-- traits.rs                   # Market trait, SwapArgs, SwapContext, etc.
|   |   +-- constants.rs                # WSOL, USDC, TOKEN_PROGRAM, etc.
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
|       +-- lib.rs                      # PumpfunAmmPool model + PumpfunAmmMarket
|       +-- pda.rs                      # 10 PDA derivation functions
+-- tests/
    +-- trade_stream.rs                 # Live DEX swap streaming via Yellowstone gRPC
    +-- creation_stream.rs              # Live token + pool creation streaming
```

## Development Commands

```bash
cargo check                        # Type-check all crates
cargo build                        # Build all crates
cargo test                         # Run unit tests (5 tick array tests in raydium-clmm)
cargo test -p raydium-clmm         # Run tests for one crate
cargo check -p thunder-core        # Check a single crate
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

| Layer | Crate | Usage |
|-------|-------|-------|
| On-chain account data | `borsh` | `BorshDeserialize` on pool model structs. 8-byte discriminator skip for Anchor programs. |
| Instruction args | `borsh` | `BorshSerialize` with hand-crafted discriminator prefix. |
| Cache/API | `serde` | `Serialize, Deserialize` derives on all public types. |

### Constants

- DEX-specific program IDs live in each DEX crate (e.g., `raydium_amm_v4::RAYDIUM_LIQUIDITY_POOL_V4`).
- Shared constants (WSOL, USDC, TOKEN_PROGRAM) live in `thunder_core`.
- All constants are `pub const &str` -- convert to `Pubkey` via `Pubkey::from_str_const()`.

### Naming

- Pool models: `RaydiumAMMV4`, `RaydiumCLMMPool`, `MeteoraDAMMPool`, `MeteoraDLMMPool`, `PumpfunAmmPool`
- Market impls: `RaydiumAmmV4Market`, `RaydiumClmmMarket`, `MeteoraDAMMMarket`, `MeteoraDlmmMarket`, `PumpfunAmmMarket`
- PDA helpers: `derive_*_address()` or `get_*_pda()` -- return `Pubkey` or `(Pubkey, u8)`

### Swap Instruction Discriminators

| DEX | Discriminator |
|-----|--------------|
| Raydium V4 | `[9]` (swap_base_in) / `[11]` (swap_base_out) |
| Raydium CLMM | `[43, 4, 237, 11, 26, 201, 30, 98]` |
| Meteora DAMM V1/V2 | `[248, 198, 158, 145, 225, 117, 135, 200]` |
| Meteora DLMM | `[65, 75, 63, 76, 235, 91, 91, 136]` |
| Pumpfun AMM Buy | `[102, 6, 61, 18, 1, 218, 235, 234]` |
| Pumpfun AMM Sell | `[51, 230, 133, 164, 1, 127, 131, 173]` |

### Pool Creation Discriminators

| DEX | Instruction | Discriminator | Pool Idx | Mint A Idx | Mint B Idx |
|-----|-------------|--------------|----------|------------|------------|
| Raydium V4 | `Initialize2` | `[1]` | 4 | 8 | 9 |
| Raydium CLMM | `create_pool` | `[233,146,209,142,207,104,64,188]` | 2 | 3 | 4 |
| Meteora DAMM V1 | `init_permissionless_pool` | `[118,173,41,157,173,72,97,103]` | 0 | 2 | 3 |
| Meteora DAMM V1 | `init_cp_pool_config2` | `[48,149,220,130,61,11,9,178]` | 0 | 3 | 4 |
| Meteora DAMM V2 | `initialize_pool` | `[95,180,10,172,84,174,232,40]` | 6 | 8 | 9 |
| Meteora DLMM | `initialize_lb_pair` | `[45,154,237,210,221,15,166,92]` | 0 | 2 | 3 |
| Meteora DLMM | `init_cust_perm_lb_pair2` | `[243,73,129,126,51,19,241,107]` | 0 | 2 | 3 |
| Pumpfun AMM | `create_pool` | `[233,146,209,142,207,104,64,188]` | 0 | 3 | 4 |

### Pool Discovery Memcmp Offsets

| DEX | Program ID | data_size | Mint offsets | Discriminator skip |
|-----|-----------|-----------|-------------|-------------------|
| Raydium V4 | `675kPX...` | 752 | 400, 432 | None (byte 0) |
| Raydium CLMM | `CAMMC...` | 1544 | 73, 105 | 8 bytes |
| Meteora DAMM V1 | `Eo7Wj...` | 944, 952 | 40, 72 | 8 bytes |
| Meteora DAMM V2 | `cpamd...` | 1112 | 168, 200 | 8 bytes |
| Meteora DLMM | `LBUZKh...` | 904 | 88, 120 | 8 bytes |
| Pumpfun AMM | `pAMMB...` | N/A | PDA derivation | 8 bytes |

## Important Files

| File | What it is |
|------|-----------|
| `crates/core/src/traits.rs` | `Market` trait, `SwapArgs`, `SwapDirection`, `SwapContext`, `RequiredAccounts`, shared math |
| `crates/core/src/constants.rs` | WSOL, USDC, TOKEN_PROGRAM addresses |
| `crates/raydium-clmm/src/tick_arrays.rs` | CLMM tick array bitmap computation + unit tests |
| `crates/meteora-damm/src/models.rs` | All Meteora DAMM model types (V1, V2, VaultAuthority) |
| `crates/pumpfun-amm/src/pda.rs` | 10 PDA derivation functions for Pumpfun accounts |
| `tests/trade_stream.rs` | Live DEX swap streaming via Yellowstone gRPC |
| `tests/creation_stream.rs` | Live token + pool creation streaming via Yellowstone gRPC |

## Runtime / Tooling

- **Rust edition:** 2024 (requires rustc 1.85+)
- **Workspace resolver:** 3
- **No `rustfmt.toml`, `clippy.toml`, or `.cargo/config.toml`** -- default rules
- **All dependency versions** centralized in root `[workspace.dependencies]`
- **7 workspace dependencies total:** `serde`, `solana-sdk`, `solana-pubkey`, `solana-system-interface`, `spl-associated-token-account`, `spl-token`, `borsh`
- **Dev dependencies** (tests only): `tokio`, `futures`, `dotenvy`, `yellowstone-grpc-client`, `yellowstone-grpc-proto`, `solana-rpc-client`, `solana-rpc-client-api`, `solana-commitment-config`, `solana-account-decoder-client-types`

## Testing

### Unit Tests

5 tests in `crates/raydium-clmm/src/tick_arrays.rs` covering tick array bitmap operations. Run with `cargo test`.

### Integration Tests

Two Geyser-based streaming tests in `tests/`. Require `GEYSER_ENDPOINT` in `.env`:

- **`trade_stream`** -- Streams live swap transactions. Identifies swaps by discriminator, extracts trader/pool/amounts from token balance changes.
- **`creation_stream`** -- Streams live token creation (SPL + Token-2022) and pool creation events across all 6 DEXs. Matches pool creation by per-DEX instruction discriminators and extracts pool address, mint pair, and creator.

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
