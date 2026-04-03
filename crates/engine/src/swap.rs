//! Transaction assembly: builds a complete unsigned VersionedTransaction.
//!
//! All swaps use the **router** path: a single compact `ExecuteRouteArgs`
//! instruction targeting the thunder-router on-chain program. The engine only
//! collects accounts in adapter order; the router program CPIs into each DEX,
//! reads balances, and chains actual output amounts between hops.

use borsh::BorshSerialize;
use solana_pubkey::Pubkey;
use solana_sdk::{
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    message::{v0, VersionedMessage},
    transaction::VersionedTransaction,
};
use solana_system_interface::instruction as system_ix;
use spl_associated_token_account::{
    get_associated_token_address_with_program_id,
    instruction::create_associated_token_account_idempotent,
};
use spl_token::instruction::{close_account, sync_native};
use thunder_aggregator::types::Route;
use thunder_core::{calculate_min_amount_out, GenericError, WSOL, TOKEN_PROGRAM, TOKEN_PROGRAM_2022};

use crate::account_store::AccountStore;
use crate::pool_registry::PoolRegistry;

// ---------------------------------------------------------------------------
// Router instruction types (mirrors crates/router-program/src/lib.rs V2)
//
// Duplicated here to avoid cross-crate dependency conflicts between
// solana-program 2.x (router-program) and solana-sdk 3.x (engine).
// ---------------------------------------------------------------------------

#[derive(BorshSerialize)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
enum DexType {
    MeteoraDAMMV1 = 0,
    MeteoraDAMMV2 = 1,
    MeteoraDLMM = 2,
    RaydiumCLMM = 3,
    RaydiumAMMV4 = 4,
    PumpfunBuy = 5,
    PumpfunSell = 6,
}

#[derive(BorshSerialize)]
struct SwapHop {
    dex_type: DexType,
    num_accounts: u8,
}

#[derive(BorshSerialize)]
struct ExecuteRouteArgs {
    amount_in: u64,
    min_amount_out: u64,
    hops: Vec<SwapHop>,
}

// ---------------------------------------------------------------------------
// Program / authority constants
// ---------------------------------------------------------------------------

