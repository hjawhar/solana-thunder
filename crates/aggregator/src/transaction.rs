//! Versioned transaction builder for multi-hop swap routes.
//!
//! Composes per-hop swap instructions into a single `VersionedTransaction` (v0).
//! Each hop's `build_swap_instruction` handles its own WSOL wrapping/unwrapping
//! and ATA creation, so multi-hop works by concatenating instruction sequences.

use std::collections::HashMap;

use solana_pubkey::Pubkey;
#[allow(deprecated)] // solana-sdk 3.0 re-exports are deprecated in favor of subcrates
use solana_sdk::{
    hash::Hash,
    message::{v0, VersionedMessage},
    signature::Signature,
    transaction::VersionedTransaction,
};
use spl_associated_token_account::get_associated_token_address;
use thunder_core::{calculate_min_amount_out, GenericError, SwapArgs, SwapContext, SwapDirection, TOKEN_PROGRAM};

use crate::pool_index::PoolIndex;
use crate::types::Route;

/// Build a v0 `VersionedTransaction` from a computed [`Route`].
///
/// The transaction is unsigned — the caller must sign with the user's keypair
/// before submitting.
///
/// # Errors
///
/// Returns an error if:
/// - The route has zero hops.
/// - A referenced pool is missing from the index.
/// - Any hop's `build_swap_instruction` fails.
/// - The v0 message fails to compile (e.g., too many accounts).
pub fn build_versioned_transaction(
    route: &Route,
    user: Pubkey,
    slippage_bps: u64,
    index: &PoolIndex,
    recent_blockhash: Hash,
) -> Result<VersionedTransaction, GenericError> {
    if route.hops.is_empty() {
        return Err("Route has no hops".into());
    }

    let mut all_instructions = Vec::new();
    let hop_count = route.hops.len();

    for (i, hop) in route.hops.iter().enumerate() {
        let pool = index
            .get_pool(&hop.pool_address)
            .ok_or_else(|| format!("Pool {} not found in index", hop.pool_address))?;

        let meta = pool.market.metadata()?;

        // Direction is determined by which mint the hop sends in.
        // Buy = spending quote to get base; Sell = spending base to get quote.
        let direction = if hop.input_mint == meta.quote_mint {
            SwapDirection::Buy
        } else {
            SwapDirection::Sell
        };

        let source_ata = get_associated_token_address(&user, &hop.input_mint);
        let destination_ata = get_associated_token_address(&user, &hop.output_mint);

        // For intermediate hops (i > 0), the source ATA was created or funded
        // by the previous hop's instruction set. For hop 0, the user must
        // already hold the input token — so source_ata_exists = true.
        //
        // destination_ata_exists = false lets each DEX's build_swap_instruction
        // emit a create-ATA-if-needed instruction.
        let context = SwapContext {
            user,
            source_ata,
            source_ata_exists: true,
            destination_ata,
            destination_ata_exists: i > 0, // intermediate ATAs created by prior hop
            token_program_id: Pubkey::from_str_const(TOKEN_PROGRAM),
            extra_accounts: HashMap::new(),
        };

        // Apply slippage only to the final hop. Intermediate hops accept any
        // output (min_amount_out = 1) because the exact intermediate amount is
        // unknown at build time — the final hop's slippage guard protects the
        // user's total outcome.
        let min_out = if i == hop_count - 1 {
            calculate_min_amount_out(hop.output_amount, slippage_bps)
        } else {
            1
        };

        let args = SwapArgs::new(hop.input_amount, min_out);
        let instructions = pool.market.build_swap_instruction(context, args, direction)?;
        all_instructions.extend(instructions);
    }

    // Compile into a v0 message (no address lookup tables in MVP).
    #[allow(deprecated)]
    let message = v0::Message::try_compile(&user, &all_instructions, &[], recent_blockhash)?;

    Ok(VersionedTransaction {
        signatures: vec![Signature::default()],
        message: VersionedMessage::V0(message),
    })
}
