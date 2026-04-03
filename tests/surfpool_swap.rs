//! Dynamic multi-hop swap on Surfpool via thunder-router.
//!
//! Run:
//!   INPUT=SOL OUTPUT=6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN \
//!   AMOUNT=0.1 MAX_HOPS=2 \
//!   cargo test --release --test surfpool_swap -- --nocapture

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::str::FromStr;

use borsh::BorshSerialize;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_rpc_client_api::filter::{Memcmp, RpcFilterType};
use solana_account_decoder_client_types::UiAccountEncoding;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    message::{v0, AddressLookupTableAccount, VersionedMessage},
    signature::{Keypair, Signer},
    transaction::VersionedTransaction,
};
use solana_system_interface::instruction as system_ix;
use spl_associated_token_account::get_associated_token_address_with_program_id;
use spl_associated_token_account::instruction::create_associated_token_account_idempotent;
use spl_token::instruction::sync_native;
use thunder_aggregator::{
    cache, loader, pool_index::PoolIndex, router::Router,
    types::Route,
};
use thunder_core::{infer_mint_decimals, Market, WSOL, TOKEN_PROGRAM, TOKEN_PROGRAM_2022};

const DLMM_PROGRAM: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";
const CLMM_PROGRAM: &str = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";
const ROUTER_PROGRAM_ID: &str = "7WgM9BLWicvmxZwNsT5AUKqxsf6QqBSy2RxeEEwjzJFu";
const DAMM_V1_PROGRAM: &str = "Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB";
const DAMM_V2_PROGRAM: &str = "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG";
const DAMM_V2_POOL_AUTHORITY: &str = "HLnpSz9h2S4hiLQ43rnSD9XkcUThA7B8hQMKmDaiTLcC";
const PUMPFUN_PROGRAM: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";
const RAY_V4_PROGRAM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
const RAY_V4_AUTHORITY: &str = "5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1";
const VAULT_PROGRAM: &str = "24Uqj9JCLxUeoC3hGfh5W3s9FM9uCHqSim67FNPDFSms";
const MEMO_PROGRAM_STR: &str = "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr";
const DLMM_EVENT_AUTH: &str = "D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6";

/// Supported DEX names for the router.
const SUPPORTED_DEXES: &[&str] = &[
    "Meteora DLMM",
    "Raydium CLMM",
    "Meteora DAMM V1",
    "Meteora DAMM V2",
    "Raydium AMM V4",
    "Pumpfun AMM",
];

// =========================================================================
// Router program types — V2 compact format (mirrors on-chain layout)
// =========================================================================

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

// =========================================================================
// Build a single router instruction encompassing all hops (V2 compact)
// =========================================================================

