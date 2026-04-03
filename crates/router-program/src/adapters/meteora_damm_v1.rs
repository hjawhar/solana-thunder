use solana_program::{
    account_info::AccountInfo,
    instruction::{AccountMeta, Instruction},
    msg,
    program::invoke,
    program_error::ProgramError,
};

/// Anchor swap discriminator shared with DAMM V2.
const SWAP_DISC: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];

/// Adapter accounts layout (16 total):
///  [0]  dex_program_id
///  [1]  swap_authority_pubkey  (signer)
///  [2]  swap_source_token
///  [3]  swap_destination_token
///  [4]  pool
///  [5]  a_vault
///  [6]  b_vault
///  [7]  a_token_vault
///  [8]  b_token_vault
///  [9]  a_vault_lp_mint
///  [10] b_vault_lp_mint
///  [11] a_vault_lp
///  [12] b_vault_lp
///  [13] admin_token_fee
///  [14] vault_program
///  [15] token_program
const ACCOUNTS_LEN: usize = 16;

pub fn swap(accounts: &[AccountInfo], amount_in: u64) -> Result<(), ProgramError> {
    if accounts.len() < ACCOUNTS_LEN {
        msg!("DAMM V1: expected {} accounts, got {}", ACCOUNTS_LEN, accounts.len());
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let dex_program = accounts[0].key;

    // Data: disc(8) + amount_in(8) + min_out=1(8) = 24 bytes
    let mut data = Vec::with_capacity(24);
    data.extend_from_slice(&SWAP_DISC);
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&1u64.to_le_bytes());

    // CPI metas — OKX's exact ordering (15 metas)
    let metas = vec![
        AccountMeta::new(*accounts[4].key, false),           //  0 pool
        AccountMeta::new(*accounts[2].key, false),           //  1 swap_source_token
        AccountMeta::new(*accounts[3].key, false),           //  2 swap_destination_token
        AccountMeta::new(*accounts[5].key, false),           //  3 a_vault
        AccountMeta::new(*accounts[6].key, false),           //  4 b_vault
        AccountMeta::new(*accounts[7].key, false),           //  5 a_token_vault
        AccountMeta::new(*accounts[8].key, false),           //  6 b_token_vault
        AccountMeta::new(*accounts[9].key, false),           //  7 a_vault_lp_mint
        AccountMeta::new(*accounts[10].key, false),          //  8 b_vault_lp_mint
        AccountMeta::new(*accounts[11].key, false),          //  9 a_vault_lp
        AccountMeta::new(*accounts[12].key, false),          // 10 b_vault_lp
        AccountMeta::new(*accounts[13].key, false),          // 11 admin_token_fee
        AccountMeta::new_readonly(*accounts[1].key, true),   // 12 swap_authority (readonly, signer)
        AccountMeta::new_readonly(*accounts[14].key, false), // 13 vault_program
        AccountMeta::new_readonly(*accounts[15].key, false), // 14 token_program
    ];

    let ix = Instruction { program_id: *dex_program, accounts: metas, data };
    invoke(&ix, accounts)?;
    Ok(())
}
