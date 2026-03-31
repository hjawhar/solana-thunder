//! Core trait definitions for the DEX aggregator.

use std::collections::HashMap;
use std::error::Error;

use serde::{Deserialize, Serialize};
use solana_pubkey::Pubkey;
use solana_sdk::instruction::Instruction;

/// All trait methods use this as the error type.
pub type GenericError = Box<dyn Error + Send + Sync>;

// ============================================================================
// Core Data Structures
// ============================================================================

/// Generic swap arguments (works for all DEXs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapArgs {
    /// Amount of input token (in raw units, not decimal-adjusted).
    /// When `exact_output=true`, this is the MAXIMUM input (spending cap).
    pub amount_in: u64,
    /// Minimum amount of output token (slippage protection).
    /// When `exact_output=true`, this is the EXACT output desired.
    pub min_amount_out: u64,
    /// When true, use "exact output" mode: get exactly `min_amount_out` tokens,
    /// spend at most `amount_in`. Not all DEXs support this.
    #[serde(default)]
    pub exact_output: bool,
}

impl SwapArgs {
    /// Create SwapArgs with a fixed minimum output (exact input mode).
    pub fn new(amount_in: u64, min_amount_out: u64) -> Self {
        Self {
            amount_in,
            min_amount_out,
            exact_output: false,
        }
    }

    /// Create SwapArgs for exact output mode: get exactly `amount_out` tokens,
    /// spend at most `max_amount_in`.
    pub fn exact_output(amount_out: u64, max_amount_in: u64) -> Self {
        Self {
            amount_in: max_amount_in,
            min_amount_out: amount_out,
            exact_output: true,
        }
    }

    /// Create SwapArgs with calculated slippage tolerance.
    pub fn with_slippage(amount_in: u64, expected_output: u64, slippage_bps: u64) -> Self {
        let min_amount_out = calculate_min_amount_out(expected_output, slippage_bps);
        Self {
            amount_in,
            min_amount_out,
            exact_output: false,
        }
    }

    /// Create SwapArgs accepting any output (use with caution — no slippage protection).
    pub fn no_slippage_check(amount_in: u64) -> Self {
        Self {
            amount_in,
            min_amount_out: 1,
            exact_output: false,
        }
    }
}

/// Direction of the swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SwapDirection {
    /// Buy: Quote → Base (SOL → Token)
    Buy,
    /// Sell: Base → Quote (Token → SOL)
    Sell,
}

/// Pool financial state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolFinancials {
    pub quote_balance: u64,
    pub base_balance: u64,
    pub quote_decimals: u8,
    pub base_decimals: u8,
}

/// Fee structure for a pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolFees {
    /// Trading fee as basis points (100 = 1%).
    pub trade_fee_bps: u64,
    /// Protocol fee (if applicable).
    pub protocol_fee_bps: Option<u64>,
}

/// Generic pool metadata (common across all DEXs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolMetadata {
    /// Pool address.
    pub address: String,
    /// DEX name (e.g., "Raydium AMM V4").
    pub dex_name: String,
    /// Quote token mint (usually WSOL).
    pub quote_mint: Pubkey,
    /// Base token mint.
    pub base_mint: Pubkey,
    /// Quote token vault.
    pub quote_vault: Pubkey,
    /// Base token vault.
    pub base_vault: Pubkey,
    /// Fee structure.
    pub fees: PoolFees,
}

// ============================================================================
// Instruction Building Types
// ============================================================================

/// Complete context needed to build swap instructions.
///
/// Contains ALL pre-fetched data that DEX implementations need.
/// The caller fetches this data (from cache/RPC), then passes it
/// to `build_swap_instruction` for pure, deterministic instruction building.
#[derive(Debug, Clone)]
pub struct SwapContext {
    /// User's wallet pubkey.
    pub user: Pubkey,
    /// Destination ATA (where output tokens go).
    pub destination_ata: Pubkey,
    /// Whether destination ATA already exists.
    pub destination_ata_exists: bool,
    /// Source ATA (where input tokens come from).
    pub source_ata: Pubkey,
    /// Whether source ATA already exists.
    pub source_ata_exists: bool,
    /// Token program ID for the token being swapped.
    pub token_program_id: Pubkey,
    /// Additional account data (DEX-specific).
    /// Key: account pubkey as string, Value: account data bytes.
    pub extra_accounts: HashMap<String, Vec<u8>>,
}

/// Declares what account data a DEX needs to build swap instructions.
///
/// DEX implementations return this via `required_accounts()` so the caller
/// knows what to fetch before calling `build_swap_instruction()`.
#[derive(Debug, Clone)]
pub struct RequiredAccounts {
    /// Mint for source ATA (the token being spent).
    pub source_mint: Pubkey,
    /// Mint for destination ATA (the token being received).
    pub destination_mint: Pubkey,
    /// Accounts whose full data must be fetched and provided in `SwapContext.extra_accounts`.
    pub account_data: Vec<Pubkey>,
}

// ============================================================================
// Core Trait: Market
// ============================================================================

/// Unified interface for interacting with any DEX on Solana.
///
/// Each DEX crate provides a struct implementing this trait. All methods are
/// synchronous and pure — no I/O, no RPC calls.
pub trait Market: Send + Sync {
    /// Get pool metadata (address, mints, vaults, fees).
    fn metadata(&self) -> Result<PoolMetadata, GenericError>;

    /// Get current pool financials (balances, decimals).
    fn financials(&self) -> Result<PoolFinancials, GenericError>;