async fn build_router_instruction(
    rpc: &RpcClient,
    route: &Route,
    user: &Pubkey,
    amount_in: u64,
    min_amount_out: u64,
    bitmap_map: &HashMap<Pubkey, Pubkey>,
    tick_array_map: &HashMap<Pubkey, Vec<Pubkey>>,
    mint_programs: &HashMap<Pubkey, Pubkey>,
) -> Result<Instruction, Box<dyn std::error::Error + Send + Sync>> {
    let tp = Pubkey::from_str_const(TOKEN_PROGRAM);
    let mut all_account_metas: Vec<AccountMeta> = Vec::new();
    let mut hops: Vec<SwapHop> = Vec::new();

    for hop in &route.hops {
        let pool_pubkey = Pubkey::from_str(&hop.pool_address)?;
        let account = rpc.get_account(&pool_pubkey).await?;
        let pool_data = &account.data;

        let in_prog = *mint_programs.get(&hop.input_mint).unwrap_or(&tp);
        let out_prog = *mint_programs.get(&hop.output_mint).unwrap_or(&tp);
        let user_in = get_associated_token_address_with_program_id(user, &hop.input_mint, &in_prog);
        let user_out = get_associated_token_address_with_program_id(user, &hop.output_mint, &out_prog);

        let (dex_type, accounts) = match hop.dex_name.as_str() {
            "Meteora DAMM V2" => {
                let metas = collect_damm_v2_accounts(
                    pool_data, pool_pubkey, *user, user_in, user_out, &hop.input_mint,
                );
                (DexType::MeteoraDAMMV2, metas)
            }
            "Meteora DAMM V1" => {
                let metas = collect_damm_v1_accounts(
                    pool_data, pool_pubkey, *user, user_in, user_out, &hop.input_mint,
                );
                (DexType::MeteoraDAMMV1, metas)
            }
            "Meteora DLMM" => {
                let bitmap_ext = bitmap_map.get(&pool_pubkey).copied();
                let metas = collect_dlmm_accounts(
                    pool_data, pool_pubkey, *user, user_in, user_out,
                    &hop.input_mint, in_prog, out_prog, bitmap_ext,
                );
                (DexType::MeteoraDLMM, metas)
            }
            "Raydium CLMM" => {
                let tick_arrays = tick_array_map.get(&pool_pubkey).cloned().unwrap_or_default();
                let metas = collect_clmm_accounts(
                    pool_data, pool_pubkey, *user, user_in, user_out,
                    &hop.input_mint, in_prog, out_prog, tick_arrays,
                );
                (DexType::RaydiumCLMM, metas)
            }
            "Raydium AMM V4" => {
                let metas = collect_ray_v4_accounts(
                    pool_data, pool_pubkey, *user, user_in, user_out,
                );
                (DexType::RaydiumAMMV4, metas)
            }
            "Pumpfun AMM" => {
                let quote_mint = pubkey_at(pool_data, 75);
                let is_buy = hop.input_mint == quote_mint;
                let dex = if is_buy { DexType::PumpfunBuy } else { DexType::PumpfunSell };
                let metas = collect_pumpfun_accounts(
                    pool_data, pool_pubkey, *user, user_in, user_out,
                    &hop.input_mint, in_prog, out_prog,
                );
                (dex, metas)
            }
            other => return Err(format!("DEX '{}' not supported by router", other).into()),
        };

        hops.push(SwapHop {
            dex_type,
            num_accounts: accounts.len() as u8,
        });
        all_account_metas.extend(accounts);
    }

    let args = ExecuteRouteArgs {
        amount_in,
        min_amount_out,
        hops,
    };

    let data = borsh::to_vec(&args)?;
    let router_program = Pubkey::from_str(ROUTER_PROGRAM_ID)?;

    Ok(Instruction {
        program_id: router_program,
        accounts: all_account_metas,
        data,
    })
}

// =========================================================================
// Per-DEX account collection (replicates engine's crates/engine/src/swap.rs)
// =========================================================================

/// Extract a 32-byte Pubkey from raw account data at the given byte offset.
fn pubkey_at(data: &[u8], offset: usize) -> Pubkey {
    Pubkey::new_from_array(data[offset..offset + 32].try_into().unwrap())
}

/// DAMM V2 — 13 accounts (V3 OKX-pattern).
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

/// DAMM V1 — 16 accounts (V3 OKX-pattern).
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

/// DLMM Swap2 — 19 accounts (V3 OKX-pattern, fixed size).
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
    let event_auth = Pubkey::from_str_const(DLMM_EVENT_AUTH);
    let memo = Pubkey::from_str_const(MEMO_PROGRAM_STR);

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
        AccountMeta::new_readonly(Pubkey::default(), false), // [17] bin_array1 (ZERO)
        AccountMeta::new_readonly(Pubkey::default(), false), // [18] bin_array2 (ZERO)
    ]
}

/// Raydium CLMM SwapV2 — 18 accounts (V3 OKX-pattern, fixed size).
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
    let memo = Pubkey::from_str_const(MEMO_PROGRAM_STR);

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

    // Tick array bitmap extension PDA.
    let (ex_bitmap, _) = Pubkey::find_program_address(
        &[b"TickArrayBitmapExtension", pool_pubkey.as_ref()], &dex,
    );

    let zero = Pubkey::default();
    let ta0 = tick_arrays.first().copied().unwrap_or(zero);
    let ta1 = tick_arrays.get(1).copied().unwrap_or(zero);
    let ta2 = tick_arrays.get(2).copied().unwrap_or(zero);

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
        AccountMeta::new(ex_bitmap, false),                 // [14] ex_bitmap (PDA)
        AccountMeta::new(ta0, false),                       // [15] tick_array0
        AccountMeta::new(ta1, false),                       // [16] tick_array1 or ZERO
        AccountMeta::new(ta2, false),                       // [17] tick_array2 or ZERO
    ]
}

