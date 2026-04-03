# Thunder Router-Only Multi-Hop Swaps

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route ALL multi-hop swaps through the on-chain thunder-router program, remove the direct per-DEX instruction path, support all 6 DEXs, and test end-to-end on Surfpool.

**Architecture:** Single on-chain program (`thunder-router`) receives a serialized `ExecuteRouteArgs` containing per-hop instruction data. For each hop, it CPIs into the target DEX, reads actual token balance deltas to chain exact amounts, and enforces slippage on the final output. The engine builds this single instruction with all hop data and accounts. Surfpool test deploys the router BPF binary and executes real multi-hop swaps against forked mainnet state.

**Tech Stack:** Rust, Solana BPF (solana-program 2.2), borsh, Surfpool, cargo-build-sbf

---

## Constraints

| Constraint | Value | Impact |
|---|---|---|
| Transaction size | 1232 bytes | Limits 2-hop to ~30 unique accounts without ALTs |
| CPI depth | 4 levels | Router → DEX → Token = 3, fits fine |
| Compute budget | 1.4M CU max | 2-hop ~100K CU, 3-hop ~170K, well within limits |
| ALTs | Compress keys only, not ix data | Essential for 3+ hop routes |

## File Structure

| File | Responsibility |
|---|---|
| `crates/router-program/src/lib.rs` | On-chain BPF program (already exists, no changes needed) |
| `crates/engine/src/swap.rs` | Transaction assembly — router path only |
| `crates/engine/src/api.rs` | HTTP API — remove `use_router` flag |
| `crates/aggregator/src/swap_builder.rs` | Per-DEX instruction builders (add DAMM V1 `from_pool_data`) |
| `tests/surfpool_swap.rs` | Full end-to-end: deploy router to Surfpool, execute multi-hop swaps |

---

### Task 1: Add `build_damm_v1_swap_from_pool_data` to swap_builder

The engine's `build_hop_instruction` needs to support DAMM V1 pools. Currently there is `build_damm_v1_swap` (takes explicit accounts struct) but no `from_pool_data` variant that parses raw on-chain bytes. This is required because the router path builds all hop instructions from pool data in the AccountStore.

**Files:**
- Modify: `crates/aggregator/src/swap_builder.rs`

- [ ] **Step 1: Identify DAMM V1 pool data layout**

Read the existing `build_damm_v1_swap` function and the `DammV1SwapAccounts` struct to understand what accounts are needed. Then check the DAMM V1 pool deserialization in `crates/meteora-damm/src/models.rs` and `crates/meteora-damm/src/lib.rs` to find the byte offsets for each field in the raw on-chain data.

The DAMM V1 pool struct (`MeteoraDAMMPool`) has an 8-byte Anchor discriminator, then fields. We need to extract: `a_vault`, `b_vault`, `a_token_vault`, `b_token_vault`, `a_vault_lp_mint`, `b_vault_lp_mint`, `a_vault_lp`, `b_vault_lp`, `protocol_token_fee` (protocol fee account — PDA derived from pool + mint), `token_a_mint`, `token_b_mint`, and the token program.

- [ ] **Step 2: Implement `build_damm_v1_swap_from_pool_data`**

