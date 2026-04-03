//! Centralized swap instruction builder for all DEXs.
//!
//! Builds raw swap instructions with correct account layouts.
//! Does NOT handle WSOL wrapping or ATA creation — those are handled
//! as separate pre-instructions by the caller.
//!
//! Each function takes pool metadata, user accounts, and swap params,
//! and returns a single `Instruction` for the swap itself.

use solana_pubkey::Pubkey;
use solana_sdk::instruction::{AccountMeta, Instruction};
use thunder_core::GenericError;

// =========================================================================
// Meteora DLMM — Swap2
// =========================================================================

const DLMM_PROGRAM: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";
const DLMM_SWAP_DISC: [u8; 8] = [65, 75, 63, 76, 235, 91, 91, 136];
const DLMM_SWAP_V1_DISC: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200]; // older 'swap' (no bitmap ext)
const DLMM_EVENT_AUTHORITY: &str = "D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6";
const MEMO_PROGRAM: &str = "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr";

pub struct DlmmSwapAccounts {
    pub pool: Pubkey,
    pub reserve_x: Pubkey,
    pub reserve_y: Pubkey,
    pub token_x_mint: Pubkey,
    pub token_y_mint: Pubkey,
    pub user_token_in: Pubkey,
    pub user_token_out: Pubkey,
    pub user: Pubkey,
    pub token_x_program: Pubkey,
    pub token_y_program: Pubkey,
    /// Pass None for pools that don't need it (bin_array_index in [-512, 511]).
    pub bitmap_extension: Option<Pubkey>,
    /// Active bin's bin array PDA. Derive: seeds=[b"bin_array", pool, index.to_le_bytes()].
    pub bin_array: Pubkey,
}

pub fn build_dlmm_swap(
    accounts: &DlmmSwapAccounts,
    amount_in: u64,
    min_amount_out: u64,
) -> Result<Instruction, GenericError> {
    let dlmm = Pubkey::from_str_const(DLMM_PROGRAM);

    // Oracle PDA: seeds=[b"oracle", pool]
    let (oracle, _) = Pubkey::find_program_address(
        &[b"oracle", accounts.pool.as_ref()],
        &dlmm,
    );

    let bitmap_ext = accounts.bitmap_extension.unwrap_or(dlmm);

    let keys = vec![
        AccountMeta::new(accounts.pool, false),               // 0: lb_pair
        AccountMeta::new_readonly(bitmap_ext, false),          // 1: bitmap_extension
        AccountMeta::new(accounts.reserve_x, false),           // 2: reserve_x
        AccountMeta::new(accounts.reserve_y, false),           // 3: reserve_y
        AccountMeta::new(accounts.user_token_in, false),       // 4: user_token_in
        AccountMeta::new(accounts.user_token_out, false),      // 5: user_token_out
        AccountMeta::new_readonly(accounts.token_x_mint, false), // 6: token_x_mint
        AccountMeta::new_readonly(accounts.token_y_mint, false), // 7: token_y_mint
        AccountMeta::new(oracle, false),                       // 8: oracle
        AccountMeta::new_readonly(dlmm, false),                // 9: host_fee (None)
        AccountMeta::new(accounts.user, true),                 // 10: user
        AccountMeta::new_readonly(accounts.token_x_program, false), // 11
        AccountMeta::new_readonly(accounts.token_y_program, false), // 12
        AccountMeta::new_readonly(Pubkey::from_str_const(MEMO_PROGRAM), false), // 13
        AccountMeta::new_readonly(Pubkey::from_str_const(DLMM_EVENT_AUTHORITY), false), // 14
        AccountMeta::new_readonly(dlmm, false),                // 15: program
        AccountMeta::new(accounts.bin_array, false),           // remaining[0]
    ];

    // Data: disc(8) + amount_in(u64) + min_amount_out(u64) + remaining_accounts_info(Vec=empty)
    let mut data = Vec::with_capacity(28);
    data.extend_from_slice(&DLMM_SWAP_DISC);
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&min_amount_out.to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes()); // empty Vec

    Ok(Instruction { program_id: dlmm, accounts: keys, data })
}