    /// Calculate output amount for a given input (accounting for fees).
    fn calculate_output(
        &self,
        amount_in: u64,
        direction: SwapDirection,
    ) -> Result<u64, GenericError>;

    /// Calculate price impact for a swap (in basis points, 100 = 1%).
    fn calculate_price_impact(
        &self,
        amount_in: u64,
        direction: SwapDirection,
    ) -> Result<u64, GenericError>;

    /// Get the current mid-market price (quote per base, e.g. SOL per token).
    fn current_price(&self) -> Result<f64, GenericError>;

    /// Build swap instructions from pre-fetched context (pure, no I/O).
    fn build_swap_instruction(
        &self,
        context: SwapContext,
        args: SwapArgs,
        direction: SwapDirection,
    ) -> Result<Vec<Instruction>, GenericError>;

    /// Declare what account data is needed for instruction building.
    fn required_accounts(
        &self,
        user: Pubkey,
        direction: SwapDirection,
    ) -> Result<RequiredAccounts, GenericError>;

    // ========================================================================
    // Default Convenience Methods
    // ========================================================================

    /// Get quote token mint address.
    fn get_quote_mint(&self) -> Result<Pubkey, GenericError> {
        Ok(self.metadata()?.quote_mint)
    }

    /// Get base token mint address.
    fn get_base_mint(&self) -> Result<Pubkey, GenericError> {
        Ok(self.metadata()?.base_mint)
    }

    /// Calculate swap with automatic slippage tolerance.
    fn build_swap_args(
        &self,
        amount_in: u64,
        direction: SwapDirection,
        slippage_bps: u64,
    ) -> Result<SwapArgs, GenericError> {
        let expected_output = self.calculate_output(amount_in, direction)?;
        let min_amount_out = calculate_min_amount_out(expected_output, slippage_bps);
        Ok(SwapArgs {
            amount_in,
            min_amount_out,
            exact_output: false,
        })
    }

    /// Check if price impact is within an acceptable threshold.
    fn is_price_impact_acceptable(
        &self,
        amount_in: u64,
        direction: SwapDirection,
        max_impact_bps: u64,
    ) -> Result<bool, GenericError> {
        let impact = self.calculate_price_impact(amount_in, direction)?;
        Ok(impact <= max_impact_bps)
    }

    /// Get total liquidity in quote terms.
    fn total_liquidity_quote(&self) -> Result<u64, GenericError> {
        let financials = self.financials()?;
        let price = self.current_price()?;
        let base_in_quote = (financials.base_balance as f64 * price) as u64;
        Ok(financials.quote_balance + base_in_quote)
    }

    /// Heuristic max trade size that keeps impact under `max_impact_bps`.
    fn recommended_max_trade_size(
        &self,
        direction: SwapDirection,
        max_impact_bps: u64,
    ) -> Result<u64, GenericError> {
        let financials = self.financials()?;
        let pool_size = match direction {
            SwapDirection::Buy => financials.quote_balance,
            SwapDirection::Sell => financials.base_balance,
        };
        // Conservative: max_trade = pool_size * (max_impact_bps / 10000) * 0.5
        let max_trade =
            (pool_size as f64 * (max_impact_bps as f64 / 10000.0) * 0.5) as u64;
        Ok(max_trade.max(1))
    }
}

// ============================================================================
// Extension Traits
// ============================================================================

/// For DEXs that support concentrated liquidity (CLMM, DLMM).
pub trait ConcentratedLiquidity: Market {
    /// Get active bin/tick index.
    fn active_bin(&self) -> Result<i32, GenericError>;
    /// Get liquidity distribution across bins/ticks.
    fn liquidity_distribution(&self) -> Result<Vec<(i32, u64)>, GenericError>;
}

/// For DEXs with bonding curves (Pumpfun).
pub trait BondingCurve: Market {
    /// Calculate price at a given supply level.
    fn price_at_supply(&self, supply: u64) -> Result<f64, GenericError>;
    /// Check if bonding curve has graduated.
    fn is_graduated(&self) -> Result<bool, GenericError>;
}

// ============================================================================
// Shared Utilities
// ============================================================================

/// Calculate slippage-adjusted min_amount_out.
pub fn calculate_min_amount_out(expected_output: u64, slippage_bps: u64) -> u64 {
    let slippage_multiplier = 10000 - slippage_bps;
    (expected_output as u128 * slippage_multiplier as u128 / 10000) as u64
}

/// Calculate price impact in basis points (100 = 1%).
pub fn calculate_price_impact_bps(pre_swap_price: f64, post_swap_price: f64) -> u64 {
    let impact = ((post_swap_price - pre_swap_price) / pre_swap_price).abs();
    (impact * 10000.0) as u64
}

/// Standard AMM constant product formula: x * y = k.
pub fn constant_product_swap(
    reserve_in: u64,
    reserve_out: u64,
    amount_in: u64,
    fee_bps: u64,
) -> Result<u64, GenericError> {
    if reserve_in == 0 || reserve_out == 0 {
        return Err("Pool has zero liquidity".into());
    }
    let fee_multiplier = 10000 - fee_bps;
    let amount_in_with_fee = (amount_in as u128 * fee_multiplier as u128) / 10000;
    let numerator = amount_in_with_fee * reserve_out as u128;
    let denominator = reserve_in as u128 + amount_in_with_fee;
    Ok((numerator / denominator) as u64)
}