Add the function after the existing `build_damm_v1_swap`. It should:
1. Parse the pool account bytes to extract all needed pubkeys
2. Determine swap direction from `input_mint` vs `token_a_mint`/`token_b_mint`
3. Delegate to `build_damm_v1_swap` with the extracted accounts

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p thunder-aggregator`

---

### Task 2: Simplify engine swap.rs — router path only

Remove the direct per-DEX path entirely. The `build_swap_transaction` function always builds a single router instruction. Remove the `use_router` parameter, `build_direct_swap_instructions`, and the `exact_output` parameter from `build_hop_instruction` (the router chains via balance reads, not exact-output flags).

**Files:**
- Modify: `crates/engine/src/swap.rs`
- Modify: `crates/engine/src/api.rs`

- [ ] **Step 1: Remove direct path and simplify `build_swap_transaction`**

Remove:
- `build_direct_swap_instructions` function
- `use_router` parameter from `build_swap_transaction`
- The `if use_router { ... } else { ... }` conditional — keep only the router branch

The function becomes:
```rust
pub fn build_swap_transaction(
    route: &Route,
    user: &Pubkey,
    amount_in: u64,
    slippage_bps: u64,
    store: &AccountStore,
    registry: &PoolRegistry,
    recent_blockhash: Hash,
) -> Result<VersionedTransaction, GenericError> {
    // ... pre-instructions (WSOL wrap, ATA creation) ...
    let router_ix = build_router_instruction(
        route, user, amount_in, min_amount_out, store, registry, &mint_programs, &tp,
    )?;
    ixs.push(router_ix);
    // ... post-instructions (WSOL unwrap) ...
}
```

- [ ] **Step 2: Remove `exact_output` from `build_hop_instruction`**

The router always uses exact-input instructions (it patches amounts at runtime via balance reads). Remove the `exact_output` parameter from `build_hop_instruction` and pass `false` to any swap_builder functions that have it. This also means passing `false` to `build_clmm_swap_from_pool_data` and `build_ray_v4_swap_from_pool_data`.

- [ ] **Step 3: Add DAMM V1 arm to `build_hop_instruction`**

Add a match arm for `"Meteora DAMM V1"` that calls the new `build_damm_v1_swap_from_pool_data`. Keep the existing catch-all `s if s.starts_with("Meteora DAMM")` arm for DAMM V2 as fallback.

Actually, fix the match to be explicit:
```rust
"Meteora DAMM V1" => { swap_builder::build_damm_v1_swap_from_pool_data(...) }
"Meteora DAMM V2" => { swap_builder::build_damm_v2_swap_from_pool_data(...) }
```

- [ ] **Step 4: Remove `use_router` from API**

In `crates/engine/src/api.rs`:
- Remove `use_router: bool` field from `SwapParams`
- Remove `params.use_router` from the `build_swap_transaction` call

- [ ] **Step 5: Verify compilation**

Run: `cargo check`

---

### Task 3: Rewrite Surfpool test for router-based swaps

The current `tests/surfpool_swap.rs` builds direct per-DEX instructions and does NOT deploy or use the router. Rewrite it to:
1. Build the router BPF binary via `cargo-build-sbf`
2. Deploy it to Surfpool
3. Use the engine's `build_swap_transaction` (now router-only) to build transactions
4. Sign and execute on Surfpool
5. Verify token balance changes

**Files:**
- Modify: `tests/surfpool_swap.rs`

- [ ] **Step 1: Build the router BPF program**

Before running the test, build the router program:
```bash
cd crates/router-program && cargo build-sbf
```

This produces `crates/router-program/target/deploy/thunder_router.so`.

- [ ] **Step 2: Rewrite the surfpool test**

The test should:
1. Start by deploying `thunder_router.so` to Surfpool using `solana program deploy` or BPF loader instructions
2. Load pool index from cache
3. Find routes via `Router::find_routes`
4. For each viable route, use `thunder_engine::swap::build_swap_transaction` to build the transaction (which now always uses the router)
5. Sign and send to Surfpool
6. On success, print balance diffs

Key changes from current test:
- Remove the manual `build_hop()` function (only supported DLMM + CLMM)
- Use the engine's `build_swap_transaction` which supports all 6 DEXs via the router
- Add router program deployment step
- The test needs `thunder-engine` as a dev-dependency to call `build_swap_transaction`

- [ ] **Step 3: Add `thunder-engine` dev-dependency**

In root `Cargo.toml` dev-dependencies, `thunder-engine` is already listed. Verify the test can import `thunder_engine::swap::build_swap_transaction`.

- [ ] **Step 4: Test on Surfpool**

```bash
# Terminal 1: Start Surfpool
source .env
surfpool start --rpc-url "$RPC_URL" --no-tui --no-deploy --no-studio \
  --airdrop $(solana address) --airdrop-amount 100000000000 &

# Terminal 2: Deploy router and run test
cd crates/router-program && cargo build-sbf && cd ../..
solana -u http://127.0.0.1:8899 program deploy \
  crates/router-program/target/deploy/thunder_router.so \
  --program-id crates/router-program/target/deploy/thunder_router-keypair.json

INPUT=SOL OUTPUT=6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN AMOUNT=0.1 MAX_HOPS=2 \
  cargo test --release --test surfpool_swap -- --nocapture
```

---

### Task 4: Update simulate_swap test

Update `tests/simulate_swap.rs` to work with the router-only swap path (no `useRouter` flag needed — it's always router now).

**Files:**
- Modify: `tests/simulate_swap.rs`

- [ ] **Step 1: Remove any `useRouter` references from the test**

The test calls `POST /swap` on the engine. Since `use_router` is removed from SwapParams, no changes needed to the request body. Just verify the test still compiles and the print statements make sense.

- [ ] **Step 2: Verify test compiles**

Run: `cargo test --no-run --test simulate_swap`

---

### Task 5: Final verification

- [ ] **Step 1: Full workspace compilation**

```bash
cargo check
```

- [ ] **Step 2: Unit tests**

```bash
cargo test -p thunder-aggregator
```

- [ ] **Step 3: Release build**

```bash
cargo build --release --bin thunder-engine
```

- [ ] **Step 4: Surfpool end-to-end test**

Deploy router to Surfpool and run the swap test (as described in Task 3 Step 4).
