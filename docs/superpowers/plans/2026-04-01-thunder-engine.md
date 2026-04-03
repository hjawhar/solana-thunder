# Thunder Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A persistent service that keeps all DEX data in memory via gRPC, pre-validates pools, and serves a Jupiter-like HTTP API with sub-50ms routing.

**Architecture:** New `crates/engine/` workspace member. AccountStore (DashMap) holds raw account data, PoolRegistry wraps pools with swappable flags, gRPC subscriber keeps everything live, axum serves /quote /swap /price.

**Tech Stack:** Rust, tokio, axum, dashmap, yellowstone-grpc-client, solana-sdk 3.0

---

## Phase 1: Account Store + Engine Skeleton

### Task 1: Create engine crate and AccountStore

**Files:**
- Create: `crates/engine/Cargo.toml`
- Create: `crates/engine/src/lib.rs`
- Create: `crates/engine/src/main.rs`
- Create: `crates/engine/src/account_store.rs`
- Modify: `Cargo.toml` (workspace members)

- [ ] **Step 1: Create crate structure**

`crates/engine/Cargo.toml`:
```toml
[package]
name = "thunder-engine"
version = "0.1.0"
edition = "2024"

[[bin]]
name = "thunder-engine"
path = "src/main.rs"

[dependencies]
thunder-core = { path = "../core" }
thunder-aggregator = { path = "../aggregator" }
solana-sdk = { workspace = true }
solana-pubkey = { workspace = true }
solana-rpc-client = "3.0"
solana-rpc-client-api = "3.0"
solana-account-decoder-client-types = "3.0"
solana-commitment-config = "3.0"
spl-associated-token-account = { workspace = true }
spl-token = { workspace = true }
borsh = { workspace = true }
serde = { workspace = true }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
dashmap = "6"
axum = "0.8"
tower-http = { version = "0.6", features = ["cors"] }
yellowstone-grpc-client = "11.0"
yellowstone-grpc-proto = { version = "11.0", features = ["plugin"] }
rustls = { version = "0.23", default-features = false, features = ["ring"] }
futures = "0.3"
dotenvy = "0.15"
```

Add `"crates/engine"` to workspace members in root `Cargo.toml`.

- [ ] **Step 2: Implement AccountStore**

`crates/engine/src/account_store.rs`:
```rust
use dashmap::DashMap;
use solana_pubkey::Pubkey;
use std::sync::atomic::{AtomicU64, Ordering};

pub struct AccountData {
    pub data: Vec<u8>,
    pub owner: Pubkey,
    pub lamports: u64,
    pub slot: u64,
}

pub struct AccountStore {
    accounts: DashMap<Pubkey, AccountData>,
    last_slot: AtomicU64,
    account_count: AtomicU64,
}

impl AccountStore {
    pub fn new() -> Self {
        Self {
            accounts: DashMap::new(),
            last_slot: AtomicU64::new(0),
            account_count: AtomicU64::new(0),
        }
    }

    pub fn upsert(&self, pubkey: Pubkey, data: Vec<u8>, owner: Pubkey, lamports: u64, slot: u64) {
        let is_new = !self.accounts.contains_key(&pubkey);
        self.accounts.insert(pubkey, AccountData { data, owner, lamports, slot });
        if is_new {
            self.account_count.fetch_add(1, Ordering::Relaxed);
        }
        let prev = self.last_slot.load(Ordering::Relaxed);
        if slot > prev {
            self.last_slot.store(slot, Ordering::Relaxed);
        }
    }

    pub fn get_data(&self, pubkey: &Pubkey) -> Option<Vec<u8>> {
        self.accounts.get(pubkey).map(|v| v.data.clone())
    }

    pub fn get(&self, pubkey: &Pubkey) -> Option<dashmap::mapref::one::Ref<Pubkey, AccountData>> {
        self.accounts.get(pubkey)
    }

    pub fn contains(&self, pubkey: &Pubkey) -> bool {
        self.accounts.contains_key(pubkey)
    }

    pub fn read_token_balance(&self, pubkey: &Pubkey) -> u64 {
        self.accounts.get(pubkey)
            .filter(|v| v.data.len() >= 72)
            .map(|v| u64::from_le_bytes(v.data[64..72].try_into().unwrap()))
            .unwrap_or(0)
    }

    pub fn last_slot(&self) -> u64 {
        self.last_slot.load(Ordering::Relaxed)
    }

    pub fn len(&self) -> u64 {
        self.account_count.load(Ordering::Relaxed)
    }
}
```

- [ ] **Step 3: Minimal main.rs and lib.rs**

`crates/engine/src/lib.rs`:
```rust
pub mod account_store;
```

`crates/engine/src/main.rs`:
```rust
use thunder_engine::account_store::AccountStore;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    println!("Thunder Engine starting...");
    let store = AccountStore::new();
    println!("AccountStore ready ({} accounts)", store.len());
}
```