/// Build a DLMM swap using the older 'swap' instruction (no bitmap extension required).
/// Use this for pools that don't have a bitmap extension account.
pub fn build_dlmm_swap_v1(
    accounts: &DlmmSwapAccounts,
    amount_in: u64,
    min_amount_out: u64,
) -> Result<Instruction, GenericError> {
    let dlmm = Pubkey::from_str_const(DLMM_PROGRAM);

    let (oracle, _) = Pubkey::find_program_address(
        &[b"oracle", accounts.pool.as_ref()],
        &dlmm,
    );

    // The older 'swap' instruction has a simpler layout:
    // 0: lb_pair, 1: reserve_x, 2: reserve_y, 3: user_token_in, 4: user_token_out,
    // 5: token_x_mint, 6: token_y_mint, 7: oracle, 8: host_fee(None), 9: user,
    // 10: token_x_program, 11: token_y_program, remaining: bin_array(s)
    let keys = vec![
        AccountMeta::new(accounts.pool, false),
        AccountMeta::new(accounts.reserve_x, false),
        AccountMeta::new(accounts.reserve_y, false),
        AccountMeta::new(accounts.user_token_in, false),
        AccountMeta::new(accounts.user_token_out, false),
        AccountMeta::new_readonly(accounts.token_x_mint, false),
        AccountMeta::new_readonly(accounts.token_y_mint, false),
        AccountMeta::new(oracle, false),
        AccountMeta::new_readonly(dlmm, false),  // host_fee = None
        AccountMeta::new(accounts.user, true),
        AccountMeta::new_readonly(accounts.token_x_program, false),
        AccountMeta::new_readonly(accounts.token_y_program, false),
        AccountMeta::new(accounts.bin_array, false),  // remaining[0]
    ];

    let mut data = Vec::with_capacity(24);
    data.extend_from_slice(&DLMM_SWAP_V1_DISC);
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&min_amount_out.to_le_bytes());

    Ok(Instruction { program_id: dlmm, accounts: keys, data })
}

/// Derive the bin array PDA for a DLMM pool.
pub fn dlmm_bin_array_pda(pool: &Pubkey, active_id: i32) -> Pubkey {
    let index: i64 = (active_id as i64).div_euclid(70);
    let dlmm = Pubkey::from_str_const(DLMM_PROGRAM);
    Pubkey::find_program_address(
        &[b"bin_array", pool.as_ref(), &index.to_le_bytes()],
        &dlmm,
    ).0
}

// =========================================================================
// Meteora DAMM V1 — Swap
// =========================================================================

const DAMM_V1_PROGRAM: &str = "Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB";
const DAMM_V1_SWAP_DISC: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];

pub struct DammV1SwapAccounts {
    pub pool: Pubkey,
    pub a_vault: Pubkey,
    pub b_vault: Pubkey,
    pub a_token_vault: Pubkey,
    pub b_token_vault: Pubkey,
    pub a_vault_lp_mint: Pubkey,
    pub b_vault_lp_mint: Pubkey,
    pub a_vault_lp: Pubkey,
    pub b_vault_lp: Pubkey,
    pub protocol_token_fee: Pubkey,
    pub user_token_in: Pubkey,
    pub user_token_out: Pubkey,
    pub user: Pubkey,
    pub token_program: Pubkey,
}

pub fn build_damm_v1_swap(
    accounts: &DammV1SwapAccounts,
    amount_in: u64,
    min_amount_out: u64,
) -> Result<Instruction, GenericError> {
    let program = Pubkey::from_str_const(DAMM_V1_PROGRAM);

    let keys = vec![
        AccountMeta::new(accounts.pool, false),               // 0: pool
        AccountMeta::new(accounts.a_vault, false),             // 1: a_vault
        AccountMeta::new(accounts.b_vault, false),             // 2: b_vault
        AccountMeta::new(accounts.a_token_vault, false),       // 3: a_token_vault
        AccountMeta::new(accounts.b_token_vault, false),       // 4: b_token_vault
        AccountMeta::new(accounts.a_vault_lp_mint, false),     // 5: a_vault_lp_mint
        AccountMeta::new(accounts.b_vault_lp_mint, false),     // 6: b_vault_lp_mint
        AccountMeta::new(accounts.a_vault_lp, false),          // 7: a_vault_lp
        AccountMeta::new(accounts.b_vault_lp, false),          // 8: b_vault_lp
        AccountMeta::new(accounts.protocol_token_fee, false),  // 9: protocol_token_fee
        AccountMeta::new(accounts.user_token_in, false),       // 10: user_source_token
        AccountMeta::new(accounts.user_token_out, false),      // 11: user_destination_token
        AccountMeta::new_readonly(accounts.user, true),        // 12: user_transfer_authority
        AccountMeta::new_readonly(accounts.token_program, false), // 13: token_program
    ];

    let mut data = Vec::with_capacity(24);
    data.extend_from_slice(&DAMM_V1_SWAP_DISC);
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&min_amount_out.to_le_bytes());

    Ok(Instruction { program_id: program, accounts: keys, data })
}

