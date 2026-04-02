//! Thunder Router: on-chain multi-hop swap program.
//!
//! Single instruction: `execute_route`. Accepts a list of swap hops as
//! instruction data. For each hop, CPIs into the target DEX program,
//! reads the actual output amount from the destination token account,
//! and chains it as the input to the next hop.
//!
//! Accounts are passed as remaining accounts, split by hop.

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    account_info::AccountInfo,
    entrypoint,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    msg,
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
};

entrypoint!(process_instruction);

// ---------------------------------------------------------------------------
// Instruction data
// ---------------------------------------------------------------------------

/// A single hop in the multi-hop route.
#[derive(BorshSerialize, BorshDeserialize, Debug)]
pub struct SwapHop {
    /// DEX program ID to CPI into.
    pub program_id: Pubkey,
    /// Pre-built instruction data (discriminator + args).
    /// The router patches amount_in and min_amount_out at the specified offsets.
    pub instruction_data: Vec<u8>,
    /// Number of accounts this hop uses (from remaining_accounts).
    pub num_accounts: u8,
    /// For each account: (is_signer: bool, is_writable: bool).
    pub account_metas: Vec<(bool, bool)>,
    /// Offset in instruction_data where amount_in (u64 LE) should be patched.
    pub amount_in_offset: u16,
    /// Offset in instruction_data where min_amount_out (u64 LE) should be patched.
    pub min_amount_out_offset: u16,
    /// Index into this hop's accounts that points to the output token account.
    /// After the CPI, the router reads this account's balance to determine
    /// the actual output amount for chaining to the next hop.
    pub output_account_index: u8,
}

/// Top-level instruction: execute a multi-hop swap route.
#[derive(BorshSerialize, BorshDeserialize, Debug)]
pub struct ExecuteRouteArgs {
    /// Initial input amount (raw lamports/token units).
    pub amount_in: u64,
    /// Minimum final output (slippage protection on the last hop).
    pub min_amount_out: u64,
    /// The swap hops to execute in order.
    pub hops: Vec<SwapHop>,
}

// ---------------------------------------------------------------------------
// Entrypoint
// ---------------------------------------------------------------------------

pub fn process_instruction(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let args = ExecuteRouteArgs::try_from_slice(instruction_data)
        .map_err(|_| ProgramError::InvalidInstructionData)?;

    msg!(
        "Thunder Router: {} hops, amount_in={}, min_out={}",
        args.hops.len(),
        args.amount_in,
        args.min_amount_out
    );

    if args.hops.is_empty() {
        msg!("No hops provided");
        return Ok(());
    }

    let mut remaining = accounts;
    let mut current_amount = args.amount_in;

    for (i, hop) in args.hops.iter().enumerate() {
        let n = hop.num_accounts as usize;
        if remaining.len() < n {
            msg!("Hop {}: need {} accounts, have {}", i, n, remaining.len());
            return Err(ProgramError::NotEnoughAccountKeys);
        }

        let (hop_accounts, rest) = remaining.split_at(n);
        remaining = rest;

        // Read the output token account balance BEFORE the swap.
        let output_idx = hop.output_account_index as usize;
        let balance_before = if output_idx < hop_accounts.len() {
            read_token_balance(&hop_accounts[output_idx])
        } else {
            0
        };

        // Build the CPI instruction data with patched amounts.
        let mut data = hop.instruction_data.clone();

        // Patch amount_in with the actual chained amount.
        let ain_off = hop.amount_in_offset as usize;
        if ain_off + 8 <= data.len() {
            data[ain_off..ain_off + 8].copy_from_slice(&current_amount.to_le_bytes());
        }

        // Patch min_amount_out: last hop uses user's slippage, intermediates use 1.
        let min_out = if i == args.hops.len() - 1 {
            args.min_amount_out
        } else {
            1u64
        };
        let mout_off = hop.min_amount_out_offset as usize;
        if mout_off + 8 <= data.len() {
            data[mout_off..mout_off + 8].copy_from_slice(&min_out.to_le_bytes());
        }

        // Build AccountMetas for the CPI.
        let account_metas: Vec<AccountMeta> = hop_accounts
            .iter()
            .zip(hop.account_metas.iter())
            .map(|(ai, (is_signer, is_writable))| {
                if *is_writable {
                    AccountMeta::new(*ai.key, *is_signer)
                } else {
                    AccountMeta::new_readonly(*ai.key, *is_signer)
                }
            })
            .collect();

        let ix = Instruction {
            program_id: hop.program_id,
            accounts: account_metas,
            data,
        };

        msg!("Hop {}: CPI into {} ({} accounts, amount={})",
            i, hop.program_id, n, current_amount);

        // Execute the CPI.
        invoke(&ix, hop_accounts)?;

        // Read the output token account balance AFTER the swap.
        // The delta is the actual amount received from this hop.
        if output_idx < hop_accounts.len() {
            let balance_after = read_token_balance(&hop_accounts[output_idx]);
            let output = balance_after.saturating_sub(balance_before);
            msg!("Hop {}: output={} (balance {} -> {})",
                i, output, balance_before, balance_after);
            current_amount = output;
        }
    }

    // Verify final output meets minimum (slippage protection).
    if current_amount < args.min_amount_out {
        msg!(
            "Slippage exceeded: got {}, minimum {}",
            current_amount, args.min_amount_out
        );
        return Err(ProgramError::Custom(1)); // SlippageExceeded
    }

    msg!("Route complete: final_output={}", current_amount);
    Ok(())
}

/// Read the SPL token balance from an account's data (bytes 64-72).
fn read_token_balance(account: &AccountInfo) -> u64 {
    let data = account.try_borrow_data().ok();
    match data {
        Some(d) if d.len() >= 72 => u64::from_le_bytes(d[64..72].try_into().unwrap()),
        _ => 0,
    }
}