- [ ] **Step 4: Verify compilation**

```bash
cargo check -p thunder-engine
```

---

### Task 2: Pool Registry with swappable flags

**Files:**
- Create: `crates/engine/src/pool_registry.rs`
- Modify: `crates/engine/src/lib.rs`

- [ ] **Step 1: Implement PoolRegistry**

The PoolRegistry wraps the existing PoolIndex and adds per-pool auxiliary data (tick arrays, bitmap extensions, swappable flag). It loads from the existing cache and validates each pool against the AccountStore.

Key types:
```rust
pub struct PoolInfo {
    pub address: String,
    pub dex_name: String,
    pub market: Box<dyn Market>,
    pub swappable: bool,
    pub quote_mint: Pubkey,
    pub base_mint: Pubkey,
    pub quote_vault: Pubkey,
    pub base_vault: Pubkey,
    pub tick_arrays: Vec<Pubkey>,     // CLMM only
    pub bitmap_ext: Option<Pubkey>,   // DLMM only
}
```

The registry:
- Loads pools from the existing `cache::load_cache` or `loader::PoolLoader`
- For each pool, extracts metadata (mints, vaults) and stores in PoolInfo
- Builds the adjacency graph (same as PoolIndex)
- Validates swappable flag against AccountStore

- [ ] **Step 2: Implement validation logic**

For each DEX, check the required accounts exist in the AccountStore:
- DLMM: derive bin_array PDA from active_id, check it exists. Check bitmap_ext if needed.
- CLMM: check tick_arrays Vec is non-empty (populated during cold start).
- DAMM V1: check `is_active()` and vault balances > 0.
- DAMM V2: check `is_active()` and vault balances > 0.
- Raydium V4: check vault balances > 0.
- Pumpfun: always swappable.

Vault balance checked by reading from AccountStore, not from the cached pool struct.

- [ ] **Step 3: Re-validate on account updates**

Add method `on_account_update(pubkey, owner)` that:
- If `owner` is a DEX program → it's a pool account → re-deserialize, update PoolInfo
- If `owner` is Token Program → it's a vault → find which pool owns this vault → re-check vault balance
- If `owner` is CLMM program and it's a tick array → find which pool it belongs to → update tick_arrays
- In all cases → re-evaluate `swappable` flag

---

### Task 3: Cold start — batch-fetch auxiliary accounts

**Files:**
- Create: `crates/engine/src/cold_start.rs`

- [ ] **Step 1: Implement batch vault fetching**

After loading pools, collect all vault pubkeys (2 per pool = ~4M), batch-fetch via `getMultipleAccounts` (100 per batch, 20 concurrent), store in AccountStore. Reuse the existing `batch_fetch_balances` pattern from the loader but store full account data, not just balances.

- [ ] **Step 2: Implement tick array fetching for top CLMM pools**

Sort CLMM pools by vault balance (liquidity proxy). For the top 10,000:
- Use `getProgramAccounts` with memcmp on pool address at offset 8
- Store tick array account data in AccountStore
- Record tick array pubkeys in the pool's PoolInfo.tick_arrays

Parallelize across pools using `futures::join_all` with concurrency limit (50 at a time).

- [ ] **Step 3: Fetch DLMM bitmap extensions**

One `getProgramAccounts` call with dataSize=12488 for the DLMM program. Store all 43 accounts in AccountStore. Parse `lb_pair` from offset 8 and set `bitmap_ext` on the matching PoolInfo.

- [ ] **Step 4: Validate all pools**

Iterate all pools, run the validation check, set `swappable` flag. Log stats:
```
Pools: 2,029,628 total, 847,293 swappable
  Raydium CLMM: 170K total, 9,834 swappable
  Meteora DLMM: 140K total, 138,291 swappable
  ...
```

---

## Phase 2: gRPC Streaming

### Task 4: Yellowstone gRPC subscriber

**Files:**
- Create: `crates/engine/src/streaming.rs`

- [ ] **Step 1: Implement gRPC connection and subscription**

Connect to Yellowstone gRPC using the existing helper pattern from `tests/helpers/mod.rs`. Subscribe to:
- Account updates owned by all 6 DEX programs (pool changes)
- Account updates for specific vault pubkeys (balance changes)

The vault subscription list is built from the loaded pools' vault addresses.

- [ ] **Step 2: Process stream events**

For each `SubscribeUpdate` with an `AccountUpdate`:
1. Extract pubkey, data, owner, slot
2. Call `account_store.upsert(pubkey, data, owner, slot)`
3. Call `pool_registry.on_account_update(pubkey, owner)` to re-validate affected pools

Run the stream processing in a dedicated tokio task.

- [ ] **Step 3: Handle reconnection**

If the gRPC stream disconnects, log the error and reconnect with exponential backoff (1s, 2s, 4s, max 30s). On reconnect, re-subscribe with the same filters.

