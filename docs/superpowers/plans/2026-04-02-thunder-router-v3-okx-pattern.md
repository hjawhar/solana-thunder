# Thunder Router V3 — OKX-Pattern Adapter Architecture

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rewrite all thunder-router adapters following OKX's battle-tested, audited pattern: uniform 4-account prefix (dex_program, authority, source_token, dest_token) across all DEXs, proper `invoke` for first hop and balance-delta chaining, correct writable/signer/readonly flags matching each DEX program's expectations.

**Architecture:** Every adapter receives accounts in a fixed layout: `[0] dex_program, [1] swap_authority (user), [2] swap_source_token, [3] swap_destination_token, [4+] DEX-specific accounts`. The router's main loop reads balances from accounts[2] and [3] (source/dest), dispatches to the adapter which builds the CPI, invokes, and the router verifies output balance delta. This matches OKX's audited implementation exactly.

**Tech Stack:** Rust, `solana-program` 2.2, borsh. No Anchor.

---

## Key Insight from OKX: Uniform Account Prefix

Every OKX adapter expects:
```
[0]  dex_program_id        — validated against expected program ID
[1]  swap_authority_pubkey  — user wallet (signer on hop 0) or PDA (intermediate hops)
[2]  swap_source_token      — SPL token account (input), for balance tracking
[3]  swap_destination_token  — SPL token account (output), for balance tracking
[4+] DEX-specific accounts
```

Balance reads happen on accounts [2] and [3] BEFORE and AFTER the CPI. The delta on [3] is the actual output amount chained to the next hop.

The CPI AccountMeta list is built per-DEX, ordering accounts as the DEX program expects. The `swap_authority` may appear at different positions in the CPI metas (e.g., index 17 for Raydium V4, index 0 for CLMM), but it's always sourced from accounts[1].

---

## Per-DEX Account Layouts (matching OKX exactly)

### DAMM V1 — 16 accounts
```
[0]  dex_program_id              — Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB
[1]  swap_authority_pubkey       — user (signer)
[2]  swap_source_token           — user's input ATA
[3]  swap_destination_token      — user's output ATA
[4]  pool
[5]  a_vault
[6]  b_vault
[7]  a_token_vault
[8]  b_token_vault
[9]  a_vault_lp_mint
[10] b_vault_lp_mint
[11] a_vault_lp
[12] b_vault_lp
[13] admin_token_fee
[14] vault_program               — 24Uqj9JCLxUeoC3hGfh5W3s9FM9uCHqSim67FNPDFSms
[15] token_program               — TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA
```
CPI: 15 metas (pool, src, dst, vaults..., authority as signer, vault_prog, token_prog)

### DAMM V2 — 16 accounts
```
[0]  dex_program_id              — cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG
[1]  swap_authority_pubkey       — user (signer)
[2]  swap_source_token           — user's input ATA
[3]  swap_destination_token      — user's output ATA
[4]  pool_authority              — HLnpSz9h2S4hiLQ43rnSD9XkcUThA7B8hQMKmDaiTLcC
[5]  pool
[6]  input_token_account         — same as swap_source_token (or may differ in proxy mode)
[7]  output_token_account        — same as swap_destination_token
[8]  token_a_vault
[9]  token_b_vault
[10] token_a_mint
[11] token_b_mint
[12] token_a_program
[13] token_b_program
[14] referral_token_account      — dex_program_id (sentinel for no referral)
[15] event_authority
```
CPI: 14 metas (pool_auth, pool, input, output, vaults, mints, authority, token_progs, referral, event_auth, program)

### DLMM Swap2 — 19 accounts
```
[0]  dex_program_id              — LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo
[1]  swap_authority_pubkey       — user (signer)
[2]  swap_source_token           — user's input ATA
[3]  swap_destination_token      — user's output ATA
[4]  lb_pair
[5]  bin_array_bitmap_extension
[6]  reserve_x
[7]  reserve_y
[8]  token_x_mint
[9]  token_y_mint
[10] oracle
[11] host_fee_in
[12] token_x_program
[13] token_y_program
[14] memo_program
[15] event_authority
[16] bin_array0
[17] bin_array1                   — ZERO_ADDRESS if unused
[18] bin_array2                   — ZERO_ADDRESS if unused
```
CPI: 17-19 metas (lb_pair, bitmap, reserves, src, dst, mints, oracle, host_fee, authority, token_progs, memo, event_auth, program, bin_arrays)
Note: bin_array1/bin_array2 conditionally included only if key != ZERO_ADDRESS