/// Raydium AMM V4 — 19 accounts (V3 OKX-pattern).
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

    let base_vault = pubkey_at(pool_data, 336);
    let quote_vault = pubkey_at(pool_data, 368);

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
        AccountMeta::new(base_vault, false),                // [9]  pool_coin_vault
        AccountMeta::new(quote_vault, false),               // [10] pool_pc_vault
        AccountMeta::new(pool_pubkey, false),               // [11] serum_program (placeholder)
        AccountMeta::new(pool_pubkey, false),               // [12] serum_market (placeholder)
        AccountMeta::new(pool_pubkey, false),               // [13] serum_bids (placeholder)
        AccountMeta::new(pool_pubkey, false),               // [14] serum_asks (placeholder)
        AccountMeta::new(pool_pubkey, false),               // [15] serum_event_queue (placeholder)
        AccountMeta::new(pool_pubkey, false),               // [16] serum_coin_vault (placeholder)
        AccountMeta::new(pool_pubkey, false),               // [17] serum_pc_vault (placeholder)
        AccountMeta::new(pool_pubkey, false),               // [18] serum_vault_signer (placeholder)
    ]
}

/// Pumpfun AMM — 13 accounts (V3 OKX-pattern, same for buy and sell).
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

    // Direction: buy = input is quote, sell = input is base.
    let is_buy = *input_mint == quote_mint;
    let (base_token_prog, quote_token_prog) = if is_buy {
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
        AccountMeta::new_readonly(base_token_prog, false),   // [9]  base_token_program
        AccountMeta::new_readonly(quote_token_prog, false),  // [10] quote_token_program
        AccountMeta::new_readonly(system_program, false),    // [11] system_program
        AccountMeta::new_readonly(event_auth, false),        // [12] event_authority
    ]
}

// =========================================================================
// Address Lookup Table helpers
// =========================================================================

/// ALT program ID.
const ALT_PROGRAM_ID: Pubkey = Pubkey::from_str_const("AddressLookupTab1e1111111111111111111111111");

/// Derive the ALT address from authority + recent slot (mirrors the on-chain derivation).
fn derive_lookup_table_address(authority: &Pubkey, recent_slot: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[authority.as_ref(), &recent_slot.to_le_bytes()],
        &ALT_PROGRAM_ID,
    )
}

/// Build a CreateLookupTable instruction (bincode-encoded, variant 0).
fn build_create_alt_ix(authority: Pubkey, payer: Pubkey, recent_slot: u64) -> (Instruction, Pubkey) {
    let (alt_address, bump_seed) = derive_lookup_table_address(&authority, recent_slot);
    // bincode: variant 0 (u32 LE) + recent_slot (u64 LE) + bump_seed (u8)
    let mut data = Vec::with_capacity(13);
    data.extend_from_slice(&0u32.to_le_bytes());
    data.extend_from_slice(&recent_slot.to_le_bytes());
    data.push(bump_seed);
    let ix = Instruction {
        program_id: ALT_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(alt_address, false),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(Pubkey::from_str_const("11111111111111111111111111111111"), false),
        ],
        data,
    };
    (ix, alt_address)
}

/// Build an ExtendLookupTable instruction (bincode-encoded, variant 2).
fn build_extend_alt_ix(alt_address: Pubkey, authority: Pubkey, payer: Pubkey, new_addresses: &[Pubkey]) -> Instruction {
    // bincode: variant 2 (u32 LE) + vec_len (u64 LE) + pubkeys (32 bytes each)
    let mut data = Vec::with_capacity(4 + 8 + new_addresses.len() * 32);
    data.extend_from_slice(&2u32.to_le_bytes());
    data.extend_from_slice(&(new_addresses.len() as u64).to_le_bytes());
    for addr in new_addresses {
        data.extend_from_slice(addr.as_ref());
    }
    Instruction {
        program_id: ALT_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(alt_address, false),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(Pubkey::from_str_const("11111111111111111111111111111111"), false),
        ],
        data,
    }
}