---

## Phase 3: HTTP API

### Task 5: Axum server with /quote, /swap, /price, /health

**Files:**
- Create: `crates/engine/src/api.rs`
- Modify: `crates/engine/src/main.rs`

- [ ] **Step 1: Implement GET /quote**

Parameters: `inputMint`, `outputMint`, `amount`, `slippageBps` (default 50).

Handler:
1. Parse mints from query params (accept base58 or "SOL" shorthand)
2. Run the router over the PoolRegistry (only swappable pools)
3. Return JSON:
```json
{
  "inputMint": "So11111111111111111111111111111111111111112",
  "outputMint": "6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN",
  "amount": "100000000",
  "slippageBps": 50,
  "routes": [
    {
      "hops": [
        {
          "poolAddress": "...",
          "dexName": "Meteora DLMM",
          "inputMint": "...",
          "outputMint": "...",
          "inputAmount": "100000000",
          "outputAmount": "3340539",
          "priceImpactBps": 0
        }
      ],
      "outputAmount": "3340539",
      "priceImpactBps": 0
    }
  ],
  "timeTakenMs": 12
}
```

- [ ] **Step 2: Implement POST /swap**

Body: `{ "route": <route from /quote>, "userPublicKey": "<base58>" }`.

Handler:
1. Parse the route and user pubkey
2. For each hop, read pool data + auxiliary accounts from AccountStore
3. Build swap instructions via `swap_builder`
4. Compile into VersionedTransaction (unsigned)
5. Return base64-encoded transaction:
```json
{
  "transaction": "<base64>",
  "txSizeBytes": 713
}
```

The client signs and sends.

- [ ] **Step 3: Implement GET /price**

Parameter: `mint`.

Handler:
1. Find the best direct pool pairing mint with WSOL (highest liquidity from AccountStore balances)
2. Get SOL/USD from CLMM sqrt_price (read from AccountStore)
3. Return:
```json
{
  "mint": "...",
  "priceSol": 0.012,
  "priceUsd": 1.05
}
```

- [ ] **Step 4: Implement GET /health**

Return:
```json
{
  "status": "ok",
  "pools": 2029628,
  "swappablePools": 847293,
  "accountsInStore": 4500000,
  "lastSlot": 410500000,
  "uptimeSeconds": 3600
}
```

- [ ] **Step 5: Wire up main.rs with full startup sequence**

1. Load .env
2. Load pools from cache or RPC
3. Build PoolRegistry
4. Cold start: batch-fetch vaults, tick arrays, bitmap extensions
5. Validate all pools
6. Start gRPC subscriber (background task)
7. Start axum server on port 8080 (or PORT env var)
8. Log ready message with stats

---

## Phase 4: Router + Swap Builder Upgrades

### Task 6: Router swappable filter

**Files:**
- Modify: `crates/aggregator/src/router.rs`

- [ ] **Step 1: Add swappable check to simulate_hop**

The engine passes a `swappable_set: &HashSet<String>` (pool addresses that are swappable) to the router. In `simulate_hop`, check if the pool is in the set before proceeding. This is a minimal change — one `if` check at the top of the function.

For backward compatibility, if no swappable_set is provided (None), all pools are considered swappable (existing behavior for the CLI and tests).

### Task 7: Swap builder from AccountStore

**Files:**
- Modify: `crates/aggregator/src/swap_builder.rs`

- [ ] **Step 1: Add variants that read from AccountStore**

Add `build_dlmm_swap_from_store` and `build_clmm_swap_from_store` functions that take an `&AccountStore` and a pool address, read the pool data + auxiliary accounts from the store, and build the swap instruction. These complement the existing functions (which take explicit account structs) — the engine uses the `_from_store` variants, integration tests continue using the explicit variants.

---

## Testing

### Task 8: Integration test

**Files:**
- Create: `tests/engine_api.rs`

- [ ] **Step 1: Test /quote endpoint**

Start the engine in a test, wait for ready, call `GET /quote?inputMint=SOL&outputMint=<TRUMP>&amount=100000000`. Assert the response has at least one route with non-zero output. Assert response time < 200ms (relaxed for test, production target is <50ms).

- [ ] **Step 2: Test /health endpoint**

Call `GET /health`, assert pools > 0, swappablePools > 0, accountsInStore > 0.

---

## Run Commands

```bash
# Start the engine
GEYSER_ENDPOINT="wss://..." RPC_URL="https://..." cargo run --release -p thunder-engine

# Test quote API
curl "http://localhost:8080/quote?inputMint=SOL&outputMint=6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN&amount=100000000"

# Test swap API
curl -X POST "http://localhost:8080/swap" -H "Content-Type: application/json" \
  -d '{"route": <paste route from /quote>, "userPublicKey": "B36Y6Pr5..."}'

# Test price API
curl "http://localhost:8080/price?mint=6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN"
```