pub fn build_damm_v1_swap_from_pool_data(
    pool_address: Pubkey,
    pool_data: &[u8],
    user: Pubkey,
    user_token_in: Pubkey,
    user_token_out: Pubkey,
    input_mint: Pubkey,
    amount_in: u64,
    min_amount_out: u64,
) -> Result<Instruction, GenericError> {
    if pool_data.len() < 298 {
        return Err(format!(
            "DAMM V1 pool data too short: {} bytes, need at least 298", pool_data.len()
        ).into());
    }

    let token_a_mint = Pubkey::new_from_array(pool_data[40..72].try_into().unwrap());
    let token_b_mint = Pubkey::new_from_array(pool_data[72..104].try_into().unwrap());
    let a_vault = Pubkey::new_from_array(pool_data[104..136].try_into().unwrap());
    let b_vault = Pubkey::new_from_array(pool_data[136..168].try_into().unwrap());
    let a_vault_lp = Pubkey::new_from_array(pool_data[168..200].try_into().unwrap());
    let b_vault_lp = Pubkey::new_from_array(pool_data[200..232].try_into().unwrap());
    let protocol_token_a_fee = Pubkey::new_from_array(pool_data[234..266].try_into().unwrap());
    let protocol_token_b_fee = Pubkey::new_from_array(pool_data[266..298].try_into().unwrap());

    if input_mint != token_a_mint && input_mint != token_b_mint {
        return Err(format!(
            "input_mint {} matches neither token_a {} nor token_b {} in DAMM V1 pool {}",
            input_mint, token_a_mint, token_b_mint, pool_address
        ).into());
    }

    // Fee account is for the OUTPUT token direction.
    // Selling token_a → output is token_b → protocol_token_b_fee.
    // Selling token_b → output is token_a → protocol_token_a_fee.
    let protocol_token_fee = if input_mint == token_b_mint {
        protocol_token_a_fee
    } else {
        protocol_token_b_fee
    };

    let vault_program = Pubkey::from_str_const("24Uqj9JCLxUeoC3hGfh5W3s9FM9uCHqSim67FNPDFSms");
    let a_token_vault = Pubkey::find_program_address(&[b"token_vault", a_vault.as_ref()], &vault_program).0;
    let b_token_vault = Pubkey::find_program_address(&[b"token_vault", b_vault.as_ref()], &vault_program).0;
    let a_vault_lp_mint = Pubkey::find_program_address(&[b"lp_mint", a_vault.as_ref()], &vault_program).0;
    let b_vault_lp_mint = Pubkey::find_program_address(&[b"lp_mint", b_vault.as_ref()], &vault_program).0;

    let token_program = Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

    build_damm_v1_swap(
        &DammV1SwapAccounts {
            pool: pool_address,
            a_vault,
            b_vault,
            a_token_vault,
            b_token_vault,
            a_vault_lp_mint,
            b_vault_lp_mint,
            a_vault_lp,
            b_vault_lp,
            protocol_token_fee,
            user_token_in,
            user_token_out,
            user,
            token_program,
        },
        amount_in,
        min_amount_out,
    )
}

// =========================================================================
// Meteora DAMM V2 — Swap2
// =========================================================================

const DAMM_V2_PROGRAM: &str = "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG";
/// Pool authority PDA (hardcoded, same for all CPAMM V2 pools).
const DAMM_V2_POOL_AUTHORITY: &str = "HLnpSz9h2S4hiLQ43rnSD9XkcUThA7B8hQMKmDaiTLcC";
/// `swap` discriminator (still used on-chain despite docs saying deprecated).
const DAMM_V2_SWAP_DISC: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];

pub struct DammV2SwapAccounts {
    pub pool: Pubkey,
    pub token_a_vault: Pubkey,
    pub token_b_vault: Pubkey,
    pub token_a_mint: Pubkey,
    pub token_b_mint: Pubkey,
    pub user_token_in: Pubkey,
    pub user_token_out: Pubkey,
    pub user: Pubkey,
    pub token_a_program: Pubkey,
    pub token_b_program: Pubkey,
}

pub fn build_damm_v2_swap(
    accounts: &DammV2SwapAccounts,
    amount_in: u64,
    min_amount_out: u64,
) -> Result<Instruction, GenericError> {
    let program = Pubkey::from_str_const(DAMM_V2_PROGRAM);
    let pool_authority = Pubkey::from_str_const(DAMM_V2_POOL_AUTHORITY);
    let (event_authority, _) = Pubkey::find_program_address(&[b"__event_authority"], &program);

    // Account layout from real on-chain swap transactions.
    let keys = vec![
        AccountMeta::new_readonly(pool_authority, false),             // 0: pool_authority
        AccountMeta::new(accounts.pool, false),                      // 1: pool
        AccountMeta::new(accounts.user_token_in, false),             // 2: input_token_account
        AccountMeta::new(accounts.user_token_out, false),            // 3: output_token_account
        AccountMeta::new(accounts.token_a_vault, false),             // 4: token_a_vault
        AccountMeta::new(accounts.token_b_vault, false),             // 5: token_b_vault
        AccountMeta::new_readonly(accounts.token_a_mint, false),     // 6: token_a_mint
        AccountMeta::new_readonly(accounts.token_b_mint, false),     // 7: token_b_mint
        AccountMeta::new(accounts.user, true),                      // 8: payer (signer)
        AccountMeta::new_readonly(accounts.token_a_program, false),  // 9: token_a_program
        AccountMeta::new_readonly(accounts.token_b_program, false),  // 10: token_b_program
        AccountMeta::new_readonly(program, false),                  // 11: program
        AccountMeta::new_readonly(event_authority, false),          // 12: event_authority
        AccountMeta::new_readonly(program, false),                  // 13: program
    ];

    let mut data = Vec::with_capacity(24);
    data.extend_from_slice(&DAMM_V2_SWAP_DISC);
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&min_amount_out.to_le_bytes());

    Ok(Instruction { program_id: program, accounts: keys, data })
}

