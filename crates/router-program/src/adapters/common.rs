use solana_program::account_info::AccountInfo;

/// Read the token balance from a token account's raw data.
/// SPL Token accounts store `amount` at bytes [64..72] as little-endian u64.
pub fn read_token_balance(account: &AccountInfo) -> u64 {
    let data = account.try_borrow_data().ok();
    match data {
        Some(d) if d.len() >= 72 => u64::from_le_bytes(d[64..72].try_into().unwrap()),
        _ => 0,
    }
}
