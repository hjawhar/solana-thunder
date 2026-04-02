# Thunder Engine — Design Spec

## Goal

A persistent service (`thunder-engine`) that keeps all pool and account data in memory via gRPC streaming, pre-validates which pools can execute swaps, and serves a Jupiter-like HTTP API (`/quote`, `/swap`, `/price`) with sub-50ms response times.

## Problem Statement

Current swap pipeline takes 40-65 seconds because:
1. **No live data**: tick arrays, bitmap extensions, and vault balances fetched per-swap from RPC
2. **No pool validation**: router picks pools that look good by estimate but 90% can't execute (missing bitmap extensions, no tick arrays), wasting time on trial-and-error
3. **One-shot architecture**: every run loads pools from cache, pre-fetches accounts, tries routes — there's no persistent state

Jupiter solves this by running a persistent service with all account data in memory, updated in real-time. Routing returns pre-validated results instantly.

## Architecture

```
                                  Yellowstone gRPC
                                       │
                          ┌────────────▼────────────┐
                          │     thunder-engine       │
                          │                          │
                          │  ┌──────────────────┐   │
                          │  │  Account Store    │   │ ← HashMap<Pubkey, Vec<u8>>
                          │  │  (all raw data)   │   │    pools + vaults + tick arrays + bitmap exts
                          │  └────────┬─────────┘   │
                          │           │              │
                          │  ┌────────▼─────────┐   │
                          │  │  Pool Registry    │   │ ← deserialized Markets + swappable flags
                          │  │  (2M pools)       │   │    knows which pools have required accounts
                          │  └────────┬─────────┘   │
                          │           │              │
                          │  ┌────────▼─────────┐   │
                          │  │  Router           │   │ ← only routes through swappable pools
                          │  │  (Bellman-Ford)   │   │    combined route+quote in one pass
                          │  └────────┬─────────┘   │
                          │           │              │
                          │  ┌────────▼─────────┐   │
                          │  │  Swap Builder     │   │ ← builds instructions from in-memory data
                          │  │  (all DEXs)       │   │    no RPC calls needed
                          │  └──────────────────┘   │
                          │                          │
                          │  HTTP API (axum)         │
                          │  GET  /quote             │
                          │  POST /swap              │
                          │  GET  /price             │
                          │  GET  /health            │
                          └──────────┬───────────────┘
                                     │
                              Clients (CLI, bots, UI)
```

## Components

### 1. Account Store (`account_store.rs`)

A `DashMap<Pubkey, AccountData>` (concurrent HashMap) holding raw account data for every account the engine needs.

```rust
pub struct AccountData {
    pub data: Vec<u8>,
    pub owner: Pubkey,
    pub lamports: u64,
    pub slot: u64,        // slot when last updated
}

pub struct AccountStore {
    accounts: DashMap<Pubkey, AccountData>,
}
```

**Population strategy** (two phases):

**Phase 1: Cold start** — On startup, load pools from the existing cache/RPC loader. For each pool, derive the accounts it needs (vaults, tick arrays, bitmap extensions, oracle PDAs) and batch-fetch them via `getMultipleAccounts`. Store everything in the DashMap.

**Phase 2: Live streaming** — Subscribe to Yellowstone gRPC with `accounts` filter for all 6 DEX program IDs. Every account update arrives as a stream event and updates the DashMap. This keeps pools, vaults, and tick arrays fresh in real-time.

**Memory budget**: ~5-8GB on the 256GB server.
- 2M pools × ~1KB = 2GB
- 4M vaults × 165B = 660MB
- Selective tick arrays (top 10K CLMM pools × 3 arrays × 10KB) = 300MB
- 43 bitmap extensions × 12.5KB = negligible
- Working overhead = ~2GB

**Key design decision**: Don't load ALL 3.4M tick arrays. Only load tick arrays for CLMM pools that have significant liquidity. The top 10K CLMM pools cover >99% of routing volume. Pools without tick arrays in the store are marked un-swappable.

### 2. Pool Registry (`pool_registry.rs`)

Wraps the existing `PoolIndex` with per-pool validation flags.

```rust
pub struct PoolInfo {
    pub market: Box<dyn Market>,
    pub dex_name: String,
    pub swappable: bool,           // can this pool actually execute a swap?
    pub cached_data: Vec<u8>,      // for disk cache
    // DEX-specific auxiliary data
    pub tick_arrays: Vec<Pubkey>,  // CLMM: pre-resolved tick array addresses
    pub bitmap_ext: Option<Pubkey>, // DLMM: bitmap extension address
}
```

**Validation rules** (checked on load + on every account update):

| DEX | Swappable when |
|---|---|
| DLMM | active bin's tick array exists in AccountStore, bitmap ext exists if required |
| CLMM | ≥1 tick array exists in AccountStore, pool has liquidity > 0 |
| DAMM V1 | `pool.enabled == true`, vault balances > 0 |
| DAMM V2 | `pool.pool_status == 1`, vault balances > 0 |
| Raydium V4 | vault balances > 0 |
| Pumpfun | always (bonding curve, virtual reserves) |

When a gRPC account update arrives, the registry:
1. Updates the pool's deserialized state
2. Re-checks swappable flag
3. If a tick array or bitmap extension account is created/destroyed, updates the related pool's flag

