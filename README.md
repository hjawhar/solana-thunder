# Solana Thunder

A Rust DEX aggregator library for Solana. Parses on-chain account data and builds swap instructions across 6 DEX protocols through a unified `Market` trait.

The library is pure -- no RPC calls, no async, no I/O. Data fetching belongs in the caller. Integration tests demonstrate live streaming and pool discovery using Yellowstone gRPC.

## Supported DEXs

| DEX | Crate | Pricing Model |
|-----|-------|---------------|
| Raydium AMM V4 | `raydium-amm-v4` | Constant product (x*y=k) |
| Raydium CLMM | `raydium-clmm` | Concentrated liquidity (Q64.64 sqrt_price) |
| Meteora DAMM V1 | `meteora-damm` | Constant product + stable curves |
| Meteora DAMM V2 | `meteora-damm` | sqrt_price based |
| Meteora DLMM | `meteora-dlmm` | Dynamic liquidity bins |
| Pumpfun AMM | `pumpfun-amm` | Bonding curve (virtual reserves) |

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
solana-thunder = { path = "." }
```

Or use individual crates:

```toml
[dependencies]
thunder-core = { path = "crates/core" }
raydium-amm-v4 = { path = "crates/raydium-amm-v4" }
```

### Usage

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

solana-thunder            Root crate: re-exports all of the above
```

Each DEX is an independent crate depending only on `thunder-core`. No DEX crate imports another. Adding a new DEX means creating a new crate -- zero changes to existing code.

### Data Flow

```
Raw account bytes --> BorshDeserialize --> Pool model struct
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
                                         +-- build_swap_instruction()
                                                    |
                                                    v
                                              Vec<Instruction>
```

## Integration Tests

The test suite demonstrates live Solana streaming via Yellowstone gRPC. Create a `.env` file:

```bash
GEYSER_ENDPOINT="https://your-geyser-endpoint:port"
GEYSER_TOKEN="your-auth-token"           # optional, depends on provider
```

### Stream Live Trades

Streams swap transactions across all 6 DEXs in real time. Identifies trades by matching instruction discriminators and computes token balance changes.

```bash
cargo test --test trade_stream -- --nocapture
```

### Stream Token & Pool Creations

Streams new token mints (SPL Token + Token-2022) and new pool creations across all 6 DEXs.

```bash
cargo test --test creation_stream -- --nocapture
```

## Development

```bash
cargo check                    # Type-check everything
cargo build                    # Build all crates
cargo test                     # Run unit tests (5 tick array tests)
cargo test --test trade_stream -- --nocapture       # Live trade stream
cargo test --test creation_stream -- --nocapture    # Live creation stream
```

### Project Structure

```
solana-thunder/
+-- Cargo.toml                          Workspace root
+-- src/lib.rs                          Re-exports all DEX crates
+-- crates/
|   +-- core/src/
|   |   +-- lib.rs                      Exports traits + constants
|   |   +-- traits.rs                   Market trait, SwapArgs, SwapContext
|   |   +-- constants.rs                WSOL, USDC, TOKEN_PROGRAM
|   +-- raydium-amm-v4/src/lib.rs       RaydiumAMMV4 + RaydiumAmmV4Market
|   +-- raydium-clmm/src/
|   |   +-- lib.rs                      RaydiumCLMMPool + RaydiumClmmMarket
|   |   +-- tick_arrays.rs              Tick array bitmap computation + tests
|   +-- meteora-damm/src/
|   |   +-- lib.rs                      MeteoraDAMMMarket + MeteoraDAMMV2Market
|   |   +-- models.rs                   Pool models for V1, V2, VaultAuthority
|   |   +-- utils.rs                    PDA derivation (vault, LP mint)
|   +-- meteora-dlmm/src/lib.rs         MeteoraDLMMPool + MeteoraDlmmMarket
|   +-- pumpfun-amm/src/
|       +-- lib.rs                      PumpfunAmmPool + PumpfunAmmMarket
|       +-- pda.rs                      10 PDA derivation functions
+-- tests/
    +-- trade_stream.rs                 Live DEX swap streaming
    +-- creation_stream.rs              Live token + pool creation streaming
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