const ROUTER_PROGRAM_ID: &str = "7WgM9BLWicvmxZwNsT5AUKqxsf6QqBSy2RxeEEwjzJFu";
const DAMM_V1_PROGRAM: &str = "Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB";
const DAMM_V2_PROGRAM: &str = "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG";
const DAMM_V2_POOL_AUTHORITY: &str = "HLnpSz9h2S4hiLQ43rnSD9XkcUThA7B8hQMKmDaiTLcC";
const DLMM_PROGRAM: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";
const DLMM_EVENT_AUTHORITY: &str = "D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6";
const CLMM_PROGRAM: &str = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";
const RAY_V4_PROGRAM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
const RAY_V4_AUTHORITY: &str = "5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1";
const PUMPFUN_PROGRAM: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";
const MEMO_PROGRAM: &str = "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr";
const VAULT_PROGRAM: &str = "24Uqj9JCLxUeoC3hGfh5W3s9FM9uCHqSim67FNPDFSms";

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build a complete unsigned VersionedTransaction for a swap route.
///
/// The transaction includes:
/// 1. ATA creation (idempotent) for each intermediate + output mint
/// 2. WSOL wrap if input is SOL
/// 3. Single compact router instruction (accounts only, no per-DEX ix data)
/// 4. WSOL unwrap if input was SOL
pub fn build_swap_transaction(
    route: &Route,
    user: &Pubkey,
    amount_in: u64,
    slippage_bps: u64,
    store: &AccountStore,
    registry: &PoolRegistry,
    recent_blockhash: Hash,
) -> Result<VersionedTransaction, GenericError> {
    if route.hops.is_empty() {
        return Err("Route has no hops".into());
    }

    let wsol = Pubkey::from_str_const(WSOL);
    let tp = Pubkey::from_str_const(TOKEN_PROGRAM);
    let input_mint = route.hops.first().unwrap().input_mint;
    let input_is_sol = input_mint == wsol;

    // Detect token programs for all mints.
    let mut all_mints: Vec<Pubkey> = Vec::new();
    for hop in &route.hops {
        if !all_mints.contains(&hop.input_mint) { all_mints.push(hop.input_mint); }
        if !all_mints.contains(&hop.output_mint) { all_mints.push(hop.output_mint); }
    }
    let mint_programs = detect_mint_programs(store, &all_mints);

    let mut ixs: Vec<Instruction> = Vec::new();

    // --- Pre-instructions ---

    // WSOL wrap.
    if input_is_sol {
        let wsol_ata = get_associated_token_address_with_program_id(user, &wsol, &tp);
        ixs.push(create_associated_token_account_idempotent(user, user, &wsol, &tp));
        ixs.push(system_ix::transfer(user, &wsol_ata, amount_in));
        ixs.push(sync_native(&tp, &wsol_ata).map_err(|e| format!("sync_native: {e}"))?);
    }

    // ATA creation for intermediate and output mints.
    for hop in &route.hops {
        let prog = mint_program(&mint_programs, &hop.output_mint, &tp);
        ixs.push(create_associated_token_account_idempotent(user, user, &hop.output_mint, &prog));
    }

    // --- Build compact router instruction ---

    let min_amount_out = calculate_min_amount_out(route.output_amount, slippage_bps);

    let (hops, all_accounts) = build_router_hops(
        route, user, store, registry, &mint_programs, &tp,
    )?;

    let args = ExecuteRouteArgs {
        amount_in,
        min_amount_out,
        hops,
    };
    let data = borsh::to_vec(&args).map_err(|e| format!("borsh serialize: {e}"))?;

    ixs.push(Instruction {
        program_id: Pubkey::from_str_const(ROUTER_PROGRAM_ID),
        accounts: all_accounts,
        data,
    });

    // --- Post-instructions ---

    if input_is_sol {
        let wsol_ata = get_associated_token_address_with_program_id(user, &wsol, &tp);
        ixs.push(
            close_account(&tp, &wsol_ata, user, user, &[])
                .map_err(|e| format!("close_account: {e}"))?,
        );
    }

    // Build unsigned versioned transaction.
    let message = v0::Message::try_compile(user, &ixs, &[], recent_blockhash)
        .map_err(|e| format!("compile message: {e}"))?;

    Ok(VersionedTransaction {
        signatures: vec![solana_sdk::signature::Signature::default()],
        message: VersionedMessage::V0(message),
    })
}

// ---------------------------------------------------------------------------
// Router hop builder
// ---------------------------------------------------------------------------