### 3. Router (upgraded, `router.rs`)

Same graph structure but two critical changes:

**a) Only route through swappable pools**: `simulate_hop` checks `pool_info.swappable` first. This eliminates 90%+ of failed routes instantly.

**b) Read live vault balances from AccountStore**: Instead of using cached balances from the pool struct, `calculate_output` reads the vault balance from the AccountStore at route time. This gives accurate output estimates that reflect the current on-chain state.

The existing BFS + hub-first + bidirectional strategy stays. The Bellman-Ford optimization (combined route+quote) is a future improvement — the immediate win is eliminating invalid pools.

### 4. Swap Builder (upgraded, `swap_builder.rs`)

Same instruction builders but reads ALL account data from the AccountStore instead of RPC:

- DLMM: reads `active_id` from stored pool data, gets bitmap extension from PoolInfo, resolves bin array PDA
- CLMM: reads `tick_current` from stored pool data, gets tick arrays from PoolInfo
- All: reads vault balances from stored vault accounts

**Zero RPC calls at swap time.** Everything is in memory.

### 5. HTTP API (`api.rs`)

Axum server with 4 endpoints:

**`GET /quote?inputMint=<>&outputMint=<>&amount=<>&slippage=<>`**
- Runs the router over the live PoolRegistry
- Returns: `{ routes: [...], bestRoute: { hops, expectedOutput, priceImpact } }`
- Target: <50ms

**`POST /swap` with body `{ route, userPublicKey }`**
- Takes a route from /quote, builds the versioned transaction
- Reads all accounts from AccountStore (tick arrays, bitmap exts, vaults)
- Returns: `{ transaction: <base64 serialized VersionedTransaction> }`
- Target: <10ms

**`GET /price?mint=<>`**
- Returns SOL and USD price from live pool data
- Target: <5ms

**`GET /health`**
- Returns: pool count, account count, gRPC lag (current slot vs last update slot), uptime

### 6. gRPC Subscriber (`streaming.rs`)

Subscribes to Yellowstone gRPC for:

1. **Account updates** for all 6 DEX program IDs — receives pool account changes in real-time
2. **Account updates** for vault token accounts — receives balance changes
3. **Account updates** for CLMM tick array accounts — tracks tick array creation/deletion

On each update:
1. Store raw data in AccountStore
2. If it's a pool account, re-deserialize and update PoolRegistry
3. If it's a vault/tick-array/bitmap-ext, find the related pool and re-validate its swappable flag

**Subscription filters**:
```
accounts: {
    "pools": { owner: [DLMM, CLMM, DAMM_V1, DAMM_V2, PUMPFUN] },
    "vaults": { account: [... all vault pubkeys from loaded pools ...] },
}
```

For vaults, the initial subscription includes all vault pubkeys from the loaded pools. As new pools are created (detected via pool account updates), their vaults are added to the subscription.

## File Structure

```
crates/engine/
  Cargo.toml
  src/
    main.rs              # Engine binary entry point
    lib.rs               # Public API for library use
    account_store.rs     # DashMap-based in-memory account store
    pool_registry.rs     # Pool index + validation flags + Market wrappers
    streaming.rs         # Yellowstone gRPC subscriber
    api.rs               # Axum HTTP server (/quote, /swap, /price, /health)
```

Modifications to existing crates:
- `crates/aggregator/src/router.rs` — add `swappable` check to `simulate_hop`, accept `AccountStore` for live balance reads
- `crates/aggregator/src/swap_builder.rs` — add functions that read account data from `AccountStore` instead of raw bytes

## Dependencies

```toml
# New for engine
axum = "0.8"
tokio = { version = "1", features = ["full"] }
dashmap = "6"
serde_json = "1"

# Existing (move from dev-deps to deps)
yellowstone-grpc-client = "11.0"
yellowstone-grpc-proto = { version = "11.0", features = ["plugin"] }
rustls = { version = "0.23", default-features = false, features = ["ring"] }
```

## Startup Sequence

1. Load pools from cache (6s) or RPC (4 min)
2. Build PoolRegistry with initial swappable=false for all
3. Batch-fetch all vault accounts via `getMultipleAccounts` → store in AccountStore
4. For top CLMM pools (by liquidity), fetch tick arrays via `getProgramAccounts` → store
5. Fetch all 43 DLMM bitmap extensions → store
6. Re-validate all pools → mark swappable
7. Connect gRPC, start streaming updates
8. Start HTTP API server
9. Log: "Engine ready, N pools (M swappable), serving on :8080"

Cold start target: <30s (most time is step 3-4). After that, everything is live.

## Performance Targets

| Operation | Target | How |
|---|---|---|
| /quote | <50ms | In-memory router over swappable pools only |
| /swap | <10ms | Build instruction from in-memory accounts |
| /price | <5ms | Read from live pool data |
| gRPC update → store | <1ms | DashMap insert |
| Account update → pool re-validation | <1ms | Check existence in DashMap |

## Not In Scope (Future)

- Split routing (splitting across multiple pools for same hop)
- Bellman-Ford replacement for router (current BFS is fast enough with swappable filtering)
- MEV protection / Jito bundles
- WebSocket streaming of price updates
- Multi-chain support
