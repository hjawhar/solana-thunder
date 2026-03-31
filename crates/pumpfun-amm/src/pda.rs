use solana_pubkey::Pubkey;
use spl_associated_token_account::get_associated_token_address;

use crate::{PUMPFUN_AMM_PROGRAM, PUMPFUN_FEE_PROGRAM, PUMPFUN_PROGRAM};
use thunder_core::WSOL;

pub fn get_pumpfun_pool_authority_pda(base_mint: Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &["pool-authority".as_ref(), base_mint.as_ref()],
        &Pubkey::from_str_const(PUMPFUN_PROGRAM),
    )
    .0
}

pub fn get_pumpfun_amm_pool(base_mint: Pubkey) -> Pubkey {
    let pool_authority = get_pumpfun_pool_authority_pda(base_mint);
    Pubkey::find_program_address(
        &[
            "pool".as_ref(),
            &0_i32.to_le_bytes()[0..2],
            pool_authority.as_array(),
            base_mint.as_ref(),
            Pubkey::from_str_const(WSOL).as_ref(),
        ],
        &Pubkey::from_str_const(PUMPFUN_AMM_PROGRAM),
    )
    .0
}

pub fn get_pumpfun_creator_vault_authority_pda(coin_creator: Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &["creator_vault".as_ref(), coin_creator.as_ref()],
        &Pubkey::from_str_const(PUMPFUN_AMM_PROGRAM),
    )
    .0
}

pub fn get_pumpfun_creator_vault_ata(vault_authority: Pubkey, mint: Pubkey) -> Pubkey {
    get_associated_token_address(&vault_authority, &mint)
}

pub fn get_bonding_curve_pda(mint: Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &["bonding-curve".as_ref(), mint.as_ref()],
        &Pubkey::from_str_const(PUMPFUN_PROGRAM),
    )
    .0
}

pub fn get_global_volume_accumulator_pda() -> Pubkey {
    Pubkey::find_program_address(
        &["global_volume_accumulator".as_ref()],
        &Pubkey::from_str_const(PUMPFUN_AMM_PROGRAM),
    )
    .0
}

pub fn get_user_volume_accumulator_pda(user: Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &["user_volume_accumulator".as_ref(), user.as_ref()],
        &Pubkey::from_str_const(PUMPFUN_AMM_PROGRAM),
    )
    .0
}

pub fn get_user_volume_accumulator_wsol_ata(user: Pubkey) -> Pubkey {
    let uva_pda = get_user_volume_accumulator_pda(user);
    get_associated_token_address(&uva_pda, &Pubkey::from_str_const(WSOL))
}

pub fn get_pool_v2_pda(base_mint: Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &["pool-v2".as_ref(), base_mint.as_ref()],
        &Pubkey::from_str_const(PUMPFUN_AMM_PROGRAM),
    )
    .0
}

pub fn get_pumpfun_config_pda() -> Pubkey {
    Pubkey::find_program_address(
        &[
            "fee_config".as_ref(),
            Pubkey::from_str_const(PUMPFUN_AMM_PROGRAM).as_ref(),
        ],
        &Pubkey::from_str_const(PUMPFUN_FEE_PROGRAM),
    )
    .0
}