/// Iterate route hops, collect per-adapter accounts, and produce the compact
/// hop descriptors the router program expects.
fn build_router_hops(
    route: &Route,
    user: &Pubkey,
    store: &AccountStore,
    registry: &PoolRegistry,
    mint_programs: &[(Pubkey, Pubkey)],
    tp: &Pubkey,
) -> Result<(Vec<SwapHop>, Vec<AccountMeta>), GenericError> {
    let mut hops = Vec::new();
    let mut all_accounts = Vec::new();

    for hop in &route.hops {
        let pool_pubkey = hop.pool_address.parse::<Pubkey>()
            .map_err(|e| format!("invalid pool address {}: {e}", hop.pool_address))?;
        let pool_data = store.get_data(&pool_pubkey)
            .ok_or_else(|| format!("no pool data for {}", pool_pubkey))?;

        let in_prog = mint_program(mint_programs, &hop.input_mint, tp);
        let out_prog = mint_program(mint_programs, &hop.output_mint, tp);
        let user_in = get_associated_token_address_with_program_id(user, &hop.input_mint, &in_prog);
        let user_out = get_associated_token_address_with_program_id(user, &hop.output_mint, &out_prog);

        let (dex_type, accounts) = match hop.dex_name.as_str() {
            "Meteora DAMM V2" => {
                let metas = collect_damm_v2_accounts(
                    &pool_data, pool_pubkey, *user, user_in, user_out, &hop.input_mint,
                );
                (DexType::MeteoraDAMMV2, metas)
            }
            "Meteora DAMM V1" => {
                let metas = collect_damm_v1_accounts(
                    &pool_data, pool_pubkey, *user, user_in, user_out, &hop.input_mint,
                );
                (DexType::MeteoraDAMMV1, metas)
            }
            "Meteora DLMM" => {
                let bitmap_ext = registry.get_pool(&pool_pubkey.to_string())
                    .and_then(|info| info.bitmap_ext);
                let metas = collect_dlmm_accounts(
                    &pool_data, pool_pubkey, *user, user_in, user_out,
                    &hop.input_mint, in_prog, out_prog, bitmap_ext,
                );
                (DexType::MeteoraDLMM, metas)
            }
            "Raydium CLMM" => {
                let tick_arrays = registry.get_pool(&pool_pubkey.to_string())
                    .map(|info| info.tick_arrays.clone())
                    .unwrap_or_default();
                let metas = collect_clmm_accounts(
                    &pool_data, pool_pubkey, *user, user_in, user_out,
                    &hop.input_mint, in_prog, out_prog, tick_arrays,
                );
                (DexType::RaydiumCLMM, metas)
            }
            "Raydium AMM V4" => {
                let metas = collect_ray_v4_accounts(
                    &pool_data, pool_pubkey, *user, user_in, user_out,
                );
                (DexType::RaydiumAMMV4, metas)
            }
            "Pumpfun AMM" => {
                let quote_mint = pubkey_at(&pool_data, 75);
                let is_buy = hop.input_mint == quote_mint;
                let dex = if is_buy { DexType::PumpfunBuy } else { DexType::PumpfunSell };
                let metas = collect_pumpfun_accounts(
                    &pool_data, pool_pubkey, *user, user_in, user_out,
                    &hop.input_mint, in_prog, out_prog,
                );
                (dex, metas)
            }
            other => return Err(format!("unsupported DEX: {other}").into()),
        };

        hops.push(SwapHop {
            dex_type,
            num_accounts: accounts.len() as u8,
        });
        all_accounts.extend(accounts);
    }

    Ok((hops, all_accounts))
}

// ---------------------------------------------------------------------------
// Per-DEX account collection
// ---------------------------------------------------------------------------

/// DAMM V2 — 13 accounts.
fn collect_damm_v2_accounts(
    pool_data: &[u8],
    pool_pubkey: Pubkey,
    user: Pubkey,
    user_token_in: Pubkey,
    user_token_out: Pubkey,
    _input_mint: &Pubkey,
) -> Vec<AccountMeta> {
    let dex = Pubkey::from_str_const(DAMM_V2_PROGRAM);
    let pool_authority = Pubkey::from_str_const(DAMM_V2_POOL_AUTHORITY);
    let (event_auth, _) = Pubkey::find_program_address(&[b"__event_authority"], &dex);

    let token_a_mint = pubkey_at(pool_data, 168);
    let token_b_mint = pubkey_at(pool_data, 200);
    let token_a_vault = pubkey_at(pool_data, 232);
    let token_b_vault = pubkey_at(pool_data, 264);

    let tp = Pubkey::from_str_const(TOKEN_PROGRAM);
    let tp22 = Pubkey::from_str_const(TOKEN_PROGRAM_2022);
    let token_a_program = if pool_data[482] == 1 { tp22 } else { tp };
    let token_b_program = if pool_data[483] == 1 { tp22 } else { tp };

    vec![
        AccountMeta::new_readonly(dex, false),             // [0]  dex_program
        AccountMeta::new(user, true),                       // [1]  swap_authority (signer)
        AccountMeta::new(user_token_in, false),             // [2]  swap_source_token
        AccountMeta::new(user_token_out, false),            // [3]  swap_dest_token
        AccountMeta::new_readonly(pool_authority, false),   // [4]  pool_authority
        AccountMeta::new(pool_pubkey, false),               // [5]  pool
        AccountMeta::new(token_a_vault, false),             // [6]  token_a_vault
        AccountMeta::new(token_b_vault, false),             // [7]  token_b_vault
        AccountMeta::new_readonly(token_a_mint, false),     // [8]  token_a_mint
        AccountMeta::new_readonly(token_b_mint, false),     // [9]  token_b_mint
        AccountMeta::new_readonly(token_a_program, false),  // [10] token_a_program
        AccountMeta::new_readonly(token_b_program, false),  // [11] token_b_program
        AccountMeta::new_readonly(event_auth, false),       // [12] event_authority
    ]
}