// =========================================================================
// Raydium CLMM — SwapV2
// =========================================================================

const CLMM_PROGRAM: &str = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";
const CLMM_SWAP_DISC: [u8; 8] = [43, 4, 237, 11, 26, 201, 30, 98];

pub struct ClmmSwapAccounts {
    pub pool: Pubkey,
    pub amm_config: Pubkey,
    pub input_vault: Pubkey,
    pub output_vault: Pubkey,
    pub observation: Pubkey,
    pub input_mint: Pubkey,
    pub output_mint: Pubkey,
    pub user_input_token: Pubkey,
    pub user_output_token: Pubkey,
    pub user: Pubkey,
    pub input_token_program: Pubkey,
    pub output_token_program: Pubkey,
    /// Tick arrays the swap traverses (usually 3).
    pub tick_arrays: Vec<Pubkey>,
}

pub fn build_clmm_swap(
    accounts: &ClmmSwapAccounts,
    amount_in: u64,
    min_amount_out: u64,
    sqrt_price_limit: u128,
    exact_output: bool,
) -> Result<Instruction, GenericError> {
    let program = Pubkey::from_str_const(CLMM_PROGRAM);

    let mut keys = vec![
        AccountMeta::new(accounts.user, true),                    // 0: payer
        AccountMeta::new_readonly(accounts.amm_config, false),     // 1: amm_config
        AccountMeta::new(accounts.pool, false),                    // 2: pool_state
        AccountMeta::new(accounts.user_input_token, false),        // 3: input_token_account
        AccountMeta::new(accounts.user_output_token, false),       // 4: output_token_account
        AccountMeta::new(accounts.input_vault, false),             // 5: input_vault
        AccountMeta::new(accounts.output_vault, false),            // 6: output_vault
        AccountMeta::new(accounts.observation, false),             // 7: observation_state
        AccountMeta::new_readonly(accounts.input_token_program, false), // 8
        AccountMeta::new_readonly(accounts.output_token_program, false), // 9
        AccountMeta::new_readonly(Pubkey::from_str_const(MEMO_PROGRAM), false), // 10
        AccountMeta::new_readonly(accounts.input_mint, false),     // 11
        AccountMeta::new_readonly(accounts.output_mint, false),    // 12
    ];

    // Remaining: tick arrays
    for ta in &accounts.tick_arrays {
        keys.push(AccountMeta::new(*ta, false));
    }

    // Data: disc(8) + amount(u64) + other_amount_threshold(u64) + sqrt_price_limit_x64(u128) + is_base_input(bool)
    //
    // exact_output=false (ExactIn):  is_base_input=true,  amount=input,      threshold=min_output
    // exact_output=true  (ExactOut): is_base_input=false, amount=exact_output, threshold=max_input
    let (amount, threshold, is_base_input) = if exact_output {
        (min_amount_out, amount_in, false)
    } else {
        (amount_in, min_amount_out, true)
    };

    let mut data = Vec::with_capacity(41);
    data.extend_from_slice(&CLMM_SWAP_DISC);
    data.extend_from_slice(&amount.to_le_bytes());
    data.extend_from_slice(&threshold.to_le_bytes());
    data.extend_from_slice(&sqrt_price_limit.to_le_bytes());
    data.push(is_base_input as u8);

    Ok(Instruction { program_id: program, accounts: keys, data })
}

// =========================================================================
// Raydium AMM V4 — SwapBaseIn
// =========================================================================

const RAY_V4_PROGRAM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
const RAY_V4_AUTHORITY: &str = "5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1";

pub struct RayV4SwapAccounts {
    pub pool: Pubkey,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub user_token_in: Pubkey,
    pub user_token_out: Pubkey,
    pub user: Pubkey,
}

