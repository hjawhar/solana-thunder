# Solana Thunder Roadmap

Prioritized by impact for a production trading system. Each phase builds on the previous.

---

## Phase 1: Make All DEX Adapters Work Reliably

**Goal:** Every supported DEX produces successful swaps through the router on Surfpool.

### 1.1 Fix CLMM and V4 adapter CPI flags
- **Problem:** Raydium CLMM and V4 routes fail with `PrivilegeEscalation` or `Program failed to complete`. The engine's AccountMeta writable/readonly flags don't match what the DEX programs expect (same bug we fixed for DLMM's oracle/host_fee).
- **Fix:** For each DEX, simulate a transaction on Surfpool, read full program logs, identify which accounts need writable vs readonly, update the engine's `collect_*_accounts` and surfpool test's equivalents.
- **Files:** `crates/engine/src/swap.rs`, `tests/surfpool_swap.rs`
- **Verify:** Run surfpool_swap test with routes through each DEX type.

### 1.2 Raydium V4 real Serum/OpenBook accounts
- **Problem:** V4 adapter uses pool address as placeholder for all Serum accounts (amm_open_orders, serum_market, serum_bids, serum_asks, serum_event_queue, serum_coin_vault, serum_pc_vault, serum_vault_signer). Real V4 pools need actual accounts.
- **Fix:** Parse V4 pool data to extract open_orders, target_orders, and Serum market accounts. The V4 pool struct has these at known offsets. For pools without OpenBook markets (newer ones), the current placeholder approach may work.
- **Files:** `crates/engine/src/swap.rs` (`collect_ray_v4_accounts`), `tests/surfpool_swap.rs`
- **Verify:** V4 single-hop and multi-hop swaps succeed on Surfpool.

### 1.3 Pumpfun buy/sell direction validation
- **Problem:** Pumpfun buy instruction is natively exact-output (`{max_sol_in, exact_tokens_out}`), sell is exact-input. The adapter needs to handle both correctly, with proper swap_source/swap_destination mapping based on direction.
- **Fix:** Verify buy and sell paths produce correct CPI instructions. Test with SOL->PumpfunToken (buy) and PumpfunToken->SOL (sell).
- **Files:** `crates/router-program/src/adapters/pumpfun.rs`, `crates/engine/src/swap.rs`
- **Verify:** Pumpfun routes succeed on Surfpool in both directions.

---

## Phase 2: Engine Production Readiness

**Goal:** The engine's `/swap` endpoint produces transactions that land on mainnet.

### 2.1 Engine ALT support
- **Problem:** The surfpool test creates ALTs for transaction size reduction, but the engine's `/swap` passes empty `[]` for lookup tables. Without ALTs, 3-hop routes exceed the 1232-byte transaction limit.
- **Fix:** The engine should pre-create an ALT with common DEX program IDs, authorities, token programs, and common mints. Cache the ALT address. Pass the ALT when compiling V0 messages.
- **Files:** `crates/engine/src/swap.rs`, `crates/engine/src/api.rs`, possibly new `crates/engine/src/alt.rs`
- **Verify:** `/swap` returns transactions using the ALT. 3-hop routes fit within 1232 bytes.

### 2.2 Compute budget instructions
- **Problem:** No compute budget instructions in the transaction. Without them, the default 200K CU limit may be too low for complex routes, and there's no priority fee for fast landing.
- **Fix:** Add `SetComputeUnitLimit` (300K for 2-hop, 400K for 3-hop) and `SetComputeUnitPrice` as the first two instructions. Accept `priorityFee` parameter in SwapParams.
- **Files:** `crates/engine/src/swap.rs`, `crates/engine/src/api.rs`
- **Verify:** Transactions include compute budget instructions. CU limit matches hop count.

### 2.3 Pre-send simulation
- **Problem:** The engine returns unsigned transactions without checking if they'll succeed. Bad routes (disabled pools, stale data) produce transactions that fail on-chain, wasting fees.
- **Fix:** Before returning the transaction, call `simulateTransaction` on the RPC. If simulation fails, try the next-best route. Return the first route that simulates successfully.
- **Files:** `crates/engine/src/api.rs` (handle_swap)
- **Verify:** `/swap` only returns transactions that simulate successfully.

### 2.4 Live data slippage accuracy
- **Problem:** Slippage calculations use cached pool simulation amounts, which may be hours stale. On Surfpool, this caused 2-hop routes to fail slippage checks even at 500 bps because simulated output (2.73 TRUMP) was 100x different from actual (0.028 TRUMP).
- **Fix:** The engine already has live data via gRPC streaming + AccountStore. Ensure the router uses `calculate_output_live` with fresh AccountStore data for slippage calculation. The cold start must complete before `/swap` produces reliable results.
- **Files:** `crates/engine/src/api.rs`, verify `crates/aggregator/src/router.rs` uses live data
- **Verify:** Start engine, wait for cold start, test `/swap` — simulated output should match actual on-chain output within slippage tolerance.

---

## Phase 3: Expand DEX Coverage

**Goal:** Support the highest-volume Solana DEXs beyond the initial 6.

### 3.1 Orca Whirlpool
- Pool loader: `getProgramAccounts` with Whirlpool program ID (`whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc`)
- DEX crate: `crates/whirlpool/` — parse WhirlpoolPool struct, compute swap output with tick-based concentrated liquidity
- Router adapter: `crates/router-program/src/adapters/whirlpool.rs`
- DexType variant: add `Whirlpool` to the enum
- Reference: OKX adapter at `adapters/whirlpool.rs` (15KB)

### 3.2 Raydium CPMM
- Pool loader: `getProgramAccounts` with CPMM program ID (`CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C`)
- DEX crate: may share with raydium-clmm or be a new `crates/raydium-cpmm/`
- Router adapter: add to `adapters/raydium_cpmm.rs`
- Reference: OKX adapter in `adapters/raydium.rs` (swap_cpmm function)

### 3.3 Phoenix
- Orderbook DEX with distinct account structure
- Reference: OKX adapter at `adapters/phoenix.rs` (8KB)

### 3.4 OpenBook V2
- Serum-successor orderbook
- Reference: OKX adapter at `adapters/openbookv2.rs` (12KB)

### 3.5 Lifinity V2
- Oracle-based AMM
- Reference: OKX adapter at `adapters/lifinity.rs` (7KB)

For each new DEX, the work is:
1. Create pool loader in `crates/aggregator/src/loader.rs` (add DexConfig entry + build function)
2. Create DEX crate under `crates/` with Market trait implementation
3. Add router adapter under `crates/router-program/src/adapters/`
4. Add DexType variant to router lib.rs and engine swap.rs
5. Add account collector in engine swap.rs
6. Test on Surfpool

---

## Phase 4: Performance and MEV Protection

**Goal:** Minimize latency and protect against MEV.

### 4.1 Jito bundle support
- Build and submit transactions as Jito bundles instead of raw RPC
- Protects against sandwich attacks on multi-hop routes
- Add Jito tip instruction to the transaction
- **Files:** New `crates/engine/src/jito.rs`, modify api.rs

### 4.2 Route optimization
- The router currently tries routes sequentially. Parallelize route simulation.
- Score routes by: expected output, CU cost, transaction size, pool reliability
- Implement split routing (e.g., 60% through route A, 40% through route B) for large orders

### 4.3 CU optimization
- Profile each adapter's CU consumption on Surfpool
- Minimize allocations in the on-chain program (fixed-size arrays instead of Vec where possible)
- Consider removing msg!() logs in production build (saves ~100 CU per log)

### 4.4 Transaction size optimization
- Pre-populate ALTs with the most commonly used pool accounts (top 1000 pools by volume)
- Dynamic ALT creation per-route for maximum compression
- Investigate using V0 message with multiple ALTs

---

## Phase 5: Mainnet Deployment

**Goal:** Deploy and operate on mainnet.

### 5.1 Router program deployment
- Deploy thunder-router.so to mainnet
- Set up program upgrade authority (multisig recommended)
- Fund program account for rent exemption
- Update ROUTER_PROGRAM_ID in engine swap.rs and surfpool test

### 5.2 Project structure split
- Move router-program to its own repository (deploys independently)
- The pool/engine code stays in solana-thunder
- Router program version tagged and pinned

### 5.3 Monitoring and alerting
- Track swap success/failure rates per DEX
- Monitor CU consumption trends
- Alert on pool status changes (disabled pools in routes)
- Dashboard for route quality metrics

### 5.4 Security audit
- Audit the router program (CPI patterns, balance reads, slippage enforcement)
- Review account validation (program ID checks, signer verification)
- Fuzz testing with adversarial inputs