/// DAMM V1 — 16 accounts.
fn collect_damm_v1_accounts(
    pool_data: &[u8],
    pool_pubkey: Pubkey,
    user: Pubkey,
    user_token_in: Pubkey,
    user_token_out: Pubkey,
    _input_mint: &Pubkey,
) -> Vec<AccountMeta> {
    let dex = Pubkey::from_str_const(DAMM_V1_PROGRAM);
    let vault_program = Pubkey::from_str_const(VAULT_PROGRAM);
    let tp = Pubkey::from_str_const(TOKEN_PROGRAM);

    let a_vault = pubkey_at(pool_data, 104);
    let b_vault = pubkey_at(pool_data, 136);

    // PDA derivations against the vault program.
    let (a_token_vault, _) = Pubkey::find_program_address(
        &[b"token_vault", a_vault.as_ref()], &vault_program,
    );
    let (b_token_vault, _) = Pubkey::find_program_address(
        &[b"token_vault", b_vault.as_ref()], &vault_program,
    );
    let (a_vault_lp_mint, _) = Pubkey::find_program_address(
        &[b"lp_mint", a_vault.as_ref()], &vault_program,
    );
    let (b_vault_lp_mint, _) = Pubkey::find_program_address(
        &[b"lp_mint", b_vault.as_ref()], &vault_program,
    );

    let a_vault_lp = pubkey_at(pool_data, 168);
    let b_vault_lp = pubkey_at(pool_data, 200);
    // Protocol token fee: use token A fee (offset 234..266).
    let protocol_token_fee = pubkey_at(pool_data, 234);

    vec![
        AccountMeta::new_readonly(dex, false),            // [0]  dex_program
        AccountMeta::new(user, true),                      // [1]  swap_authority (signer)
        AccountMeta::new(user_token_in, false),            // [2]  swap_source_token
        AccountMeta::new(user_token_out, false),           // [3]  swap_dest_token
        AccountMeta::new(pool_pubkey, false),              // [4]  pool
        AccountMeta::new(a_vault, false),                  // [5]  a_vault
        AccountMeta::new(b_vault, false),                  // [6]  b_vault
        AccountMeta::new(a_token_vault, false),            // [7]  a_token_vault (PDA)
        AccountMeta::new(b_token_vault, false),            // [8]  b_token_vault (PDA)
        AccountMeta::new(a_vault_lp_mint, false),          // [9]  a_vault_lp_mint (PDA)
        AccountMeta::new(b_vault_lp_mint, false),          // [10] b_vault_lp_mint (PDA)
        AccountMeta::new(a_vault_lp, false),               // [11] a_vault_lp
        AccountMeta::new(b_vault_lp, false),               // [12] b_vault_lp
        AccountMeta::new(protocol_token_fee, false),       // [13] admin_token_fee
        AccountMeta::new_readonly(vault_program, false),   // [14] vault_program
        AccountMeta::new_readonly(tp, false),              // [15] token_program
    ]
}

