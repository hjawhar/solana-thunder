# Thunder Router V2 — On-Chain DEX Instruction Building

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rewrite the thunder-router on-chain program so it builds DEX CPI instructions internally (like Jupiter V6 and OKX DEX Router), instead of receiving pre-built instructions from the client. The client sends a compact route plan; the program does the rest.

**Architecture:** The client sends a single instruction containing `{ amount_in, min_amount_out, hops: Vec<SwapHop> }` where each `SwapHop` is just `{ dex_type: u8, num_accounts: u8 }` (~2 bytes per hop instead of ~100+ bytes). All DEX program accounts are passed as `remaining_accounts`. The on-chain program iterates hops: for each, it slices the right accounts from `remaining_accounts`, builds the DEX-specific CPI instruction data (discriminator + amount + min_out), invokes, reads the output token balance delta, and chains it to the next hop. Slippage is enforced on the final output.

**Tech Stack:** Rust, `solana-program` 2.2, borsh, no Anchor (raw `process_instruction` entrypoint for minimal CU overhead)

---

## Reference Architecture: OKX DEX Router (open source)

Repository: https://github.com/okxlabs/DEX-Router-Solana-V1

### Key patterns from OKX (Anchor-based, 30+ DEXs):

1. **Dex enum** — each DEX variant is a serialized enum tag in instruction data
2. **Per-DEX adapter modules** — each module knows how to:
   - Parse its accounts from `remaining_accounts[offset..offset+N]`
   - Build the CPI `Instruction` (discriminator + args + AccountMeta list)
   - Invoke and check results
3. **`distribute_swap()` function** — giant match on Dex enum → calls the right adapter
4. **`invoke_process()` function** — reads before/after balances, calls CPI, validates output
5. **Program-owned authority PDA** — for intermediate hop transfers (proxy swap pattern)
6. **Two-level split routing** — amounts[] + routes[][] for parallel DEX splits
7. **Each adapter**: `parse_accounts` → `build_instruction` → `invoke` → `post_swap_check`

### Key patterns from Jupiter V6:

1. **SharedAccountsRoute instruction** — program-owned intermediate token accounts
2. **RoutePlanStep** — `{ swap: SwapParams(enum), percent: u8, input_index: u8, output_index: u8 }`
3. **Only 46 bytes of instruction data** for a 2-hop swap
4. **36 accounts** total with ALTs compressing transaction to ~400-600 bytes
5. **131K CU** for a 2-hop Whirlpool → custom DEX swap

---

## Design Decisions for Thunder Router V2

### 1. No Anchor — raw `solana-program` entrypoint

**Why:** Anchor adds ~30K CU overhead for deserialization and account validation. For a router where every microsecond matters, we use raw `process_instruction` with manual borsh deserialization. This matches our current approach and is the right choice for speed.

### 2. Compact SwapHop — dex_type + num_accounts only

```rust
#[derive(BorshSerialize, BorshDeserialize)]
pub enum DexType {
    MeteoraDAMMV1,     // 0
    MeteoraDAMMV2,     // 1
    MeteoraDLMM,       // 2
    MeteoraDLMMV1,     // 3 (older swap, no bitmap ext)
    RaydiumCLMM,       // 4
    RaydiumAMMV4,      // 5
    PumpfunBuy,        // 6
    PumpfunSell,       // 7
}

#[derive(BorshSerialize, BorshDeserialize)]
pub struct SwapHop {
    pub dex_type: DexType,
    pub num_accounts: u8,
}

#[derive(BorshSerialize, BorshDeserialize)]
pub struct ExecuteRouteArgs {
    pub amount_in: u64,
    pub min_amount_out: u64,
    pub hops: Vec<SwapHop>,
}
```

**Instruction data size**: 8 (disc) + 8 (amount_in) + 8 (min_out) + 4 (vec len) + N × 2 (hops) = ~30 bytes for a 2-hop route. Compare to current ~250+ bytes.

### 3. Per-DEX CPI builders inside the program

