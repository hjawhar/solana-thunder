use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    account_info::AccountInfo, entrypoint,
    entrypoint::ProgramResult, msg, program_error::ProgramError, pubkey::Pubkey,
};

pub mod adapters;
use adapters::common;

entrypoint!(process_instruction);

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, Copy)]
pub enum DexType {
    MeteoraDAMMV1,  // 0
    MeteoraDAMMV2,  // 1
    MeteoraDLMM,    // 2
    RaydiumCLMM,    // 3
    RaydiumAMMV4,   // 4
    PumpfunBuy,     // 5
    PumpfunSell,    // 6
}

#[derive(BorshSerialize, BorshDeserialize, Debug)]
pub struct SwapHop {
    pub dex_type: DexType,
    pub num_accounts: u8,
}

#[derive(BorshSerialize, BorshDeserialize, Debug)]
pub struct ExecuteRouteArgs {
    pub amount_in: u64,
    pub min_amount_out: u64,
    pub hops: Vec<SwapHop>,
}

pub fn process_instruction(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let args = ExecuteRouteArgs::try_from_slice(instruction_data)
        .map_err(|_| ProgramError::InvalidInstructionData)?;

    msg!(
        "Thunder Router V3: {} hops, amount_in={}, min_out={}",
        args.hops.len(),
        args.amount_in,
        args.min_amount_out
    );

    if args.hops.is_empty() {
        return Err(ProgramError::InvalidInstructionData);
    }

    let mut offset: usize = 0;
    let mut current_amount = args.amount_in;

    for (i, hop) in args.hops.iter().enumerate() {
        let n = hop.num_accounts as usize;
        if accounts.len() < offset + n {
            msg!(
                "Hop {}: need {} accounts at offset {}, have {}",
                i,
                n,
                offset,
                accounts.len()
            );
            return Err(ProgramError::NotEnoughAccountKeys);
        }

        let hop_accounts = &accounts[offset..offset + n];

        // Uniform prefix: [3] is always swap_destination_token
        let dest_account = &hop_accounts[3];
        let balance_before = common::read_token_balance(dest_account);

        msg!(
            "Hop {}: {:?} ({} accounts, amount={})",
            i,
            hop.dex_type,
            n,
            current_amount
        );

        match hop.dex_type {
            DexType::MeteoraDAMMV1 => {
                adapters::meteora_damm_v1::swap(hop_accounts, current_amount)?
            }
            DexType::MeteoraDAMMV2 => {
                adapters::meteora_damm_v2::swap(hop_accounts, current_amount)?
            }
            DexType::MeteoraDLMM => adapters::meteora_dlmm::swap(hop_accounts, current_amount)?,
            DexType::RaydiumCLMM => adapters::raydium_clmm::swap(hop_accounts, current_amount)?,
            DexType::RaydiumAMMV4 => adapters::raydium_v4::swap(hop_accounts, current_amount)?,
            DexType::PumpfunBuy => adapters::pumpfun::buy(hop_accounts, current_amount)?,
            DexType::PumpfunSell => adapters::pumpfun::sell(hop_accounts, current_amount)?,
        };

        let balance_after = common::read_token_balance(dest_account);
        current_amount = balance_after.saturating_sub(balance_before);

        msg!("Hop {}: output={}", i, current_amount);
        offset += n;
    }

    if current_amount < args.min_amount_out {
        msg!(
            "Slippage exceeded: got {}, minimum {}",
            current_amount,
            args.min_amount_out
        );
        return Err(ProgramError::Custom(1));
    }

    msg!("Route complete: final_output={}", current_amount);
    Ok(())
}