/// DLMM Swap2 — 19 accounts (bin_array1/2 are ZERO_ADDRESS).
fn collect_dlmm_accounts(
    pool_data: &[u8],
    pool_pubkey: Pubkey,
    user: Pubkey,
    user_token_in: Pubkey,
    user_token_out: Pubkey,
    input_mint: &Pubkey,
    in_program: Pubkey,
    out_program: Pubkey,
    bitmap_ext: Option<Pubkey>,
) -> Vec<AccountMeta> {
    let dex = Pubkey::from_str_const(DLMM_PROGRAM);
    let event_auth = Pubkey::from_str_const(DLMM_EVENT_AUTHORITY);
    let memo = Pubkey::from_str_const(MEMO_PROGRAM);

    let token_x_mint = pubkey_at(pool_data, 88);
    let token_y_mint = pubkey_at(pool_data, 120);
    let reserve_x = pubkey_at(pool_data, 152);
    let reserve_y = pubkey_at(pool_data, 184);

    // Bitmap extension: use sentinel (dex_program) if not available.
    let bitmap = bitmap_ext.unwrap_or(dex);

    // Token program ordering depends on which mint is token_x.
    let (token_x_program, token_y_program) = if *input_mint == token_x_mint {
        (in_program, out_program)
    } else {
        (out_program, in_program)
    };

    // Oracle PDA.
    let (oracle, _) = Pubkey::find_program_address(
        &[b"oracle", pool_pubkey.as_ref()], &dex,
    );

    // Bin array PDA: active_id at offset 76..80 (i32 LE).
    let active_id = i32::from_le_bytes(pool_data[76..80].try_into().unwrap());
    let bin_array_index = active_id.div_euclid(70) as i64;
    let (bin_array0, _) = Pubkey::find_program_address(
        &[b"bin_array", pool_pubkey.as_ref(), &bin_array_index.to_le_bytes()], &dex,
    );

    vec![
        AccountMeta::new_readonly(dex, false),             // [0]  dex_program
        AccountMeta::new(user, true),                       // [1]  swap_authority (signer)
        AccountMeta::new(user_token_in, false),             // [2]  swap_source_token
        AccountMeta::new(user_token_out, false),            // [3]  swap_dest_token
        AccountMeta::new(pool_pubkey, false),               // [4]  lb_pair
        AccountMeta::new_readonly(bitmap, false),          // [5]  bitmap_extension (readonly)
        AccountMeta::new(reserve_x, false),                 // [6]  reserve_x
        AccountMeta::new(reserve_y, false),                 // [7]  reserve_y
        AccountMeta::new_readonly(token_x_mint, false),     // [8]  token_x_mint
        AccountMeta::new_readonly(token_y_mint, false),     // [9]  token_y_mint
        AccountMeta::new(oracle, false),                    // [10] oracle (writable!)
        AccountMeta::new(dex, false),                       // [11] host_fee_in (writable!)
        AccountMeta::new_readonly(token_x_program, false),  // [12] token_x_program
        AccountMeta::new_readonly(token_y_program, false),  // [13] token_y_program
        AccountMeta::new_readonly(memo, false),             // [14] memo_program
        AccountMeta::new_readonly(event_auth, false),       // [15] event_authority
        AccountMeta::new(bin_array0, false),                // [16] bin_array0
        AccountMeta::new_readonly(Pubkey::default(), false),// [17] bin_array1 (ZERO_ADDRESS)
        AccountMeta::new_readonly(Pubkey::default(), false),// [18] bin_array2 (ZERO_ADDRESS)
    ]
}