Each DEX module is a Rust function that:
1. Takes `remaining_accounts[offset..offset+num_accounts]`, `amount_in`, `user` pubkey
2. Knows the exact account layout (which index is pool, vault, user_in, user_out, etc.)
3. Builds the CPI instruction data (discriminator + amount + min_out with `min_out = 1` for intermediate hops)
4. Calls `invoke()` or `invoke_signed()`
5. Returns the actual output amount (from balance delta)

```rust
// Example: Meteora DAMM V2 adapter
fn swap_damm_v2(
    accounts: &[AccountInfo],
    amount_in: u64,
    is_intermediate: bool,
) -> ProgramResult<u64> {
    // accounts[0] = pool_authority (readonly)
    // accounts[1] = pool (writable)
    // accounts[2] = user_token_in (writable)
    // accounts[3] = user_token_out (writable)
    // accounts[4] = token_a_vault (writable)
    // accounts[5] = token_b_vault (writable)
    // accounts[6] = token_a_mint (readonly)
    // accounts[7] = token_b_mint (readonly)
    // accounts[8] = user/payer (signer, writable)
    // accounts[9] = token_a_program (readonly)
    // accounts[10] = token_b_program (readonly)
    // accounts[11] = dex_program (readonly) — also used as program_id for CPI
    // accounts[12] = event_authority (readonly)
    // accounts[13] = dex_program (readonly) — self-reference for Anchor

    let min_out: u64 = if is_intermediate { 1 } else { 0 }; // 0 = unused, router enforces final
    let mut data = Vec::with_capacity(24);
    data.extend_from_slice(&DAMM_V2_SWAP_DISC);
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&min_out.to_le_bytes());

    // Read output balance before
    let out_balance_before = read_token_balance(&accounts[3]);

    // Build AccountMetas and invoke
    let ix = Instruction { program_id: *accounts[11].key, accounts: build_metas(accounts), data };
    invoke(&ix, accounts)?;

    // Read output balance after
    let out_balance_after = read_token_balance(&accounts[3]);
    Ok(out_balance_after.saturating_sub(out_balance_before))
}
```

### 4. Account ordering convention

The client MUST pass accounts in a fixed order per DEX type. The program trusts the ordering and indexes directly (no searching). This is how both Jupiter and OKX do it — the off-chain route builder is responsible for correct account ordering.

**Convention for all DEXs:** The first 2 non-program accounts after the DEX-specific accounts are always:
- `user_token_in` (the source token account for this hop)
- `user_token_out` (the destination token account for this hop)

Actually, looking at OKX's pattern, each DEX adapter defines its own account order. We'll do the same — each adapter module has a fixed, documented account layout.

### 5. No program-owned intermediate accounts (simpler than Jupiter)

Jupiter uses program-owned PDA token accounts for intermediate hops. This is complex (requires creating/managing program ATAs, PDA signing).

We use the **user's own ATAs** for intermediate tokens (same as current design). The client creates intermediate ATAs as pre-instructions. This is simpler and avoids PDA account management on-chain.

### 6. Balance-delta chaining (same as current router)

After each CPI hop, we read the output token account's SPL balance (bytes 64-72) and compute the delta. This becomes the input for the next hop. Same proven pattern as the current router.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/router-program/src/lib.rs` | Entrypoint, ExecuteRouteArgs deserialization, hop loop, slippage check |
| `crates/router-program/src/adapters/mod.rs` | Module index for all DEX adapters |
| `crates/router-program/src/adapters/meteora_damm_v1.rs` | DAMM V1 CPI builder |
| `crates/router-program/src/adapters/meteora_damm_v2.rs` | DAMM V2 CPI builder |
| `crates/router-program/src/adapters/meteora_dlmm.rs` | DLMM Swap2 CPI builder |
| `crates/router-program/src/adapters/raydium_clmm.rs` | Raydium CLMM SwapV2 CPI builder |
| `crates/router-program/src/adapters/raydium_v4.rs` | Raydium AMM V4 CPI builder |
| `crates/router-program/src/adapters/pumpfun.rs` | Pumpfun AMM Buy/Sell CPI builder |
| `crates/router-program/src/adapters/common.rs` | Shared: `read_token_balance`, `build_account_metas` |
| `crates/engine/src/swap.rs` | Client-side: compact route plan builder (replaces current) |
| `tests/surfpool_swap.rs` | End-to-end test on Surfpool |

