use solana_pubkey::Pubkey;
use solana_sdk::pubkey;

use crate::METEORA_VAULT_PROGRAM;

pub static VAULT_PREFIX: &str = "vault";
pub static TOKEN_VAULT_PREFIX: &str = "token_vault";
pub static LP_MINT_PREFIX: &str = "lp_mint";
pub static COLLATERAL_VAULT_PREFIX: &str = "collateral_vault";

pub static VAULT_BASE_KEY: Pubkey = pubkey!("HWzXGcGHy4tcpYfaRDCyLNzXqBTv3E6BttpCH2vJxArv");

pub fn derive_vault_address(token_mint: Pubkey, base: Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[VAULT_PREFIX.as_ref(), token_mint.as_ref(), base.as_ref()],
        &Pubkey::from_str_const(METEORA_VAULT_PROGRAM),
    )
}

pub fn derive_token_vault_address(vault: Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[TOKEN_VAULT_PREFIX.as_ref(), vault.as_ref()],
        &Pubkey::from_str_const(METEORA_VAULT_PROGRAM),
    )
}

pub fn derive_strategy_address(vault: Pubkey, reserve: Pubkey, index: u8) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[vault.as_ref(), reserve.as_ref(), &[index]],
        &Pubkey::from_str_const(METEORA_VAULT_PROGRAM),
    )
}

pub fn derive_collateral_vault_address(strategy: Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[COLLATERAL_VAULT_PREFIX.as_ref(), strategy.as_ref()],
        &Pubkey::from_str_const(METEORA_VAULT_PROGRAM),
    )
}

pub fn derive_token_lp_mint(vault: Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[LP_MINT_PREFIX.as_ref(), vault.as_ref()],
        &Pubkey::from_str_const(METEORA_VAULT_PROGRAM),
    )
}
