use solana_program::{
    account_info::AccountInfo,
    instruction::{AccountMeta, Instruction},
    msg,
    program::invoke,
    program_error::ProgramError,
};

const PUMPFUN_BUY_DISC: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
const PUMPFUN_SELL_DISC: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];

/// Adapter accounts layout (13 total):
///  [0]  dex_program_id
///  [1]  swap_authority_pubkey
///  [2]  swap_source_token
///  [3]  swap_destination_token
///  [4]  pool
///  [5]  base_mint
///  [6]  quote_mint
///  [7]  pool_base_token_account
///  [8]  pool_quote_token_account
///  [9]  base_token_program
///  [10] quote_token_program
///  [11] system_program
///  [12] event_authority
const ACCOUNTS_LEN: usize = 13;

/// Build CPI metas and instruction data, then invoke.
/// CPI layout is the same for buy and sell — only the discriminator differs.
fn swap_inner(accounts: &[AccountInfo], amount_in: u64, disc: &[u8; 8]) -> Result<(), ProgramError> {
    if accounts.len() < ACCOUNTS_LEN {
        msg!("Pumpfun: expected {} accounts, got {}", ACCOUNTS_LEN, accounts.len());
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let dex_program = accounts[0].key;

    // Data: disc(8) + amount_in(8) + min_out=1(8) = 24 bytes
    let mut data = Vec::with_capacity(24);
    data.extend_from_slice(disc);
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&1u64.to_le_bytes());

    // CPI metas (13 metas)
    let metas = vec![
        AccountMeta::new(*accounts[4].key, false),           //  0 pool
        AccountMeta::new(*accounts[1].key, true),            //  1 swap_authority (writable, signer)
        AccountMeta::new(*accounts[5].key, false),           //  2 base_mint
        AccountMeta::new(*accounts[6].key, false),           //  3 quote_mint
        AccountMeta::new(*accounts[2].key, false),           //  4 swap_source_token
        AccountMeta::new(*accounts[3].key, false),           //  5 swap_destination_token
        AccountMeta::new(*accounts[7].key, false),           //  6 pool_base_token_account
        AccountMeta::new(*accounts[8].key, false),           //  7 pool_quote_token_account
        AccountMeta::new_readonly(*accounts[9].key, false),  //  8 base_token_program
        AccountMeta::new_readonly(*accounts[10].key, false), //  9 quote_token_program
        AccountMeta::new_readonly(*accounts[11].key, false), // 10 system_program
        AccountMeta::new_readonly(*accounts[12].key, false), // 11 event_authority
        AccountMeta::new_readonly(*dex_program, false),      // 12 program (Anchor self-ref)
    ];

    let ix = Instruction { program_id: *dex_program, accounts: metas, data };
    invoke(&ix, accounts)?;
    Ok(())
}

pub fn buy(accounts: &[AccountInfo], amount_in: u64) -> Result<(), ProgramError> {
    swap_inner(accounts, amount_in, &PUMPFUN_BUY_DISC)
}

pub fn sell(accounts: &[AccountInfo], amount_in: u64) -> Result<(), ProgramError> {
    swap_inner(accounts, amount_in, &PUMPFUN_SELL_DISC)
}