pub fn build_ray_v4_swap(
    accounts: &RayV4SwapAccounts,
    amount_in: u64,
    min_amount_out: u64,
    exact_output: bool,
) -> Result<Instruction, GenericError> {
    let program = Pubkey::from_str_const(RAY_V4_PROGRAM);
    let authority = Pubkey::from_str_const(RAY_V4_AUTHORITY);
    let token_prog = Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

    let keys = vec![
        AccountMeta::new_readonly(token_prog, false),          // 0: token_program
        AccountMeta::new(accounts.pool, false),                // 1: amm_id
        AccountMeta::new(authority, false),                    // 2: amm_authority
        AccountMeta::new(accounts.pool, false),                // 3: amm_open_orders (placeholder)
        AccountMeta::new(accounts.base_vault, false),          // 4: pool_coin_token_account
        AccountMeta::new(accounts.quote_vault, false),         // 5: pool_pc_token_account
        AccountMeta::new(accounts.pool, false),                // 6-13: various amm accounts (simplified)
        AccountMeta::new(accounts.pool, false),
        AccountMeta::new(accounts.pool, false),
        AccountMeta::new(accounts.pool, false),
        AccountMeta::new(accounts.pool, false),
        AccountMeta::new(accounts.pool, false),
        AccountMeta::new(accounts.pool, false),
        AccountMeta::new(accounts.pool, false),
        AccountMeta::new(accounts.user_token_in, false),       // 14: user_source
        AccountMeta::new(accounts.user_token_out, false),      // 15: user_destination
        AccountMeta::new(accounts.user, true),                 // 16: user_owner
    ];

    // disc 9 = swap_base_in (exact input), disc 11 = swap_base_out (exact output).
    // Same struct layout {amount_in, min_amount_out} but with swapped semantics:
    //   swap_base_out: amount_in = max SOL to spend, min_amount_out = exact tokens to receive.
    let disc: u8 = if exact_output { 11 } else { 9 };
    let mut data = Vec::with_capacity(17);
    data.push(disc);
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&min_amount_out.to_le_bytes());

    Ok(Instruction { program_id: program, accounts: keys, data })
}

// =========================================================================
// Pumpfun AMM — Buy / Sell
// =========================================================================

const PUMPFUN_PROGRAM: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";
const PUMPFUN_BUY_DISC: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
const PUMPFUN_SELL_DISC: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];

pub struct PumpfunSwapAccounts {
    pub pool: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,     // usually WSOL
    pub pool_base_vault: Pubkey,
    pub pool_quote_vault: Pubkey,
    pub user_base_token: Pubkey,
    pub user_quote_token: Pubkey,
    pub user: Pubkey,
    pub base_token_program: Pubkey,
    pub quote_token_program: Pubkey,
}

pub fn build_pumpfun_swap(
    accounts: &PumpfunSwapAccounts,
    amount_in: u64,
    min_amount_out: u64,
    is_buy: bool,
) -> Result<Instruction, GenericError> {
    let program = Pubkey::from_str_const(PUMPFUN_PROGRAM);
    let system = Pubkey::from_str_const("11111111111111111111111111111111");
    let (event_authority, _) = Pubkey::find_program_address(&[b"__event_authority"], &program);

    let keys = vec![
        AccountMeta::new(accounts.pool, false),
        AccountMeta::new(accounts.user, true),
        AccountMeta::new(accounts.base_mint, false),
        AccountMeta::new(accounts.quote_mint, false),
        AccountMeta::new(accounts.user_base_token, false),
        AccountMeta::new(accounts.user_quote_token, false),
        AccountMeta::new(accounts.pool_base_vault, false),
        AccountMeta::new(accounts.pool_quote_vault, false),
        AccountMeta::new_readonly(accounts.base_token_program, false),
        AccountMeta::new_readonly(accounts.quote_token_program, false),
        AccountMeta::new_readonly(system, false),
        AccountMeta::new_readonly(event_authority, false),
        AccountMeta::new_readonly(program, false),
    ];

    let disc = if is_buy { PUMPFUN_BUY_DISC } else { PUMPFUN_SELL_DISC };
    let mut data = Vec::with_capacity(24);
    data.extend_from_slice(&disc);
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&min_amount_out.to_le_bytes());

    Ok(Instruction { program_id: program, accounts: keys, data })
}

// =========================================================================
// From-pool-data helpers (for AccountStore-based swap building)
// =========================================================================

/// Build a DLMM swap instruction from raw on-chain pool account bytes.
///
/// The caller reads pool data from an AccountStore (or any source) and passes
/// the raw bytes here. This avoids coupling the aggregator crate to engine types.
///
/// `in_program` / `out_program` are the SPL token programs for the input / output
/// mints respectively. The function maps them to token_x_program / token_y_program
/// based on which pool mint matches `input_mint`.
pub fn build_dlmm_swap_from_pool_data(
    pool_address: Pubkey,
    pool_data: &[u8],
    user: Pubkey,
    user_token_in: Pubkey,
    user_token_out: Pubkey,
    input_mint: Pubkey,
    in_program: Pubkey,
    out_program: Pubkey,
    bitmap_ext: Option<Pubkey>,
    amount_in: u64,
    min_amount_out: u64,
) -> Result<Instruction, GenericError> {
    if pool_data.len() < 216 {
        return Err(format!(
            "DLMM pool data too short: {} bytes, need at least 216",
            pool_data.len()
        ).into());
    }

    let active_id = i32::from_le_bytes(pool_data[76..80].try_into().unwrap());
    let token_x_mint = Pubkey::new_from_array(pool_data[88..120].try_into().unwrap());
    let token_y_mint = Pubkey::new_from_array(pool_data[120..152].try_into().unwrap());
    let reserve_x = Pubkey::new_from_array(pool_data[152..184].try_into().unwrap());
    let reserve_y = Pubkey::new_from_array(pool_data[184..216].try_into().unwrap());

    let bin_array = dlmm_bin_array_pda(&pool_address, active_id);

    // Map caller's in/out programs to the pool's x/y programs based on mint direction.
    let (token_x_program, token_y_program) = if input_mint == token_x_mint {
        // Swapping x -> y: in_program owns x, out_program owns y
        (in_program, out_program)
    } else if input_mint == token_y_mint {
        // Swapping y -> x: in_program owns y, out_program owns x
        (out_program, in_program)
    } else {
        return Err(format!(
            "input_mint {} matches neither token_x {} nor token_y {} in DLMM pool {}",
            input_mint, token_x_mint, token_y_mint, pool_address
        ).into());
    };

    build_dlmm_swap(
        &DlmmSwapAccounts {
            pool: pool_address,
            reserve_x,
            reserve_y,
            token_x_mint,
            token_y_mint,
            user_token_in,
            user_token_out,
            user,
            token_x_program,
            token_y_program,
            bitmap_extension: bitmap_ext,
            bin_array,
        },
        amount_in,
        min_amount_out,
    )
}