/// Raydium CLMM SwapV2 — 18 accounts (tick_array1/2 may be ZERO_ADDRESS).
fn collect_clmm_accounts(
    pool_data: &[u8],
    pool_pubkey: Pubkey,
    user: Pubkey,
    user_token_in: Pubkey,
    user_token_out: Pubkey,
    input_mint: &Pubkey,
    _in_program: Pubkey,
    _out_program: Pubkey,
    tick_arrays: Vec<Pubkey>,
) -> Vec<AccountMeta> {
    let dex = Pubkey::from_str_const(CLMM_PROGRAM);
    let tp = Pubkey::from_str_const(TOKEN_PROGRAM);
    let tp22 = Pubkey::from_str_const(TOKEN_PROGRAM_2022);
    let memo = Pubkey::from_str_const(MEMO_PROGRAM);

    let amm_config = pubkey_at(pool_data, 9);
    let token_mint_0 = pubkey_at(pool_data, 73);
    let token_mint_1 = pubkey_at(pool_data, 105);
    let vault_0 = pubkey_at(pool_data, 137);
    let vault_1 = pubkey_at(pool_data, 169);
    let observation = pubkey_at(pool_data, 201);

    // Determine direction: if input is mint0 → vault0 is input vault.
    let (input_vault, output_vault, input_mint_key, output_mint_key) =
        if *input_mint == token_mint_0 {
            (vault_0, vault_1, token_mint_0, token_mint_1)
        } else {
            (vault_1, vault_0, token_mint_1, token_mint_0)
        };

    // TickArrayBitmapExtension PDA.
    let (ex_bitmap, _) = Pubkey::find_program_address(
        &[b"TickArrayBitmapExtension", pool_pubkey.as_ref()], &dex,
    );

    let tick0 = tick_arrays.first().copied().unwrap_or_default();
    let tick1 = tick_arrays.get(1).copied().unwrap_or_default();
    let tick2 = tick_arrays.get(2).copied().unwrap_or_default();

    vec![
        AccountMeta::new_readonly(dex, false),             // [0]  dex_program
        AccountMeta::new(user, true),                       // [1]  swap_authority (signer)
        AccountMeta::new(user_token_in, false),             // [2]  swap_source_token
        AccountMeta::new(user_token_out, false),            // [3]  swap_dest_token
        AccountMeta::new_readonly(amm_config, false),       // [4]  amm_config
        AccountMeta::new(pool_pubkey, false),               // [5]  pool
        AccountMeta::new(input_vault, false),               // [6]  input_vault
        AccountMeta::new(output_vault, false),              // [7]  output_vault
        AccountMeta::new(observation, false),               // [8]  observation
        AccountMeta::new_readonly(tp, false),               // [9]  token_program
        AccountMeta::new_readonly(tp22, false),             // [10] token_program_2022
        AccountMeta::new_readonly(memo, false),             // [11] memo_program
        AccountMeta::new_readonly(input_mint_key, false),   // [12] input_vault_mint
        AccountMeta::new_readonly(output_mint_key, false),  // [13] output_vault_mint
        AccountMeta::new(ex_bitmap, false),                 // [14] tick_array_bitmap_extension
        AccountMeta::new(tick0, false),                     // [15] tick_array0
        AccountMeta::new(tick1, false),                     // [16] tick_array1 (or ZERO_ADDRESS)
        AccountMeta::new(tick2, false),                     // [17] tick_array2 (or ZERO_ADDRESS)
    ]
}

/// Raydium AMM V4 — 19 accounts.
fn collect_ray_v4_accounts(
    pool_data: &[u8],
    pool_pubkey: Pubkey,
    user: Pubkey,
    user_token_in: Pubkey,
    user_token_out: Pubkey,
) -> Vec<AccountMeta> {
    let dex = Pubkey::from_str_const(RAY_V4_PROGRAM);
    let tp = Pubkey::from_str_const(TOKEN_PROGRAM);
    let authority = Pubkey::from_str_const(RAY_V4_AUTHORITY);

    let coin_vault = pubkey_at(pool_data, 336);
    let pc_vault = pubkey_at(pool_data, 368);

    vec![
        AccountMeta::new_readonly(dex, false),             // [0]  dex_program
        AccountMeta::new(user, true),                       // [1]  swap_authority (signer)
        AccountMeta::new(user_token_in, false),             // [2]  swap_source_token
        AccountMeta::new(user_token_out, false),            // [3]  swap_dest_token
        AccountMeta::new_readonly(tp, false),               // [4]  token_program
        AccountMeta::new(pool_pubkey, false),               // [5]  amm_id
        AccountMeta::new_readonly(authority, false),        // [6]  amm_authority
        AccountMeta::new(pool_pubkey, false),               // [7]  amm_open_orders (placeholder)
        AccountMeta::new(pool_pubkey, false),               // [8]  amm_target_orders (placeholder)
        AccountMeta::new(coin_vault, false),                // [9]  pool_coin_vault
        AccountMeta::new(pc_vault, false),                  // [10] pool_pc_vault
        AccountMeta::new(pool_pubkey, false),               // [11] serum_program_id (placeholder)
        AccountMeta::new(pool_pubkey, false),               // [12] serum_market (placeholder)
        AccountMeta::new(pool_pubkey, false),               // [13] serum_bids (placeholder)
        AccountMeta::new(pool_pubkey, false),               // [14] serum_asks (placeholder)
        AccountMeta::new(pool_pubkey, false),               // [15] serum_event_queue (placeholder)
        AccountMeta::new(pool_pubkey, false),               // [16] serum_coin_vault (placeholder)
        AccountMeta::new(pool_pubkey, false),               // [17] serum_pc_vault (placeholder)
        AccountMeta::new(pool_pubkey, false),               // [18] serum_vault_signer (placeholder)
    ]
}