### Raydium CLMM V2 — 18 accounts
```
[0]  dex_program_id              — CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK
[1]  swap_authority_pubkey       — user (signer)
[2]  swap_source_token           — user's input ATA
[3]  swap_destination_token      — user's output ATA
[4]  amm_config_id
[5]  pool_id
[6]  input_vault
[7]  output_vault
[8]  observation_id
[9]  token_program               — SPL Token
[10] token_program_2022          — Token-2022
[11] memo_program
[12] input_vault_mint
[13] output_vault_mint
[14] ex_bitmap                    — tick array bitmap extension
[15] tick_array0
[16] tick_array1                  — ZERO_ADDRESS if unused
[17] tick_array2                  — ZERO_ADDRESS if unused
```
CPI: 15-17 metas (authority, amm_config, pool, src, dst, vaults, observation, token_progs, memo, mints, bitmap, tick_arrays)

### Raydium AMM V4 — 19 accounts
```
[0]  dex_program_id              — 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8
[1]  swap_authority_pubkey       — user (signer)
[2]  swap_source_token           — user's input ATA
[3]  swap_destination_token      — user's output ATA
[4]  token_program
[5]  amm_id
[6]  amm_authority
[7]  amm_open_orders
[8]  amm_target_orders
[9]  pool_coin_token_account
[10] pool_pc_token_account
[11] serum_program_id
[12] serum_market
[13] serum_bids
[14] serum_asks
[15] serum_event_queue
[16] serum_coin_vault_account
[17] serum_pc_vault_account
[18] serum_vault_signer
```
CPI: 18 metas (token_prog, amm_id..serum_vault_signer, src, dst, authority)

### Pumpfun AMM — 14 accounts (buy and sell share layout)
```
[0]  dex_program_id              — pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA
[1]  swap_authority_pubkey       — user (signer)
[2]  swap_source_token           — user's input ATA
[3]  swap_destination_token      — user's output ATA
[4]  pool
[5]  global_config
[6]  base_mint
[7]  quote_mint
[8]  pool_base_token_account
[9]  pool_quote_token_account
[10] protocol_fee_recipient
[11] protocol_fee_recipient_token_account
[12] base_token_program
[13] quote_token_program
```

Actually, OKX's Pumpfun AMM adapter (pumpfunamm.rs sell3) has 21 accounts including system_program, associated_token_program, event_authority, coin_creator_vault_ata, coin_creator_vault_authority, fee_config, fee_program. This is more accounts than our current implementation. Let me simplify for our needs — use the accounts that match what the Pumpfun AMM program actually requires.

---

## Tasks

### Task 1: Rewrite all adapter modules to match OKX pattern

Rewrite every adapter with:
1. The uniform [0-3] prefix: dex_program, authority, source, dest
2. Correct CPI metas matching each DEX's on-chain expectations (writable/readonly/signer exactly right)
3. Balance reads on accounts[3] (destination) for output measurement
4. Proper invoke() call

### Task 2: Rewrite router lib.rs to use uniform account pattern

The router's hop loop uses the uniform prefix to read balances:
```rust
let source = &accounts[2];  // always
let dest = &accounts[3];    // always
let balance_before = read_token_balance(dest);
// ... adapter does CPI ...
let balance_after = read_token_balance(dest);
current_amount = balance_after - balance_before;
```

### Task 3: Rewrite engine account collectors to match new layouts

Each `collect_*_accounts` function must produce accounts in the new OKX-pattern order.

### Task 4: Update surfpool test account collection

### Task 5: Build, deploy, test on Surfpool