#[allow(deprecated)]
/// Create an Address Lookup Table populated with common DEX addresses.
/// This compresses V0 transaction size by replacing 32-byte pubkeys with 1-byte indices.
async fn create_swap_alt(
    rpc: &RpcClient,
    payer: &Keypair,
) -> Result<AddressLookupTableAccount, Box<dyn std::error::Error + Send + Sync>> {
    let common_addresses: Vec<Pubkey> = vec![
        // DEX programs
        Pubkey::from_str_const("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo"), // DLMM
        Pubkey::from_str_const("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK"), // CLMM
        Pubkey::from_str_const("Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB"), // DAMM V1
        Pubkey::from_str_const("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG"),  // DAMM V2
        Pubkey::from_str_const("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8"), // Raydium V4
        Pubkey::from_str_const("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"),  // Pumpfun
        // Infrastructure
        Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"), // Token Program
        Pubkey::from_str_const("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"),  // Token-2022
        Pubkey::from_str_const("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"), // AToken
        Pubkey::from_str_const("11111111111111111111111111111111"),                // System
        Pubkey::from_str_const(ROUTER_PROGRAM_ID),                                  // Router
        // Common DEX authorities and helpers
        Pubkey::from_str_const("5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1"), // Raydium V4 authority
        Pubkey::from_str_const("HLnpSz9h2S4hiLQ43rnSD9XkcUThA7B8hQMKmDaiTLcC"), // DAMM V2 pool authority
        Pubkey::from_str_const("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr"),  // Memo program
        Pubkey::from_str_const("D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6"), // DLMM event authority
        // Common mints
        Pubkey::from_str_const(WSOL),
        Pubkey::from_str_const("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"), // USDC
        Pubkey::from_str_const("Es9vMFrzaCERmKfrE1SBVdVJNNWWT8o2WAwi2xoF7d3s"), // USDT
    ];

    let recent_slot = rpc.get_slot().await?;
    let (create_ix, alt_address) = build_create_alt_ix(payer.pubkey(), payer.pubkey(), recent_slot);

    // Create the ALT
    let blockhash = rpc.get_latest_blockhash().await?;
    let create_msg = solana_sdk::message::Message::new_with_blockhash(
        &[create_ix], Some(&payer.pubkey()), &blockhash,
    );
    let create_tx = VersionedTransaction::try_new(
        VersionedMessage::Legacy(create_msg), &[payer],
    )?;
    rpc.send_and_confirm_transaction(&create_tx).await?;

    // Extend with all addresses (max ~30 per tx to stay within size limits)
    for chunk in common_addresses.chunks(20) {
        let extend_ix = build_extend_alt_ix(alt_address, payer.pubkey(), payer.pubkey(), chunk);
        let bh = rpc.get_latest_blockhash().await?;
        let extend_msg = solana_sdk::message::Message::new_with_blockhash(
            &[extend_ix], Some(&payer.pubkey()), &bh,
        );
        let extend_tx = VersionedTransaction::try_new(
            VersionedMessage::Legacy(extend_msg), &[payer],
        )?;
        rpc.send_and_confirm_transaction(&extend_tx).await?;
    }

    // Wait for ALT to become usable (needs to advance past creation slot)
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    Ok(AddressLookupTableAccount {
        key: alt_address,
        addresses: common_addresses,
    })
}

// =========================================================================
// Test
// =========================================================================