---

## Per-DEX Account Layouts

These are the account orderings that each adapter expects. The client must pass accounts in exactly this order.

### Meteora DAMM V2 (14 accounts)
```
[0]  pool_authority          (readonly)   — static: HLnpSz9h2S4hiLQ43rnSD9XkcUThA7B8hQMKmDaiTLcC
[1]  pool                    (writable)   — from pool data
[2]  user_token_in           (writable)   — user's ATA for input mint
[3]  user_token_out          (writable)   — user's ATA for output mint  ← OUTPUT BALANCE READ
[4]  token_a_vault           (writable)   — from pool data
[5]  token_b_vault           (writable)   — from pool data
[6]  token_a_mint            (readonly)   — from pool data
[7]  token_b_mint            (readonly)   — from pool data
[8]  user/payer              (signer)     — user wallet
[9]  token_a_program         (readonly)   — Token or Token-2022
[10] token_b_program         (readonly)   — Token or Token-2022
[11] dex_program             (readonly)   — cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG
[12] event_authority         (readonly)   — PDA derived from dex program
[13] dex_program             (readonly)   — self-reference (Anchor pattern)
```
Instruction data: `disc(8) + amount_in(8) + min_out(8)` = 24 bytes

### Meteora DLMM Swap2 (17 accounts)
```
[0]  pool                    (writable)   — lb_pair
[1]  bitmap_extension        (readonly)   — or program ID if not needed
[2]  reserve_x               (writable)   — from pool data
[3]  reserve_y               (writable)   — from pool data
[4]  user_token_in           (writable)
[5]  user_token_out          (writable)   ← OUTPUT BALANCE READ
[6]  token_x_mint            (readonly)   — from pool data
[7]  token_y_mint            (readonly)   — from pool data
[8]  oracle                  (writable)   — PDA: seeds=[b"oracle", pool]
[9]  host_fee                (readonly)   — program ID (None)
[10] user                    (signer)
[11] token_x_program         (readonly)
[12] token_y_program         (readonly)
[13] memo_program            (readonly)   — MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr
[14] event_authority         (readonly)   — D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6
[15] dex_program             (readonly)   — LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo
[16] bin_array               (writable)   — PDA: seeds=[b"bin_array", pool, index]
```
Instruction data: `disc(8) + amount_in(8) + min_out(8) + empty_vec(4)` = 28 bytes

### Raydium CLMM SwapV2 (13 + tick_arrays)
```
[0]  user                    (signer)
[1]  amm_config              (readonly)   — from pool data
[2]  pool                    (writable)
[3]  user_token_in           (writable)
[4]  user_token_out          (writable)   ← OUTPUT BALANCE READ
[5]  input_vault             (writable)   — from pool data
[6]  output_vault            (writable)   — from pool data
[7]  observation             (writable)   — from pool data
[8]  input_token_program     (readonly)
[9]  output_token_program    (readonly)
[10] memo_program            (readonly)   — MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr
[11] input_mint              (readonly)
[12] output_mint             (readonly)
[13..] tick_arrays           (writable)   — variable count (typically 3)
```
Instruction data: `disc(8) + amount(8) + threshold(8) + sqrt_price_limit(16) + is_base_input(1)` = 41 bytes

### Raydium AMM V4 (17 accounts)
```
[0]  token_program           (readonly)   — TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA
[1]  pool/amm_id             (writable)
[2]  amm_authority           (readonly)   — 5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1
[3]  amm_open_orders         (writable)   — from pool data (or pool as placeholder)
[4]  pool_coin_vault         (writable)   — base_vault from pool data
[5]  pool_pc_vault           (writable)   — quote_vault from pool data
[6-13] amm accounts          (writable)   — pool address repeated (placeholder)
[14] user_token_in           (writable)
[15] user_token_out          (writable)   ← OUTPUT BALANCE READ
[16] user                    (signer)
```
Instruction data: `disc(1) + amount_in(8) + min_out(8)` = 17 bytes

