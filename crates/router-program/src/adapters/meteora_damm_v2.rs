use solana_program::{
    account_info::AccountInfo,
    instruction::{AccountMeta, Instruction},
    msg,
    program::invoke,
    program_error::ProgramError,
};

/// Anchor swap discriminator shared with DAMM V1.
const SWAP_DISC: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];

/// Adapter accounts layout (13 total):
///  [0]  dex_program_id
///  [1]  swap_authority_pubkey
///  [2]  swap_source_token
///  [3]  swap_destination_token
///  [4]  pool_authority
///  [5]  pool
///  [6]  token_a_vault
///  [7]  token_b_vault
///  [8]  token_a_mint
///  [9]  token_b_mint
///  [10] token_a_program
///  [11] token_b_program
///  [12] event_authority
const ACCOUNTS_LEN: usize = 13;

pub fn swap(accounts: &[AccountInfo], amount_in: u64) -> Result<(), ProgramError> {
    if accounts.len() < ACCOUNTS_LEN {
        msg!("DAMM V2: expected {} accounts, got {}", ACCOUNTS_LEN, accounts.len());
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let dex_program = accounts[0].key;

    // Data: disc(8) + amount_in(8) + min_out=1(8) = 24 bytes
    let mut data = Vec::with_capacity(24);
    data.extend_from_slice(&SWAP_DISC);
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&1u64.to_le_bytes());

    // CPI metas — OKX's exact ordering (14 metas)
    let metas = vec![
        AccountMeta::new_readonly(*accounts[4].key, false),  //  0 pool_authority
        AccountMeta::new(*accounts[5].key, false),           //  1 pool
        AccountMeta::new(*accounts[2].key, false),           //  2 input_token (swap_source)
        AccountMeta::new(*accounts[3].key, false),           //  3 output_token (swap_dest)
        AccountMeta::new(*accounts[6].key, false),           //  4 token_a_vault
        AccountMeta::new(*accounts[7].key, false),           //  5 token_b_vault
        AccountMeta::new_readonly(*accounts[8].key, false),  //  6 token_a_mint
        AccountMeta::new_readonly(*accounts[9].key, false),  //  7 token_b_mint
        AccountMeta::new(*accounts[1].key, true),            //  8 swap_authority (writable, signer)
        AccountMeta::new_readonly(*accounts[10].key, false), //  9 token_a_program
        AccountMeta::new_readonly(*accounts[11].key, false), // 10 token_b_program
        AccountMeta::new_readonly(*dex_program, false),      // 11 referral (sentinel = dex program)
        AccountMeta::new_readonly(*accounts[12].key, false), // 12 event_authority
        AccountMeta::new_readonly(*dex_program, false),      // 13 program (Anchor self-ref)
    ];

    let ix = Instruction { program_id: *dex_program, accounts: metas, data };
    invoke(&ix, accounts)?;
    Ok(())
}