/// Build a Raydium CLMM swap instruction from raw on-chain pool account bytes.
///
/// The caller reads pool data from an AccountStore (or any source) and passes
/// the raw bytes here. This avoids coupling the aggregator crate to engine types.
pub fn build_clmm_swap_from_pool_data(
    pool_address: Pubkey,
    pool_data: &[u8],
    user: Pubkey,
    user_token_in: Pubkey,
    user_token_out: Pubkey,
    input_mint: Pubkey,
    in_program: Pubkey,
    out_program: Pubkey,
    tick_arrays: Vec<Pubkey>,
    amount_in: u64,
    min_amount_out: u64,
    exact_output: bool,
) -> Result<Instruction, GenericError> {
    if pool_data.len() < 233 {
        return Err(format!(
            "CLMM pool data too short: {} bytes, need at least 233",
            pool_data.len()
        ).into());
    }

    let amm_config = Pubkey::new_from_array(pool_data[9..41].try_into().unwrap());
    let token_mint_0 = Pubkey::new_from_array(pool_data[73..105].try_into().unwrap());
    let token_vault_0 = Pubkey::new_from_array(pool_data[137..169].try_into().unwrap());
    let token_vault_1 = Pubkey::new_from_array(pool_data[169..201].try_into().unwrap());
    let observation_key = Pubkey::new_from_array(pool_data[201..233].try_into().unwrap());

    // Determine input/output vaults and mints from swap direction.
    // token_mint_0 pairs with token_vault_0; the other mint is token_mint_1.
    let (input_vault, output_vault, output_mint) = if input_mint == token_mint_0 {
        // Swapping mint_0 -> mint_1
        let token_mint_1 = if pool_data.len() >= 137 {
            Pubkey::new_from_array(pool_data[105..137].try_into().unwrap())
        } else {
            return Err("CLMM pool data too short to read token_mint_1".into());
        };
        (token_vault_0, token_vault_1, token_mint_1)
    } else {
        // Swapping mint_1 -> mint_0
        (token_vault_1, token_vault_0, token_mint_0)
    };

    // Map in/out programs to input/output token programs.
    let (input_token_program, output_token_program) = (in_program, out_program);

    build_clmm_swap(
        &ClmmSwapAccounts {
            pool: pool_address,
            amm_config,
            input_vault,
            output_vault,
            observation: observation_key,
            input_mint,
            output_mint,
            user_input_token: user_token_in,
            user_output_token: user_token_out,
            user,
            input_token_program,
            output_token_program,
            tick_arrays,
        },
        amount_in,
        min_amount_out,
        0u128, // sqrt_price_limit = 0 means no limit
        exact_output,
    )
}


// =========================================================================
// Meteora DAMM V2 — from pool data
// =========================================================================