### Pumpfun AMM Buy (13 accounts)
```
[0]  pool                    (writable)
[1]  user                    (signer)
[2]  base_mint               (writable)
[3]  quote_mint              (writable)   — WSOL
[4]  user_base_token         (writable)   ← OUTPUT BALANCE READ (buy)
[5]  user_quote_token        (writable)   — user's WSOL ATA
[6]  pool_base_vault         (writable)
[7]  pool_quote_vault        (writable)
[8]  base_token_program      (readonly)
[9]  quote_token_program     (readonly)
[10] system_program          (readonly)
[11] event_authority         (readonly)   — PDA from pumpfun program
[12] dex_program             (readonly)   — pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA
```
Instruction data: `disc(8) + amount_in(8) + min_out(8)` = 24 bytes
Buy: `disc = [102, 6, 61, 18, 1, 218, 235, 234]`, Sell: `disc = [51, 230, 133, 164, 1, 127, 131, 173]`

### Meteora DAMM V1 (14 accounts)
```
[0]  pool                    (writable)
[1]  a_vault                 (writable)
[2]  b_vault                 (writable)
[3]  a_token_vault           (writable)   — PDA from vault program
[4]  b_token_vault           (writable)   — PDA from vault program
[5]  a_vault_lp_mint         (writable)   — PDA from vault program
[6]  b_vault_lp_mint         (writable)   — PDA from vault program
[7]  a_vault_lp              (writable)
[8]  b_vault_lp              (writable)
[9]  protocol_token_fee      (writable)
[10] user_token_in           (writable)
[11] user_token_out          (writable)   ← OUTPUT BALANCE READ
[12] user                    (signer)
[13] token_program           (readonly)
```
Instruction data: `disc(8) + amount_in(8) + min_out(8)` = 24 bytes

---

## Tasks

### Task 1: Create adapter modules for all 6 DEXs

**Files:**
- Create: `crates/router-program/src/adapters/mod.rs`
- Create: `crates/router-program/src/adapters/common.rs`
- Create: `crates/router-program/src/adapters/meteora_damm_v1.rs`
- Create: `crates/router-program/src/adapters/meteora_damm_v2.rs`
- Create: `crates/router-program/src/adapters/meteora_dlmm.rs`
- Create: `crates/router-program/src/adapters/raydium_clmm.rs`
- Create: `crates/router-program/src/adapters/raydium_v4.rs`
- Create: `crates/router-program/src/adapters/pumpfun.rs`

Each adapter module exports a single `pub fn swap(accounts: &[AccountInfo], amount_in: u64) -> Result<u64, ProgramError>` function that:
1. Validates account count matches expected
2. Reads output token balance BEFORE
3. Builds CPI instruction data (discriminator + amount_in + min_out=1)
4. Builds AccountMeta list from the accounts slice
5. Calls `invoke()`
6. Reads output token balance AFTER
7. Returns the delta (actual output amount)

The `common.rs` module provides `read_token_balance(account: &AccountInfo) -> u64` (reads bytes 64-72 of SPL token account data).

### Task 2: Rewrite router program lib.rs

**Files:**
- Modify: `crates/router-program/src/lib.rs`

The new `process_instruction`:
1. Deserialize `ExecuteRouteArgs { amount_in, min_amount_out, hops: Vec<SwapHop> }` from instruction data
2. Iterate hops, maintaining `current_amount` (starts at `amount_in`)
3. For each hop: slice `remaining_accounts[offset..offset+hop.num_accounts]`, dispatch to the right adapter based on `hop.dex_type`, get actual output
4. After all hops: verify `current_amount >= min_amount_out`
5. Log final output

