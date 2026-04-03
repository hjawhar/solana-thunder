use solana_program::{
    account_info::AccountInfo,
    instruction::{AccountMeta, Instruction},
    msg,
    program::invoke,
    program_error::ProgramError,
};

/// Raydium AMM V4 swap discriminator (single byte = 9).
const SWAP_DISC: u8 = 9;

/// Adapter accounts layout (19 total):
///  [0]  dex_program_id
///  [1]  swap_authority_pubkey
///  [2]  swap_source_token
///  [3]  swap_destination_token
///  [4]  token_program
///  [5]  amm_id
///  [6]  amm_authority
///  [7]  amm_open_orders
///  [8]  amm_target_orders
///  [9]  pool_coin_token_account
///  [10] pool_pc_token_account
///  [11] serum_program_id
///  [12] serum_market
///  [13] serum_bids
///  [14] serum_asks
///  [15] serum_event_queue
///  [16] serum_coin_vault_account
///  [17] serum_pc_vault_account
///  [18] serum_vault_signer
const ACCOUNTS_LEN: usize = 19;

pub fn swap(accounts: &[AccountInfo], amount_in: u64) -> Result<(), ProgramError> {
    if accounts.len() < ACCOUNTS_LEN {
        msg!("Raydium V4: expected {} accounts, got {}", ACCOUNTS_LEN, accounts.len());
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let dex_program = accounts[0].key;

    // Data: disc=9u8(1) + amount_in(8) + min_out=1(8) = 17 bytes
    let mut data = Vec::with_capacity(17);
    data.push(SWAP_DISC);
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&1u64.to_le_bytes());

    // CPI metas — OKX's exact ordering (18 metas)
    let metas = vec![
        AccountMeta::new_readonly(*accounts[4].key, false),  //  0 token_program
        AccountMeta::new(*accounts[5].key, false),           //  1 amm_id
        AccountMeta::new_readonly(*accounts[6].key, false),  //  2 amm_authority
        AccountMeta::new(*accounts[7].key, false),           //  3 amm_open_orders
        AccountMeta::new(*accounts[8].key, false),           //  4 amm_target_orders
        AccountMeta::new(*accounts[9].key, false),           //  5 pool_coin_token_account
        AccountMeta::new(*accounts[10].key, false),          //  6 pool_pc_token_account
        AccountMeta::new_readonly(*accounts[11].key, false), //  7 serum_program_id
        AccountMeta::new(*accounts[12].key, false),          //  8 serum_market
        AccountMeta::new(*accounts[13].key, false),          //  9 serum_bids
        AccountMeta::new(*accounts[14].key, false),          // 10 serum_asks
        AccountMeta::new(*accounts[15].key, false),          // 11 serum_event_queue
        AccountMeta::new(*accounts[16].key, false),          // 12 serum_coin_vault_account
        AccountMeta::new(*accounts[17].key, false),          // 13 serum_pc_vault_account
        AccountMeta::new_readonly(*accounts[18].key, false), // 14 serum_vault_signer
        AccountMeta::new(*accounts[2].key, false),           // 15 swap_source_token
        AccountMeta::new(*accounts[3].key, false),           // 16 swap_destination_token
        AccountMeta::new_readonly(*accounts[1].key, true),   // 17 swap_authority (readonly, signer)
    ];

    let ix = Instruction { program_id: *dex_program, accounts: metas, data };
    invoke(&ix, accounts)?;
    Ok(())
}