/// Pumpfun AMM — 13 accounts (same layout for buy and sell).
fn collect_pumpfun_accounts(
    pool_data: &[u8],
    pool_pubkey: Pubkey,
    user: Pubkey,
    user_token_in: Pubkey,
    user_token_out: Pubkey,
    input_mint: &Pubkey,
    in_program: Pubkey,
    out_program: Pubkey,
) -> Vec<AccountMeta> {
    let dex = Pubkey::from_str_const(PUMPFUN_PROGRAM);
    let system_program = Pubkey::from_str_const("11111111111111111111111111111111");
    let (event_auth, _) = Pubkey::find_program_address(&[b"__event_authority"], &dex);

    let base_mint = pubkey_at(pool_data, 43);
    let quote_mint = pubkey_at(pool_data, 75);
    let pool_base_vault = pubkey_at(pool_data, 139);
    let pool_quote_vault = pubkey_at(pool_data, 171);

    // Direction determines token program ordering: buy means input=quote, output=base.
    let is_buy = *input_mint == quote_mint;
    let (base_token_program, quote_token_program) = if is_buy {
        (out_program, in_program)
    } else {
        (in_program, out_program)
    };

    vec![
        AccountMeta::new_readonly(dex, false),              // [0]  dex_program
        AccountMeta::new(user, true),                        // [1]  swap_authority (signer)
        AccountMeta::new(user_token_in, false),              // [2]  swap_source_token
        AccountMeta::new(user_token_out, false),             // [3]  swap_dest_token
        AccountMeta::new(pool_pubkey, false),                // [4]  pool
        AccountMeta::new_readonly(base_mint, false),         // [5]  base_mint
        AccountMeta::new_readonly(quote_mint, false),        // [6]  quote_mint
        AccountMeta::new(pool_base_vault, false),            // [7]  pool_base_vault
        AccountMeta::new(pool_quote_vault, false),           // [8]  pool_quote_vault
        AccountMeta::new_readonly(base_token_program, false),// [9]  base_token_program
        AccountMeta::new_readonly(quote_token_program, false),// [10] quote_token_program
        AccountMeta::new_readonly(system_program, false),    // [11] system_program
        AccountMeta::new_readonly(event_auth, false),        // [12] event_authority
    ]
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract a 32-byte Pubkey from raw account data at the given byte offset.
fn pubkey_at(data: &[u8], offset: usize) -> Pubkey {
    Pubkey::new_from_array(data[offset..offset + 32].try_into().unwrap())
}

fn detect_mint_programs(store: &AccountStore, mints: &[Pubkey]) -> Vec<(Pubkey, Pubkey)> {
    let tp = Pubkey::from_str_const(TOKEN_PROGRAM);
    let tp22 = Pubkey::from_str_const(TOKEN_PROGRAM_2022);
    mints.iter().map(|mint| {
        let prog = store.get(mint)
            .map(|acc| if acc.owner == tp22 { tp22 } else { tp })
            .unwrap_or(tp);
        (*mint, prog)
    }).collect()
}

fn mint_program(programs: &[(Pubkey, Pubkey)], mint: &Pubkey, default: &Pubkey) -> Pubkey {
    programs.iter()
        .find(|(m, _)| m == mint)
        .map(|(_, p)| *p)
        .unwrap_or(*default)
}