```rust
pub fn process_instruction(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let args = ExecuteRouteArgs::try_from_slice(instruction_data)
        .map_err(|_| ProgramError::InvalidInstructionData)?;

    let mut remaining = accounts;
    let mut current_amount = args.amount_in;

    for (i, hop) in args.hops.iter().enumerate() {
        let n = hop.num_accounts as usize;
        let (hop_accounts, rest) = remaining.split_at(n);
        remaining = rest;

        current_amount = match hop.dex_type {
            DexType::MeteoraDAMMV1 => adapters::meteora_damm_v1::swap(hop_accounts, current_amount)?,
            DexType::MeteoraDAMMV2 => adapters::meteora_damm_v2::swap(hop_accounts, current_amount)?,
            DexType::MeteoraDLMM => adapters::meteora_dlmm::swap(hop_accounts, current_amount)?,
            DexType::MeteoraDLMMV1 => adapters::meteora_dlmm::swap_v1(hop_accounts, current_amount)?,
            DexType::RaydiumCLMM => adapters::raydium_clmm::swap(hop_accounts, current_amount)?,
            DexType::RaydiumAMMV4 => adapters::raydium_v4::swap(hop_accounts, current_amount)?,
            DexType::PumpfunBuy => adapters::pumpfun::buy(hop_accounts, current_amount)?,
            DexType::PumpfunSell => adapters::pumpfun::sell(hop_accounts, current_amount)?,
        };
    }

    if current_amount < args.min_amount_out {
        msg!("Slippage exceeded: got {}, minimum {}", current_amount, args.min_amount_out);
        return Err(ProgramError::Custom(1));
    }
    msg!("Route complete: output={}", current_amount);
    Ok(())
}
```

### Task 3: Rewrite engine swap.rs — compact route plan builder

**Files:**
- Modify: `crates/engine/src/swap.rs`

Replace `build_router_instruction` to emit the compact format. Instead of building full DEX instructions client-side, just:
1. Determine `DexType` from the hop's `dex_name`
2. Determine `num_accounts` for that DEX type
3. Collect the right accounts in the right order for each hop (using pool data from AccountStore)
4. Serialize `ExecuteRouteArgs { amount_in, min_amount_out, hops }` — just ~30 bytes
5. Return the Instruction with all accounts flat

The key change is that the engine no longer calls `swap_builder::build_*_from_pool_data()` to create DEX instructions. Instead, it just collects accounts in the correct order and maps the dex_name to a DexType enum variant.

This requires new functions like `collect_damm_v2_accounts(pool_data, user, user_in, user_out, ...) -> Vec<AccountMeta>` that know the account layout per DEX.

### Task 4: Build, deploy, and test on Surfpool

**Files:**
- Modify: `tests/surfpool_swap.rs` (update to use compact format)

Steps:
1. `cargo build-sbf` in `crates/router-program`
2. Deploy to Surfpool
3. Update surfpool test to build compact route plan
4. Run end-to-end swap test

### Task 5: Performance optimization

- Add `ComputeBudget::set_compute_unit_limit` instruction (request 200K CU for 2-hop, 300K for 3-hop)
- Add `ComputeBudget::set_compute_unit_price` instruction (for priority fees)
- Measure CU consumption per DEX adapter
- Minimize allocations in the on-chain program (use fixed-size buffers where possible)
- Profile transaction size with and without ALTs

---

## Transaction Size Comparison

### Current router (pre-built instructions)
```
2-hop DAMM V2 → DLMM:
  Instruction data: ~250 bytes (ExecuteRouteArgs with full DEX ix data)
  Total tx: ~1041 bytes (with ALT)
```

### New router (compact route plan)
```
2-hop DAMM V2 → DLMM:
  Instruction data: ~32 bytes (8 disc + 8 amount + 8 min_out + 4 vec_len + 2×2 hops)
  Savings: ~218 bytes in instruction data alone
  Estimated total tx: ~820 bytes (with ALT)
```

This leaves ~412 bytes of headroom for 3-hop routes or routes with more accounts (Raydium V4).

---

## CU Budget Comparison

### Current router
- Borsh deserialization of full instruction data: ~5-10K CU
- Each hop: clone instruction data + patch amounts: ~2K CU
- CPI invoke per hop: ~1K CU base + DEX cost

### New router
- Borsh deserialization of compact args: ~1-2K CU (much smaller data)
- Each hop: build instruction data inline (~24-41 bytes): ~500 CU
- CPI invoke per hop: ~1K CU base + DEX cost
- **Estimated savings: ~5-10K CU per transaction**
