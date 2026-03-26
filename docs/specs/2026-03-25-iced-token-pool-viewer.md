# Iced Token & Pool Viewer — Design Spec

## Goal

Desktop application that streams live token creation events from Solana (both SPL Token and Token-2022) via Yellowstone gRPC, displays them in a live feed, and discovers DEX pools on-demand when the user selects a token.

## Non-Goals

- Swap execution from the UI (instruction building stays in the library only)
- Price charts or historical data
- Persistent storage or database
- Custom theming (default Iced theme, style later)
- Automatic pool discovery on every new token (on-demand only)

## Architecture

### File Layout

Binary and library coexist in the root package. `src/lib.rs` is the library (re-exports DEX crates, unchanged). `src/main.rs` is the Iced application binary.

```
src/
├── lib.rs           # Library re-exports (existing, unchanged)
├── main.rs          # fn main() → iced::application(...).subscription(...).run()
├── app.rs           # App struct, update(), view(), subscription()
├── message.rs       # Message enum + StreamEvent enum + TokenEvent struct
├── stream.rs        # Yellowstone gRPC subscription → StreamEvent items
└── discovery.rs     # On-demand pool discovery per DEX (async RPC)
```

### Dependencies

Added to root `Cargo.toml` under `[dependencies]`:

```toml
iced = { version = "0.14", features = ["tokio", "sipper"] }
yellowstone-grpc-client = "11.0"
yellowstone-grpc-proto = { version = "11.0", features = ["plugin"] }
tonic = "0.14"
tokio = { version = "1.43.0", features = ["rt-multi-thread", "macros", "sync"] }
solana-rpc-client = "3.1"
solana-rpc-client-api = "3.1"
solana-commitment-config = "3.1"
solana-account-decoder-client-types = "3.1"
futures = "0.3.31"
borsh = { workspace = true }
solana-pubkey = { workspace = true }
```

### Environment Variables

| Variable | Required | Purpose |
|----------|----------|---------|
| `GEYSER_URL` | Yes | Yellowstone gRPC endpoint (e.g., `https://grpc.triton.one`) |
| `GEYSER_TOKEN` | No | Authentication token for the Geyser endpoint |
| `RPC_URL` | Yes | Solana RPC endpoint for pool discovery queries |

## Data Flow

```
Yellowstone gRPC
    │
    │  Subscription::run(token_stream) using sipper pattern
    │  Filter: transactions touching TOKEN_PROGRAM or TOKEN_2022_PROGRAM
    │  Parse: scan instructions for InitializeMint (disc=0) / InitializeMint2 (disc=20)
    │
    ▼
StreamEvent::TokenCreated(TokenEvent)
    │
    ▼
App.update() → push to self.tokens vec
App.view()   → left panel table renders new row
    │
    │  User clicks a token row
    │
    ▼
Message::TokenSelected(index)
    │
    ▼
App.update() → set self.selected = Some(index)
            → return Task::perform(discover_all_pools(rpc, mint), map_to_message)
    │
    ▼
Message::PoolsDiscovered(mint, Vec<DiscoveredPool>)
    │
    ▼
App.update() → store pools in self.pools map
App.view()   → right panel table renders pool rows
```

## Module Specs

### message.rs

```rust
pub enum Message {
    Stream(StreamEvent),
    TokenSelected(usize),
    DiscoverPools(String),
    PoolsDiscovered(String, Vec<DiscoveredPool>),
    PoolDiscoveryFailed(String, String),
}

pub enum StreamEvent {
    Connected,
    Disconnected,
    TokenCreated(TokenEvent),
    Error(String),
}

pub struct TokenEvent {
    pub mint: Pubkey,
    pub decimals: u8,
    pub mint_authority: Option<Pubkey>,
    pub freeze_authority: Option<Pubkey>,
    pub token_program: Pubkey,
    pub signature: String,
    pub timestamp: std::time::Instant,
}
```

### stream.rs

Yellowstone gRPC subscription using the Iced sipper pattern.

**Connection:** `GeyserGrpcClient::build_from_shared(geyser_url)` with optional `x_token` for auth and TLS config via `tonic`.

**Subscription filter:** `SubscribeRequestFilterTransactions` with `account_include` set to `[TOKEN_PROGRAM, TOKEN_2022_PROGRAM]`. This returns all transactions that touch either token program.

**Instruction parsing:** For each transaction in the stream:
1. Iterate over all instructions (outer + inner)
2. Check if the program ID matches TOKEN_PROGRAM or TOKEN_2022_PROGRAM
3. Check if the first byte of instruction data is `0` (InitializeMint) or `20` (InitializeMint2)
4. Parse instruction data: `decimals: u8` at byte 1, `mint_authority: Pubkey` at bytes 2..34, `freeze_authority: COption<Pubkey>` at bytes 34..67
5. The mint account is the first account in the instruction's account list