/// Build a DAMM V2 swap instruction from raw on-chain pool account bytes.
pub fn build_damm_v2_swap_from_pool_data(
    pool_address: Pubkey,
    pool_data: &[u8],
    user: Pubkey,
    user_token_in: Pubkey,
    user_token_out: Pubkey,
    input_mint: Pubkey,
    _in_program: Pubkey,
    _out_program: Pubkey,
    amount_in: u64,
    min_amount_out: u64,
) -> Result<Instruction, GenericError> {
    if pool_data.len() < 492 {
        return Err(format!(
            "DAMM V2 pool data too short: {} bytes, need at least 492", pool_data.len()
        ).into());
    }

    let token_a_mint = Pubkey::new_from_array(pool_data[168..200].try_into().unwrap());
    let token_b_mint = Pubkey::new_from_array(pool_data[200..232].try_into().unwrap());
    let token_a_vault = Pubkey::new_from_array(pool_data[232..264].try_into().unwrap());
    let token_b_vault = Pubkey::new_from_array(pool_data[264..296].try_into().unwrap());

    // Token program flags: 0 = Token Program, 1 = Token-2022.
    // token_a_flag @ struct offset 474, raw offset 482.
    // token_b_flag @ struct offset 475, raw offset 483.
    let tp = Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
    let tp22 = Pubkey::from_str_const("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
    let token_a_program = if pool_data[482] == 1 { tp22 } else { tp };
    let token_b_program = if pool_data[483] == 1 { tp22 } else { tp };

    if input_mint != token_a_mint && input_mint != token_b_mint {
        return Err(format!(
            "input_mint {} matches neither token_a {} nor token_b {} in DAMM V2 pool {}",
            input_mint, token_a_mint, token_b_mint, pool_address
        ).into());
    };

    build_damm_v2_swap(
        &DammV2SwapAccounts {
            pool: pool_address,
            token_a_vault,
            token_b_vault,
            token_a_mint,
            token_b_mint,
            user_token_in,
            user_token_out,
            user,
            token_a_program,
            token_b_program,
        },
        amount_in,
        min_amount_out,
    )
}

// =========================================================================
// Raydium AMM V4 — from pool data
// =========================================================================

/// Build a Raydium V4 swap instruction from raw on-chain pool account bytes.
/// Note: Raydium V4 has NO Anchor discriminator — fields start at byte 0.
pub fn build_ray_v4_swap_from_pool_data(
    pool_address: Pubkey,
    pool_data: &[u8],
    user: Pubkey,
    user_token_in: Pubkey,
    user_token_out: Pubkey,
    input_mint: Pubkey,
    amount_in: u64,
    min_amount_out: u64,
    exact_output: bool,
) -> Result<Instruction, GenericError> {
    if pool_data.len() < 464 {
        return Err(format!(
            "Raydium V4 pool data too short: {} bytes, need at least 464", pool_data.len()
        ).into());
    }

    let base_vault = Pubkey::new_from_array(pool_data[336..368].try_into().unwrap());
    let quote_vault = Pubkey::new_from_array(pool_data[368..400].try_into().unwrap());
    let base_mint = Pubkey::new_from_array(pool_data[400..432].try_into().unwrap());
    let quote_mint = Pubkey::new_from_array(pool_data[432..464].try_into().unwrap());

    // Verify input_mint matches one of the pool's mints.
    if input_mint != base_mint && input_mint != quote_mint {
        return Err(format!(
            "input_mint {} matches neither base {} nor quote {} in V4 pool {}",
            input_mint, base_mint, quote_mint, pool_address
        ).into());
    }

    build_ray_v4_swap(
        &RayV4SwapAccounts {
            pool: pool_address,
            base_vault,
            quote_vault,
            user_token_in,
            user_token_out,
            user,
        },
        amount_in,
        min_amount_out,
        exact_output,
    )
}

// =========================================================================
// Pumpfun AMM — from pool data
// =========================================================================

/// Build a Pumpfun swap instruction from raw on-chain pool account bytes.
pub fn build_pumpfun_swap_from_pool_data(
    pool_address: Pubkey,
    pool_data: &[u8],
    user: Pubkey,
    user_token_in: Pubkey,
    user_token_out: Pubkey,
    input_mint: Pubkey,
    in_program: Pubkey,
    out_program: Pubkey,
    amount_in: u64,
    min_amount_out: u64,
) -> Result<Instruction, GenericError> {
    if pool_data.len() < 203 {
        return Err(format!(
            "Pumpfun pool data too short: {} bytes, need at least 203", pool_data.len()
        ).into());
    }

    let base_mint = Pubkey::new_from_array(pool_data[43..75].try_into().unwrap());
    let quote_mint = Pubkey::new_from_array(pool_data[75..107].try_into().unwrap());
    let pool_base_token_account = Pubkey::new_from_array(pool_data[139..171].try_into().unwrap());
    let pool_quote_token_account = Pubkey::new_from_array(pool_data[171..203].try_into().unwrap());

    // Buy = spending quote (SOL) to get base token. Sell = spending base to get quote (SOL).
    let is_buy = input_mint == quote_mint;
    if !is_buy && input_mint != base_mint {
        return Err(format!(
            "input_mint {} matches neither base {} nor quote {} in Pumpfun pool {}",
            input_mint, base_mint, quote_mint, pool_address
        ).into());
    }

    let (base_token_program, quote_token_program) = if is_buy {
        (out_program, in_program)
    } else {
        (in_program, out_program)
    };

    build_pumpfun_swap(
        &PumpfunSwapAccounts {
            pool: pool_address,
            base_mint,
            quote_mint,
            pool_base_vault: pool_base_token_account,
            pool_quote_vault: pool_quote_token_account,
            user_base_token: if is_buy { user_token_out } else { user_token_in },
            user_quote_token: if is_buy { user_token_in } else { user_token_out },
            user,
            base_token_program,
            quote_token_program,
        },
        amount_in,
        min_amount_out,
        is_buy,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_pubkey(seed: u8) -> Pubkey {
        Pubkey::new_from_array([seed; 32])
    }

    #[test]
    fn clmm_exact_input_encoding() {
        let accounts = ClmmSwapAccounts {
            pool: dummy_pubkey(1), amm_config: dummy_pubkey(2),
            input_vault: dummy_pubkey(3), output_vault: dummy_pubkey(4),
            observation: dummy_pubkey(5), input_mint: dummy_pubkey(6),
            output_mint: dummy_pubkey(7), user_input_token: dummy_pubkey(8),
            user_output_token: dummy_pubkey(9), user: dummy_pubkey(10),
            input_token_program: dummy_pubkey(11), output_token_program: dummy_pubkey(12),
            tick_arrays: vec![dummy_pubkey(13)],
        };
        let ix = build_clmm_swap(&accounts, 1000, 900, 0, false).unwrap();
        // disc(8) + amount(8) + threshold(8) + sqrt_price_limit(16) + is_base_input(1) = 41
        assert_eq!(ix.data.len(), 41);
        let amount = u64::from_le_bytes(ix.data[8..16].try_into().unwrap());
        let threshold = u64::from_le_bytes(ix.data[16..24].try_into().unwrap());
        let is_base_input = ix.data[40];
        assert_eq!(amount, 1000, "ExactIn: amount should be input");
        assert_eq!(threshold, 900, "ExactIn: threshold should be min output");
        assert_eq!(is_base_input, 1, "ExactIn: is_base_input should be true");
    }

    #[test]
    fn clmm_exact_output_encoding() {
        let accounts = ClmmSwapAccounts {
            pool: dummy_pubkey(1), amm_config: dummy_pubkey(2),
            input_vault: dummy_pubkey(3), output_vault: dummy_pubkey(4),
            observation: dummy_pubkey(5), input_mint: dummy_pubkey(6),
            output_mint: dummy_pubkey(7), user_input_token: dummy_pubkey(8),
            user_output_token: dummy_pubkey(9), user: dummy_pubkey(10),
            input_token_program: dummy_pubkey(11), output_token_program: dummy_pubkey(12),
            tick_arrays: vec![dummy_pubkey(13)],
        };
        // exact_output=true: amount_in=1000 (max input), min_amount_out=900 (exact output desired)
        let ix = build_clmm_swap(&accounts, 1000, 900, 0, true).unwrap();
        let amount = u64::from_le_bytes(ix.data[8..16].try_into().unwrap());
        let threshold = u64::from_le_bytes(ix.data[16..24].try_into().unwrap());
        let is_base_input = ix.data[40];
        // Fields flip: amount = exact output (900), threshold = max input (1000)
        assert_eq!(amount, 900, "ExactOut: amount should be exact output");
        assert_eq!(threshold, 1000, "ExactOut: threshold should be max input");
        assert_eq!(is_base_input, 0, "ExactOut: is_base_input should be false");
    }

    #[test]
    fn ray_v4_exact_input_encoding() {
        let accounts = RayV4SwapAccounts {
            pool: dummy_pubkey(1), base_vault: dummy_pubkey(2),
            quote_vault: dummy_pubkey(3), user_token_in: dummy_pubkey(4),
            user_token_out: dummy_pubkey(5), user: dummy_pubkey(6),
        };
        let ix = build_ray_v4_swap(&accounts, 5000, 4500, false).unwrap();
        assert_eq!(ix.data[0], 9, "ExactIn: disc should be 9 (swap_base_in)");
        let amount_in = u64::from_le_bytes(ix.data[1..9].try_into().unwrap());
        let min_out = u64::from_le_bytes(ix.data[9..17].try_into().unwrap());
        assert_eq!(amount_in, 5000);
        assert_eq!(min_out, 4500);
    }

    #[test]
    fn ray_v4_exact_output_encoding() {
        let accounts = RayV4SwapAccounts {
            pool: dummy_pubkey(1), base_vault: dummy_pubkey(2),
            quote_vault: dummy_pubkey(3), user_token_in: dummy_pubkey(4),
            user_token_out: dummy_pubkey(5), user: dummy_pubkey(6),
        };
        // exact_output=true: amount_in=5000 (max input), min_amount_out=4500 (exact output)
        let ix = build_ray_v4_swap(&accounts, 5000, 4500, true).unwrap();
        assert_eq!(ix.data[0], 11, "ExactOut: disc should be 11 (swap_base_out)");
        // Same struct layout, different semantics for swap_base_out:
        // field1 = max_amount_in (5000), field2 = exact_amount_out (4500)
        let field1 = u64::from_le_bytes(ix.data[1..9].try_into().unwrap());
        let field2 = u64::from_le_bytes(ix.data[9..17].try_into().unwrap());
        assert_eq!(field1, 5000, "ExactOut: field1 should be max input");
        assert_eq!(field2, 4500, "ExactOut: field2 should be exact output");
    }
}