#[tokio::test]
async fn test_dynamic_swap() {
    dotenvy::dotenv().ok();

    let input_str = env::var("INPUT").unwrap_or_else(|_| "SOL".into());
    let output_str = env::var("OUTPUT").unwrap_or_else(|_| "6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN".into());
    let amount_str = env::var("AMOUNT").unwrap_or_else(|_| "0.1".into());
    let max_hops: usize = env::var("MAX_HOPS").ok().and_then(|s| s.parse().ok()).unwrap_or(2);
    let slippage_bps: u64 = env::var("SLIPPAGE").ok().and_then(|s| s.parse().ok()).unwrap_or(500);

    let input_mint = parse_mint(&input_str);
    let output_mint = parse_mint(&output_str);
    let input_decimals = infer_mint_decimals(&input_mint);
    let amount_in = (amount_str.parse::<f64>().expect("Invalid AMOUNT")
        * 10f64.powi(input_decimals as i32)) as u64;

    let keypair = Keypair::from_base58_string(&env::var("PRIVATE_KEY").expect("PRIVATE_KEY"));
    let user = keypair.pubkey();
    let surfpool_url = env::var("SURFPOOL_URL").unwrap_or_else(|_| "http://127.0.0.1:8899".into());
    let rpc_url = env::var("RPC_URL").unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".into());

    let rpc = RpcClient::new_with_commitment(surfpool_url, CommitmentConfig::confirmed());
    let mainnet = RpcClient::new_with_commitment(rpc_url.clone(), CommitmentConfig::confirmed());

    println!("Thunder - Dynamic Surfpool Swap (Router)");
    println!("========================================\n");
    println!("Wallet:  {user}");
    println!("Input:   {input_str}");
    println!("Output:  {}", &output_mint.to_string()[..12]);
    println!("Amount:  {amount_str} ({amount_in} raw)");
    println!("Hops:    {max_hops}\n");

    let sol_before = rpc.get_balance(&user).await.unwrap_or(0);
    println!("SOL balance: {:.4}\n", sol_before as f64 / 1e9);

    // Check if we already have the output token
    let out_prog_check = if let Ok(acc) = mainnet.get_account(&output_mint).await {
        acc.owner
    } else {
        Pubkey::from_str_const(TOKEN_PROGRAM)
    };
    let out_ata_check = get_associated_token_address_with_program_id(&user, &output_mint, &out_prog_check);
    let output_before = rpc.get_account(&out_ata_check).await.ok()
        .filter(|a| a.data.len() >= 72)
        .map(|a| u64::from_le_bytes(a.data[64..72].try_into().unwrap()))
        .unwrap_or(0);

    // ── Load pools ──────────────────────────────────────────────────────
    let cache_path = PathBuf::from(env::var("CACHE_PATH").unwrap_or_else(|_| "pools.cache".into()));
    let index = match cache::load_cache(&cache_path) {
        Ok((idx, _)) => { println!("Loaded {} pools from cache", idx.pool_count()); idx }
        Err(_) => {
            println!("Loading from RPC...");
            let cb: loader::ProgressCallback = Box::new(|_| {});
            let idx = loader::PoolLoader::new(&rpc_url).load_all(&cb).await.expect("Load failed");
            let _ = cache::save_cache(&idx, &cache_path);
            println!("Loaded {} pools", idx.pool_count()); idx
        }
    };

    // ── Pre-fetch all DLMM bitmap extensions (53 on-chain) ──────────────
    let bitmap_map = fetch_all_bitmap_extensions(&mainnet).await;
    println!("Bitmap extensions: {}\n", bitmap_map.len());
    // Debug: check if specific pool is in the index
    let test_pool = "3C5YE97HADPDxZehYq9Cis8AXr9aNyrUsczKzE1nDbW9";
    if let Some(entry) = index.get_pool(test_pool) {
        let meta = entry.market.metadata().unwrap();
        println!("DEBUG pool {}: quote={} base={}", &test_pool[..8], &meta.quote_mint.to_string()[..8], &meta.base_mint.to_string()[..8]);
        let direct = index.direct_pools(&meta.quote_mint, &meta.base_mint);
        println!("DEBUG direct pools for this pair: {}", direct.len());
    } else {
        println!("DEBUG pool {} NOT in index", &test_pool[..8]);
    }
    let wsol_pk = Pubkey::from_str_const(WSOL);
    let usdc_pk = Pubkey::from_str_const("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
    println!("DEBUG SOL<->USDC pools: {}", index.direct_pools(&wsol_pk, &usdc_pk).len());
    println!("DEBUG USDC<->TRUMP pools: {}", index.direct_pools(&usdc_pk, &output_mint).len());

    // ── Create Address Lookup Table ────────────────────────────────────
    println!("Creating Address Lookup Table...");
    let alt_account = create_swap_alt(&rpc, &keypair).await.expect("ALT creation failed");
    println!("ALT created: {} ({} addresses)\n", alt_account.key, alt_account.addresses.len());

    // ── Find route ──────────────────────────────────────────────────────
    println!("Finding route (max {max_hops} hops)...\n");
    let router = Router::new(&index, max_hops);
    let quote = router.find_routes(input_mint, output_mint, amount_in, 200).expect("Route failed");

    if quote.routes.is_empty() {
        println!("No routes found.");
        return;
    }

    for (i, route) in quote.routes.iter().take(5).enumerate() {
        print_route(i + 1, route, if i == 0 { " <- best" } else { "" });
    }
    if quote.routes.len() > 5 {
        println!("  ... and {} more routes\n", quote.routes.len() - 5);
    }

    // Pre-fetch CLMM tick arrays in parallel for all unique CLMM pools.
    let mut clmm_pools: Vec<(Pubkey, i32, u16)> = Vec::new();
    for route in &quote.routes {
        for hop in &route.hops {
            if hop.dex_name == "Raydium CLMM" {
                let pk = Pubkey::from_str(&hop.pool_address).unwrap();
                if !clmm_pools.iter().any(|(p, _, _)| *p == pk) {
                    if let Ok(acc) = rpc.get_account(&pk).await {
                        if acc.data.len() >= 273 {
                            let ts = u16::from_le_bytes(acc.data[235..237].try_into().unwrap());
                            let tc = i32::from_le_bytes(acc.data[269..273].try_into().unwrap());
                            clmm_pools.push((pk, tc, ts));
                        }
                    }
                }
            }
        }
    }
    println!("Fetching tick arrays for {} CLMM pools (parallel)...", clmm_pools.len());
    let tick_futures: Vec<_> = clmm_pools.iter()
        .map(|(pk, tc, ts)| fetch_clmm_tick_arrays(&mainnet, pk, *tc, *ts))
        .collect();
    let tick_results = futures::future::join_all(tick_futures).await;
    let mut tick_array_map: HashMap<Pubkey, Vec<Pubkey>> = HashMap::new();
    for ((pk, _, _), tas) in clmm_pools.iter().zip(tick_results) {
        if !tas.is_empty() {
            tick_array_map.insert(*pk, tas);
        }
    }
    println!("Found tick arrays for {}/{} pools", tick_array_map.len(), clmm_pools.len());

    // Pre-filter routes: skip routes with unsupported DEXes or missing tick arrays.
    let viable_routes: Vec<&Route> = quote.routes.iter().filter(|route| {
        route.hops.iter().all(|hop| {
            if !SUPPORTED_DEXES.contains(&hop.dex_name.as_str()) {
                return false;
            }
            match hop.dex_name.as_str() {
                "Raydium CLMM" => {
                    let pk = Pubkey::from_str(&hop.pool_address).unwrap();
                    tick_array_map.contains_key(&pk)
                }
                _ => true,
            }
        })
    }).collect();
    println!("{} viable routes (filtered from {})\n", viable_routes.len(), quote.routes.len());

    // ── Try each viable route ─────────────────────────────────────────────────────
    let wsol = Pubkey::from_str_const(WSOL);
    let tp = Pubkey::from_str_const(TOKEN_PROGRAM);

    for (ri, route) in viable_routes.iter().enumerate() {
        let hops_desc: Vec<String> = route.hops.iter().map(|h| format!("{}({})", &h.pool_address[..6], h.dex_name.chars().take(4).collect::<String>())).collect();
        println!("--- Route {} [{}] ---", ri + 1, hops_desc.join(" -> "));

        // Detect token programs
        let mut all_mints: Vec<Pubkey> = Vec::new();
        for hop in &route.hops {
            if !all_mints.contains(&hop.input_mint) { all_mints.push(hop.input_mint); }
            if !all_mints.contains(&hop.output_mint) { all_mints.push(hop.output_mint); }
        }
        let mint_programs = detect_mint_programs(&rpc, &all_mints).await;

        // Pre-instructions: ATAs + WSOL wrapping
        let mut ixs: Vec<Instruction> = Vec::new();
        let input_is_sol = input_mint == wsol;
        let input_prog = *mint_programs.get(&input_mint).unwrap_or(&tp);
        let user_input_ata = get_associated_token_address_with_program_id(&user, &input_mint, &input_prog);

        if input_is_sol {
            ixs.push(create_associated_token_account_idempotent(&user, &user, &wsol, &tp));
            ixs.push(system_ix::transfer(&user, &user_input_ata, amount_in));
            ixs.push(sync_native(&tp, &user_input_ata).unwrap());
        }

        for hop in &route.hops {
            let prog = *mint_programs.get(&hop.output_mint).unwrap_or(&tp);
            ixs.push(create_associated_token_account_idempotent(&user, &user, &hop.output_mint, &prog));
        }

        // Build the single router instruction for all hops
        let min_out = thunder_core::calculate_min_amount_out(
            route.hops.last().unwrap().output_amount,
            slippage_bps,
        );

        match build_router_instruction(
            &rpc, route, &user, amount_in, min_out,
            &bitmap_map, &tick_array_map, &mint_programs,
        ).await {
            Ok(router_ix) => ixs.push(router_ix),
            Err(e) => {
                println!("  Router IX build failed: {e}");
                continue;
            }
        }

        if input_is_sol {
            ixs.push(spl_token::instruction::close_account(&tp, &user_input_ata, &user, &user, &[]).unwrap());
        }

        // Build, sign, send
        let blockhash = rpc.get_latest_blockhash().await.expect("Blockhash");
        #[allow(deprecated)]
        let message = match v0::Message::try_compile(&user, &ixs, &[alt_account.clone()], blockhash) {
            Ok(m) => m,
            Err(e) => { println!("  Compile: {e}"); continue; }
        };

        let mut tx = VersionedTransaction {
            signatures: vec![solana_sdk::signature::Signature::default()],
            message: VersionedMessage::V0(message),
        };
        tx.signatures[0] = keypair.sign_message(tx.message.serialize().as_slice());

        let tx_size = bincode::serialize(&tx).map(|b| b.len()).unwrap_or(0);
        println!("  {} ixs, {} bytes", ixs.len(), tx_size);
        if tx_size > 1232 { println!("  Too large"); continue; }

        // Simulate first to get full logs on failure
        if ri < 3 {
            let sim_config = solana_rpc_client_api::config::RpcSimulateTransactionConfig {
                sig_verify: false,
                commitment: Some(CommitmentConfig::confirmed()),
                replace_recent_blockhash: false,
                ..Default::default()
            };
            if let Ok(sim) = rpc.simulate_transaction_with_config(&tx, sim_config).await {
                if let Some(err) = &sim.value.err {
                    println!("  SIM ERROR: {err:?}");
                }
                if let Some(logs) = &sim.value.logs {
                    for (li, log) in logs.iter().enumerate() {
                        if log.contains("failed") || log.contains("Error") || log.contains("unauthorized") || log.contains("invoke") || log.contains("Program log") {
                            println!("  LOG[{li:>2}]: {log}");
                        }
                    }
                }
            }
        }

        match rpc.send_and_confirm_transaction(&tx).await {
            Ok(sig) => {
                println!("\n  SWAP SUCCEEDED!");
                println!("  Signature: {sig}\n");

                // Show before/after diff
                let sol_after = rpc.get_balance(&user).await.unwrap_or(0);
                let out_prog = *mint_programs.get(&output_mint).unwrap_or(&tp);
                let out_ata = get_associated_token_address_with_program_id(&user, &output_mint, &out_prog);
                let output_after = rpc.get_account(&out_ata).await.ok()
                    .filter(|a| a.data.len() >= 72)
                    .map(|a| u64::from_le_bytes(a.data[64..72].try_into().unwrap()))
                    .unwrap_or(0);

                let out_dec = infer_mint_decimals(&output_mint);
                let out_diff = output_after.saturating_sub(output_before);
                let sol_diff = sol_before.saturating_sub(sol_after);

                println!("  ┌─────────────────────────────────────────┐");
                println!("  │  SOL   : -{:.6} ({:.4} -> {:.4})", sol_diff as f64 / 1e9, sol_before as f64 / 1e9, sol_after as f64 / 1e9);
                println!("  │  Token : +{:.6} ({:.6} -> {:.6})",
                    out_diff as f64 / 10f64.powi(out_dec as i32),
                    output_before as f64 / 10f64.powi(out_dec as i32),
                    output_after as f64 / 10f64.powi(out_dec as i32),
                );
                println!("  └─────────────────────────────────────────┘");
                return;
            }
            Err(e) => {
                let msg = e.to_string();
                // Extract log messages count and key error info
                let log_count = msg.matches("Program log:").count()
                    + msg.matches("Program ").count();
                if let Some(pos) = msg.find("custom program error") {
                    let end = (pos + 50).min(msg.len());
                    println!("  Failed: {}: {} log messages", &msg[pos..end], log_count);
                    // Print first few log lines for debugging
                    for line in msg.lines().filter(|l| l.contains("Program log:") || l.contains("Error")).take(5) {
                        println!("    {}", line.trim());
                    }
                } else {
                    println!("  Failed: {}", &msg[..msg.len().min(200)]);
                }
            }
        }
    }
    println!("\nAll {} viable routes exhausted ({} total found).", viable_routes.len(), quote.routes.len());
}

// =========================================================================
// Helpers (unchanged)
// =========================================================================

async fn fetch_all_bitmap_extensions(rpc: &RpcClient) -> HashMap<Pubkey, Pubkey> {
    let dlmm = Pubkey::from_str_const(DLMM_PROGRAM);
    let mut map = HashMap::new();

    #[allow(deprecated)]
    let config = RpcProgramAccountsConfig {
        filters: Some(vec![RpcFilterType::DataSize(12488)]),
        account_config: RpcAccountInfoConfig {
            encoding: Some(UiAccountEncoding::Base64),
            data_slice: Some(solana_account_decoder_client_types::UiDataSliceConfig { offset: 8, length: 32 }),
            ..Default::default()
        },
        ..Default::default()
    };

    #[allow(deprecated)]
    if let Ok(accounts) = rpc.get_program_accounts_with_config(&dlmm, config).await {
        for (ext_pubkey, account) in accounts {
            if account.data.len() >= 32 {
                if let Ok(pool_pubkey) = Pubkey::try_from(&account.data[..32]) {
                    map.insert(pool_pubkey, ext_pubkey);
                }
            }
        }
    }

    map
}

async fn detect_mint_programs(rpc: &RpcClient, mints: &[Pubkey]) -> HashMap<Pubkey, Pubkey> {
    let default = Pubkey::from_str_const(TOKEN_PROGRAM);
    let mut map = HashMap::new();
    if let Ok(accounts) = rpc.get_multiple_accounts(mints).await {
        for (mint, maybe) in mints.iter().zip(accounts) {
            map.insert(*mint, maybe.map(|a| a.owner).unwrap_or(default));
        }
    }
    map
}

/// Fetch initialized tick arrays for a CLMM pool from mainnet.
/// Returns the 3 tick arrays closest to the current tick (sorted by proximity).
async fn fetch_clmm_tick_arrays(
    rpc: &RpcClient,
    pool: &Pubkey,
    tick_current: i32,
    tick_spacing: u16,
) -> Vec<Pubkey> {
    let clmm = Pubkey::from_str_const(CLMM_PROGRAM);

    #[allow(deprecated)]
    let config = RpcProgramAccountsConfig {
        filters: Some(vec![
            RpcFilterType::Memcmp(Memcmp::new_raw_bytes(8, pool.to_bytes().to_vec())),
        ]),
        account_config: RpcAccountInfoConfig {
            encoding: Some(UiAccountEncoding::Base64),
            data_slice: Some(solana_account_decoder_client_types::UiDataSliceConfig {
                offset: 8,
                length: 36,
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    #[allow(deprecated)]
    let accounts = match rpc.get_program_accounts_with_config(&clmm, config).await {
        Ok(a) => a,
        Err(_) => return vec![],
    };

    let tick_per_array = tick_spacing as i32 * 60;
    let mut tick_array_info: Vec<(Pubkey, i32)> = accounts.iter().filter_map(|(pubkey, acc)| {
        if acc.data.len() >= 36 {
            let start = i32::from_le_bytes(acc.data[32..36].try_into().ok()?);
            Some((*pubkey, start))
        } else {
            None
        }
    }).collect();

    let current_start = tick_current.div_euclid(tick_per_array) * tick_per_array;
    tick_array_info.sort_by_key(|(_, start)| ((*start - current_start) as i64).abs());

    tick_array_info.into_iter().take(3).map(|(pk, _)| pk).collect()
}

fn parse_mint(s: &str) -> Pubkey {
    if s.eq_ignore_ascii_case("SOL") || s.eq_ignore_ascii_case("WSOL") {
        Pubkey::from_str_const(WSOL)
    } else {
        Pubkey::from_str(s).unwrap_or_else(|_| panic!("Invalid mint: {s}"))
    }
}

fn print_route(num: usize, route: &Route, tag: &str) {
    let n = route.hops.len();
    println!("  Route {num} ({n} hop{}):{tag}", if n == 1 { "" } else { "s" });
    for (j, hop) in route.hops.iter().enumerate() {
        let id = infer_mint_decimals(&hop.input_mint);
        let od = infer_mint_decimals(&hop.output_mint);
        println!(
            "    {}: {:.6} {} -> {:.6} {} via {}..{} ({})",
            j + 1,
            hop.input_amount as f64 / 10f64.powi(id as i32),
            &hop.input_mint.to_string()[..6],
            hop.output_amount as f64 / 10f64.powi(od as i32),
            &hop.output_mint.to_string()[..6],
            &hop.pool_address[..6], &hop.pool_address[hop.pool_address.len()-4..],
            hop.dex_name,
        );
    }
    let od = infer_mint_decimals(&route.output_mint);
    println!("    -> {:.6}\n", route.output_amount as f64 / 10f64.powi(od as i32));
}

#[allow(dead_code)]
async fn print_balance(rpc: &RpcClient, label: &str, ata: &Pubkey, decimals: i32) {
    if let Ok(acc) = rpc.get_account(ata).await {
        if acc.data.len() >= 72 {
            let bal = u64::from_le_bytes(acc.data[64..72].try_into().unwrap());
            println!("  {label}: {:.6} (raw: {bal})", bal as f64 / 10f64.powi(decimals));
        }
    }
}
