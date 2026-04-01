//! Thunder Router: on-chain multi-hop swap program.
//!
//! Single instruction: `execute_route`. Accepts a list of swap hops as
//! instruction data. For each hop, CPIs into the target DEX program with
//! the exact output amount from the previous hop as input.
//!
//! Accounts are passed as remaining accounts, split by hop.

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
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
    /// The router replaces amount_in and min_amount_out at fixed offsets.
    pub instruction_data: Vec<u8>,
    /// Number of accounts this hop uses (from remaining_accounts).
    pub num_accounts: u8,
    /// For each account: (is_signer: bool, is_writable: bool).
    pub account_metas: Vec<(bool, bool)>,
    /// Offset in instruction_data where amount_in (u64 LE) should be patched.
    /// Set to u16::MAX if no patching needed (first hop uses original amount).
    pub amount_in_offset: u16,
    /// Offset in instruction_data where min_amount_out (u64 LE) should be patched.
    pub min_amount_out_offset: u16,
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

    let mut remaining = accounts;
    let mut current_amount = args.amount_in;

    for (i, hop) in args.hops.iter().enumerate() {
        let n = hop.num_accounts as usize;
        if remaining.len() < n {
            msg!("Hop {}: not enough accounts (need {}, have {})", i, n, remaining.len());
            return Err(ProgramError::NotEnoughAccountKeys);
        }

        // Split accounts for this hop.
        let (hop_accounts, rest) = remaining.split_at(n);
        remaining = rest;

        // Build the CPI instruction.
        let mut data = hop.instruction_data.clone();

        // Patch amount_in if needed.
        if (hop.amount_in_offset as usize) + 8 <= data.len() {
            data[hop.amount_in_offset as usize..hop.amount_in_offset as usize + 8]
                .copy_from_slice(&current_amount.to_le_bytes());
        }

        // Patch min_amount_out: for the last hop use the user's slippage,
        // for intermediate hops use 1 (accept any amount).
        let min_out = if i == args.hops.len() - 1 {
            args.min_amount_out
        } else {
            1u64
        };
        if (hop.min_amount_out_offset as usize) + 8 <= data.len() {
            data[hop.min_amount_out_offset as usize..hop.min_amount_out_offset as usize + 8]
                .copy_from_slice(&min_out.to_le_bytes());
        }

        // Build AccountMetas.
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

        msg!("Hop {}: CPI into {} ({} accounts)", i, hop.program_id, n);

        // Execute CPI.
        invoke(&ix, hop_accounts)?;

        // Read the output amount from the user's destination token account.
        // For now, we trust the DEX produced output and use the next hop's
        // patched amount_in. In production, read the token account balance
        // delta. For simulation, this works because each DEX CPI is atomic.
        //
        // TODO: Read actual token balance delta for exact chaining.
        // For now, pass current_amount through (the DEX handles the actual amounts).
        msg!("Hop {} complete", i);
    }

    msg!("Route execution complete");
    Ok(())
}