**Reconnection:** On disconnect or error, exponential backoff (1s, 2s, 4s, ..., max 30s) then retry. Emit `StreamEvent::Disconnected` immediately, `StreamEvent::Connected` on successful reconnect.

**Sipper pattern:**
```rust
pub fn token_stream() -> impl Sipper<Never, StreamEvent> {
    sipper(async move |mut output| {
        loop {
            match connect_and_stream(&mut output).await {
                Ok(()) => {} // stream ended cleanly (shouldn't happen)
                Err(_) => {
                    output.send(StreamEvent::Disconnected).await;
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    })
}
```

### discovery.rs

On-demand async pool discovery. Same logic as `tests/pool_discovery.rs` but returning a simpler `DiscoveredPool` struct.

```rust
pub struct DiscoveredPool {
    pub dex: &'static str,
    pub pool_address: String,
    pub quote_mint: Pubkey,
    pub base_mint: Pubkey,
    pub price: f64,
    pub quote_balance: u64,
    pub base_balance: u64,
    pub fee_bps: u64,
}

pub async fn discover_all_pools(rpc_url: String, mint: String) -> Result<Vec<DiscoveredPool>, String>
```

Queries all 6 DEXs in parallel via `tokio::join!`:

| DEX | Method | Program ID | data_size | Memcmp offsets |
|-----|--------|-----------|-----------|----------------|
| Raydium V4 | getProgramAccounts | `675kPX...` | 752 | 400, 432 |
| Raydium CLMM | getProgramAccounts | `CAMMC...` | 1544 | 73, 105 |
| Meteora DAMM | getProgramAccounts | `Eo7Wj...` | 944, 952 | 40 |
| Meteora DAMM V2 | getProgramAccounts | `cpamd...` | 1112 | 168, 200 |
| Meteora DLMM | getProgramAccounts | `LBUZKh...` | 904 | 88 |
| Pumpfun AMM | PDA derivation | `pAMMB...` | N/A | N/A |

Each discovered account is deserialized, vault balances fetched, and the `Market` trait used to extract price and fees.

### app.rs

Iced application with Elm architecture.

**State:**
```rust
pub struct App {
    tokens: Vec<TokenEvent>,
    selected: Option<usize>,
    pools: HashMap<String, PoolState>,  // mint → pool state
    connection_status: ConnectionStatus,
    rpc_url: String,
}

pub enum PoolState {
    NotLoaded,
    Loading,
    Loaded(Vec<DiscoveredPool>),
    Error(String),
}

pub enum ConnectionStatus {
    Connecting,
    Connected,
    Disconnected,
    Error(String),
}
```

**View layout:**

```
┌──────────────────────────────┬──────────────────────────────────┐
│  Live Token Feed             │  Pool Discovery                  │
│  [scrollable table]          │  [scrollable table or status]    │
│  Mint | Dec | Program | Age  │  DEX | Pool | Price | Liq | Fee │
│  ...                         │  ...                             │
├──────────────────────────────┴──────────────────────────────────┤
│  Status: Connected (gRPC) │ 142 tokens │ RPC: https://...      │
└─────────────────────────────────────────────────────────────────┘
```

- Left panel: `table` widget with columns for mint (truncated), decimals, token program (SPL/T2022), and age (relative time since creation). Rows are clickable. New tokens appear at the top.
- Right panel: If no token selected, shows placeholder text. If selected and loading, shows spinner/text. If loaded, shows pool table with DEX name, pool address (truncated), price, quote balance, base balance, fee bps.
- Status bar: Connection status, token count, RPC endpoint.

**Subscription:**
```rust
fn subscription(&self) -> Subscription<Message> {
    Subscription::run(stream::token_stream).map(Message::Stream)
}
```

**Update:** Handles all Message variants. `TokenSelected` triggers `Task::perform(discover_all_pools(...), ...)`. `PoolsDiscovered` stores results in the pools map.

### main.rs

```rust
fn main() -> iced::Result {
    iced::application("Solana Thunder", App::update, App::view)
        .subscription(App::subscription)
        .run_with(App::new)
}
```

## References

- Raydium CLMM: https://github.com/raydium-io/raydium-clmm
- Raydium AMM: https://github.com/raydium-io/raydium-amm
- Meteora DAMM V2: https://github.com/MeteoraAg/damm-v2
- Meteora DAMM V1: https://github.com/MeteoraAg/damm-v1-sdk
- Meteora DLMM: https://github.com/MeteoraAg/dlmm-sdk
- Pumpfun bonding curve: https://solscan.io/account/6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P
- Pumpfun AMM: https://solscan.io/account/pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA
- Iced framework: https://github.com/iced-rs/iced (v0.14, websocket example for sipper pattern)
- Yellowstone gRPC: https://github.com/rpcpool/yellowstone-grpc
