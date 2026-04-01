pub mod models;
pub mod utils;

pub use models::{
    MeteoraDAMMPool, MeteoraDAMMV2Pool, VaultAuthority, CurveType, Config,
    V2PoolFees, BaseFee, PoolMetrics, DynamicFee, RewardInfo,
};
pub use utils::{
    derive_vault_address, derive_token_vault_address, derive_strategy_address,
    derive_collateral_vault_address, derive_token_lp_mint, VAULT_BASE_KEY,
};

use borsh::BorshDeserialize;
use solana_pubkey::Pubkey;
use solana_sdk::instruction::{AccountMeta, Instruction};
use thunder_core::{
    GenericError, Market, PoolFinancials, PoolFees, PoolMetadata, RequiredAccounts, SwapArgs,
    SwapContext, SwapDirection, calculate_price_impact_bps, constant_product_swap, infer_mint_decimals, quote_priority,
};

// =============================================================================
// Constants
// =============================================================================

pub const METEORA_DYNAMIC_AMM: &str = "Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB";
pub const METEORA_DYNAMIC_AMM_V2: &str = "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG";
pub const METEORA_VAULT_PROGRAM: &str = "24Uqj9JCLxUeoC3hGfh5W3s9FM9uCHDS2SG3LYwBpyTi";

// =============================================================================
// Shared instruction args (borsh-serialized for on-chain)
// =============================================================================

#[derive(Clone, Debug, borsh::BorshSerialize)]
struct SwapInstructionArgs {
    pub in_amount: u64,
    pub minimum_out_amount: u64,
}

// =============================================================================
// V1: MeteoraDAMMMarket
// =============================================================================

/// Market wrapper for Meteora Dynamic AMM V1 pools.
pub struct MeteoraDAMMMarket {
    pub pool: MeteoraDAMMPool,
    pub pool_address: String,
    /// Cached vault balances (updated externally after fetching on-chain data).
    pub a_vault_balance: u64,
    pub b_vault_balance: u64,
    pub token_a_decimals: u8,
    pub token_b_decimals: u8,
    /// True when the pool's token_a is actually the quote currency (WSOL/USDC),
    /// meaning the default quote=token_b / base=token_a mapping is inverted.
    pub flipped: bool,
}

impl MeteoraDAMMMarket {
    pub fn new(pool: MeteoraDAMMPool, pool_address: String) -> Self {
        let flipped = quote_priority(&pool.token_a_mint).unwrap_or(usize::MAX) < quote_priority(&pool.token_b_mint).unwrap_or(usize::MAX);
        let token_a_decimals = infer_mint_decimals(&pool.token_a_mint);
        let token_b_decimals = infer_mint_decimals(&pool.token_b_mint);
        Self {
            pool,
            pool_address,
            a_vault_balance: 0,
            b_vault_balance: 0,
            token_a_decimals,
            token_b_decimals,
            flipped,
        }
    }

    /// Calculate output based on curve type (constant product or stable).
    fn calculate_damm_output(
        &self,
        amount_in: u64,
        direction: SwapDirection,
    ) -> Result<u64, GenericError> {
        let fee_bps = (self.pool.fees.trade_fee_numerator as f64
            / self.pool.fees.trade_fee_denominator as f64
            * 10000.0) as u64;

        // Normalize direction: if flipped, a Buy in market terms is physically
        // selling token_a (the physical quote) to get token_b, which is the
        // reverse of the un-flipped path.
        let physical_direction = if self.flipped {
            match direction {
                SwapDirection::Buy => SwapDirection::Sell,
                SwapDirection::Sell => SwapDirection::Buy,
            }
        } else {
            direction
        };

        match &self.pool.curve_type {
            CurveType::ConstantProduct => match physical_direction {
                SwapDirection::Buy => constant_product_swap(
                    self.b_vault_balance,
                    self.a_vault_balance,
                    amount_in,
                    fee_bps,
                ),
                SwapDirection::Sell => constant_product_swap(
                    self.a_vault_balance,
                    self.b_vault_balance,
                    amount_in,
                    fee_bps,
                ),
            },
            CurveType::Stable { amp, .. } => {
                let base_output = match physical_direction {
                    SwapDirection::Buy => constant_product_swap(
                        self.b_vault_balance,
                        self.a_vault_balance,
                        amount_in,
                        fee_bps,
                    )?,
                    SwapDirection::Sell => constant_product_swap(
                        self.a_vault_balance,
                        self.b_vault_balance,
                        amount_in,
                        fee_bps,
                    )?,
                };

                let amp_factor = (*amp as f64 / 100.0).min(10.0);
                let stable_output = (base_output as f64 * (1.0 + amp_factor / 100.0)) as u64;

                // Cap at available reserves.
                Ok(stable_output.min(match physical_direction {
                    SwapDirection::Buy => self.a_vault_balance,
                    SwapDirection::Sell => self.b_vault_balance,
                }))
            }
        }
    }
}

impl Market for MeteoraDAMMMarket {
    fn is_active(&self) -> bool {
        self.pool.enabled
    }

    fn metadata(&self) -> Result<PoolMetadata, GenericError> {
        let fee_bps = (self.pool.fees.trade_fee_numerator as f64
            / self.pool.fees.trade_fee_denominator as f64
            * 10000.0) as u64;

        let protocol_fee_bps = (self.pool.fees.protocol_trade_fee_numerator as f64
            / self.pool.fees.protocol_trade_fee_denominator as f64
            * 10000.0) as u64;

        Ok(PoolMetadata {
            address: self.pool_address.clone(),
            dex_name: match self.pool.curve_type {
                CurveType::ConstantProduct => "Meteora DAMM (Constant Product)".to_string(),
                CurveType::Stable { .. } => "Meteora DAMM (Stable)".to_string(),
            },
            quote_mint: if self.flipped { self.pool.token_a_mint } else { self.pool.token_b_mint },
            base_mint: if self.flipped { self.pool.token_b_mint } else { self.pool.token_a_mint },
            quote_vault: if self.flipped { derive_token_vault_address(self.pool.a_vault).0 } else { derive_token_vault_address(self.pool.b_vault).0 },
            base_vault: if self.flipped { derive_token_vault_address(self.pool.b_vault).0 } else { derive_token_vault_address(self.pool.a_vault).0 },
            fees: PoolFees {
                trade_fee_bps: fee_bps,
                protocol_fee_bps: Some(protocol_fee_bps),
            },
        })
    }

    fn financials(&self) -> Result<PoolFinancials, GenericError> {
        Ok(PoolFinancials {
            quote_balance: if self.flipped { self.a_vault_balance } else { self.b_vault_balance },
            base_balance: if self.flipped { self.b_vault_balance } else { self.a_vault_balance },
            quote_decimals: if self.flipped { self.token_a_decimals } else { self.token_b_decimals },
            base_decimals: if self.flipped { self.token_b_decimals } else { self.token_a_decimals },
        })
    }

    fn calculate_output(
        &self,
        amount_in: u64,
        direction: SwapDirection,
    ) -> Result<u64, GenericError> {
        self.calculate_damm_output(amount_in, direction)
    }

    fn calculate_price_impact(
        &self,
        amount_in: u64,
        direction: SwapDirection,
    ) -> Result<u64, GenericError> {
        let pre_swap_price = self.current_price()?;
        let output = self.calculate_output(amount_in, direction)?;

        // Price impact uses physical direction (already normalized in calculate_output).
        let physical_direction = if self.flipped {
            match direction {
                SwapDirection::Buy => SwapDirection::Sell,
                SwapDirection::Sell => SwapDirection::Buy,
            }
        } else {
            direction
        };

        let post_swap_price = match physical_direction {
            SwapDirection::Buy => {
                let new_b = self.b_vault_balance + amount_in;
                let new_a = self.a_vault_balance.saturating_sub(output);
                if new_a == 0 {
                    return Err("Insufficient liquidity in pool".into());
                }
                new_b as f64 / new_a as f64
            }
            SwapDirection::Sell => {
                let new_a = self.a_vault_balance + amount_in;
                let new_b = self.b_vault_balance.saturating_sub(output);
                if new_a == 0 {
                    return Err("Insufficient liquidity in pool".into());
                }
                new_b as f64 / new_a as f64
            }
        };

        Ok(calculate_price_impact_bps(pre_swap_price, post_swap_price))
    }

    fn current_price(&self) -> Result<f64, GenericError> {
        let (quote_bal, base_bal, quote_dec, base_dec) = if self.flipped {
            (self.a_vault_balance, self.b_vault_balance, self.token_a_decimals, self.token_b_decimals)
        } else {
            (self.b_vault_balance, self.a_vault_balance, self.token_b_decimals, self.token_a_decimals)
        };
        if base_bal == 0 {
            return Err("Pool has zero base balance".into());
        }
        let quote_human = quote_bal as f64 / 10f64.powi(quote_dec as i32);
        let base_human = base_bal as f64 / 10f64.powi(base_dec as i32);
        Ok(quote_human / base_human)
    }

    fn build_swap_instruction(
        &self,
        context: SwapContext,
        args: SwapArgs,
        direction: SwapDirection,
    ) -> Result<Vec<Instruction>, GenericError> {
        use borsh::to_vec;
        use spl_associated_token_account::instruction::create_associated_token_account_idempotent;

        let mut instructions = Vec::new();

        if !self.pool.enabled {
            return Err("Pool is not enabled".into());
        }

        // Derive vault token accounts (pure PDA derivation, no RPC).
        let (a_token_vault, _) = derive_token_vault_address(self.pool.a_vault);
        let (b_token_vault, _) = derive_token_vault_address(self.pool.b_vault);

        // Extract LP mints from vault authority accounts (preferred) or derive (fallback).
        let a_vault_lp_mint = extract_lp_mint_from_context(&context, &self.pool.a_vault)
            .unwrap_or_else(|| derive_token_lp_mint(self.pool.a_vault).0);

        let b_vault_lp_mint = extract_lp_mint_from_context(&context, &self.pool.b_vault)
            .unwrap_or_else(|| derive_token_lp_mint(self.pool.b_vault).0);

        // When flipped, Market Buy = physically Sell (spend token_a quote to get token_b base).
        let physical_direction = if self.flipped {
            match direction {
                SwapDirection::Buy => SwapDirection::Sell,
                SwapDirection::Sell => SwapDirection::Buy,
            }
        } else {
            direction
        };

        match physical_direction {
            SwapDirection::Buy => {
                // Buy: B (SOL) -> A (Token)
                if !context.source_ata_exists {
                    instructions.push(create_associated_token_account_idempotent(
                        &context.user,
                        &context.user,
                        &self.pool.token_b_mint,
                        &context.token_program_id,
                    ));
                }

                if !context.destination_ata_exists {
                    instructions.push(create_associated_token_account_idempotent(
                        &context.user,
                        &context.user,
                        &self.pool.token_a_mint,
                        &context.token_program_id,
                    ));
                }

                // Wrap SOL into WSOL ATA.
                instructions.push(solana_system_interface::instruction::transfer(
                    &context.user,
                    &context.source_ata,
                    args.amount_in,
                ));

                // Sync native balance.
                instructions.push(spl_token::instruction::sync_native(
                    &context.token_program_id,
                    &context.source_ata,
                )?);

                let swap_args = SwapInstructionArgs {
                    in_amount: args.amount_in,
                    minimum_out_amount: args.min_amount_out,
                };

                let keys: Vec<AccountMeta> = vec![
                    AccountMeta::new(Pubkey::from_str_const(&self.pool_address), false),
                    AccountMeta::new(context.source_ata, false),
                    AccountMeta::new(context.destination_ata, false),
                    AccountMeta::new(self.pool.a_vault, false),
                    AccountMeta::new(self.pool.b_vault, false),
                    AccountMeta::new(a_token_vault, false),
                    AccountMeta::new(b_token_vault, false),
                    AccountMeta::new(a_vault_lp_mint, false),
                    AccountMeta::new(b_vault_lp_mint, false),
                    AccountMeta::new(self.pool.a_vault_lp, false),
                    AccountMeta::new(self.pool.b_vault_lp, false),
                    AccountMeta::new(self.pool.protocol_token_b_fee, false),
                    AccountMeta::new(context.user, true),
                    AccountMeta::new(
                        Pubkey::from_str_const(METEORA_VAULT_PROGRAM),
                        false,
                    ),
                    AccountMeta::new_readonly(
                        Pubkey::from_str_const(thunder_core::TOKEN_PROGRAM),
                        false,
                    ),
                ];

                let mut data = vec![248, 198, 158, 145, 225, 117, 135, 200];
                data.append(&mut to_vec(&swap_args)?);

                instructions.push(Instruction {
                    program_id: Pubkey::from_str_const(METEORA_DYNAMIC_AMM),
                    accounts: keys,
                    data,
                });

                // Close WSOL ATA to unwrap remaining SOL.
                instructions.push(spl_token::instruction::close_account(
                    &context.token_program_id,
                    &context.source_ata,
                    &context.user,
                    &context.user,
                    &[],
                )?);
            }

            SwapDirection::Sell => {
                // Sell: A (Token) -> B (SOL)
                if !context.source_ata_exists {
                    instructions.push(create_associated_token_account_idempotent(
                        &context.user,
                        &context.user,
                        &self.pool.token_a_mint,
                        &context.token_program_id,
                    ));
                }

                if !context.destination_ata_exists {
                    instructions.push(create_associated_token_account_idempotent(
                        &context.user,
                        &context.user,
                        &self.pool.token_b_mint,
                        &context.token_program_id,
                    ));
                }

                let swap_args = SwapInstructionArgs {
                    in_amount: args.amount_in,
                    minimum_out_amount: args.min_amount_out,
                };

                let keys: Vec<AccountMeta> = vec![
                    AccountMeta::new(Pubkey::from_str_const(&self.pool_address), false),
                    AccountMeta::new(context.source_ata, false),
                    AccountMeta::new(context.destination_ata, false),
                    AccountMeta::new(self.pool.a_vault, false),
                    AccountMeta::new(self.pool.b_vault, false),
                    AccountMeta::new(a_token_vault, false),
                    AccountMeta::new(b_token_vault, false),
                    AccountMeta::new(a_vault_lp_mint, false),
                    AccountMeta::new(b_vault_lp_mint, false),
                    AccountMeta::new(self.pool.a_vault_lp, false),
                    AccountMeta::new(self.pool.b_vault_lp, false),
                    AccountMeta::new(self.pool.protocol_token_a_fee, false),
                    AccountMeta::new(context.user, true),
                    AccountMeta::new(
                        Pubkey::from_str_const(METEORA_VAULT_PROGRAM),
                        false,
                    ),
                    AccountMeta::new_readonly(
                        Pubkey::from_str_const(thunder_core::TOKEN_PROGRAM),
                        false,
                    ),
                ];

                let mut data = vec![248, 198, 158, 145, 225, 117, 135, 200];
                data.append(&mut to_vec(&swap_args)?);

                instructions.push(Instruction {
                    program_id: Pubkey::from_str_const(METEORA_DYNAMIC_AMM),
                    accounts: keys,
                    data,
                });

                // Close WSOL ATA to unwrap received SOL.
                instructions.push(spl_token::instruction::close_account(
                    &context.token_program_id,
                    &context.destination_ata,
                    &context.user,
                    &context.user,
                    &[],
                )?);
            }
        }

        Ok(instructions)
    }

    fn required_accounts(
        &self,
        _user: Pubkey,
        direction: SwapDirection,
    ) -> Result<RequiredAccounts, GenericError> {
        let (source_mint, destination_mint) = match direction {
            SwapDirection::Buy => {
                if self.flipped {
                    (self.pool.token_a_mint, self.pool.token_b_mint)
                } else {
                    (self.pool.token_b_mint, self.pool.token_a_mint)
                }
            }
            SwapDirection::Sell => {
                if self.flipped {
                    (self.pool.token_b_mint, self.pool.token_a_mint)
                } else {
                    (self.pool.token_a_mint, self.pool.token_b_mint)
                }
            }
        };

        Ok(RequiredAccounts {
            source_mint,
            destination_mint,
            account_data: vec![self.pool.a_vault, self.pool.b_vault],
        })
    }
}

// =============================================================================
// V2: MeteoraDAMMV2Market
// =============================================================================

/// Market wrapper for Meteora Dynamic AMM V2 pools.
pub struct MeteoraDAMMV2Market {
    pub pool: MeteoraDAMMV2Pool,
    pub pool_address: String,
    pub a_vault_balance: u64,
    pub b_vault_balance: u64,
    /// Decimals for token_a and token_b. Needed for sqrt_price conversion.
    /// Defaults to (6, 9) if not provided (assumes token_a=token, token_b=SOL).
    pub token_a_decimals: u8,
    pub token_b_decimals: u8,
    /// True when the pool's token_a is actually the quote currency (WSOL/USDC).
    pub flipped: bool,
}

impl MeteoraDAMMV2Market {
    pub fn new(pool: MeteoraDAMMV2Pool, pool_address: String) -> Self {
        let flipped = quote_priority(&pool.token_a_mint).unwrap_or(usize::MAX) < quote_priority(&pool.token_b_mint).unwrap_or(usize::MAX);
        let da = infer_mint_decimals(&pool.token_a_mint);
        let db = infer_mint_decimals(&pool.token_b_mint);
        Self {
            pool,
            pool_address,
            a_vault_balance: 0,
            b_vault_balance: 0,
            token_a_decimals: da,
            token_b_decimals: db,
            flipped,
        }
    }


    /// Convert V2 Q64.128 sqrt_price to a regular price (token_a per token_b).
    ///
    /// Formula: price = (sqrt_price >> 64)^2 * 10^(decimals_a - decimals_b)
    /// The sqrt_price is Q64.128: upper 64 bits are integer, lower 64 bits are fraction.
    fn sqrt_price_to_price(&self, decimals_a: u8, decimals_b: u8) -> f64 {
        // Extract the rational value: sqrt_price / 2^64
        let sqrt_price_f64 = self.pool.sqrt_price as f64 / (1u128 << 64) as f64;
        let raw = sqrt_price_f64 * sqrt_price_f64;
        // Apply decimal adjustment
        let decimal_adj = 10f64.powi(decimals_a as i32 - decimals_b as i32);
        raw * decimal_adj
    }

    /// Base fee in basis points.
    /// cliff_fee_numerator is in parts-per-billion (1e9).
    fn calculate_base_fee_bps(&self) -> u64 {
        // cliff_fee_numerator / 1e9 gives the fraction, * 10000 gives bps
        self.pool.pool_fees.base_fee.cliff_fee_numerator / 100_000
    }

    /// Simplified V2 output calculation using current sqrt_price.
    fn calculate_v2_output(
        &self,
        amount_in: u64,
        direction: SwapDirection,
    ) -> Result<u64, GenericError> {
        let price = self.sqrt_price_to_price(self.token_a_decimals, self.token_b_decimals);
        let fee_bps = self.calculate_base_fee_bps();

        let fee_multiplier = 10000 - fee_bps;
        let amount_in_with_fee = (amount_in as u128 * fee_multiplier as u128) / 10000;

        // Normalize direction when flipped.
        let physical_direction = if self.flipped {
            match direction {
                SwapDirection::Buy => SwapDirection::Sell,
                SwapDirection::Sell => SwapDirection::Buy,
            }
        } else {
            direction
        };

        let output = match physical_direction {
            SwapDirection::Buy => (amount_in_with_fee as f64 / price) as u64,
            SwapDirection::Sell => (amount_in_with_fee as f64 * price) as u64,
        };

        Ok(output)
    }
}

impl Market for MeteoraDAMMV2Market {
    fn is_active(&self) -> bool {
        self.pool.pool_status == 1
    }

    fn metadata(&self) -> Result<PoolMetadata, GenericError> {
        let trade_fee_bps = self.calculate_base_fee_bps();
        let protocol_fee_bps =
            (self.pool.pool_fees.protocol_fee_percent as u64 * trade_fee_bps) / 100;

        Ok(PoolMetadata {
            address: self.pool_address.clone(),
            dex_name: "Meteora DAMM V2".to_string(),
            quote_mint: if self.flipped { self.pool.token_a_mint } else { self.pool.token_b_mint },
            base_mint: if self.flipped { self.pool.token_b_mint } else { self.pool.token_a_mint },
            quote_vault: if self.flipped { self.pool.token_a_vault } else { self.pool.token_b_vault },
            base_vault: if self.flipped { self.pool.token_b_vault } else { self.pool.token_a_vault },
            fees: PoolFees {
                trade_fee_bps,
                protocol_fee_bps: Some(protocol_fee_bps),
            },
        })
    }

    fn financials(&self) -> Result<PoolFinancials, GenericError> {
        Ok(PoolFinancials {
            quote_balance: if self.flipped { self.a_vault_balance } else { self.b_vault_balance },
            base_balance: if self.flipped { self.b_vault_balance } else { self.a_vault_balance },
            quote_decimals: if self.flipped { self.token_a_decimals } else { self.token_b_decimals },
            base_decimals: if self.flipped { self.token_b_decimals } else { self.token_a_decimals },
        })
    }

    fn calculate_output(
        &self,
        amount_in: u64,
        direction: SwapDirection,
    ) -> Result<u64, GenericError> {
        self.calculate_v2_output(amount_in, direction)
    }

    fn calculate_price_impact(
        &self,
        amount_in: u64,
        direction: SwapDirection,
    ) -> Result<u64, GenericError> {
        let pre_swap_price = self.current_price()?;
        let output = self.calculate_output(amount_in, direction)?;

        let physical_direction = if self.flipped {
            match direction {
                SwapDirection::Buy => SwapDirection::Sell,
                SwapDirection::Sell => SwapDirection::Buy,
            }
        } else {
            direction
        };

        let post_swap_price = match physical_direction {
            SwapDirection::Buy => {
                let new_b = self.b_vault_balance + amount_in;
                let new_a = self.a_vault_balance.saturating_sub(output);
                if new_a == 0 {
                    return Err("Insufficient liquidity in pool".into());
                }
                new_b as f64 / new_a as f64
            }
            SwapDirection::Sell => {
                let new_a = self.a_vault_balance + amount_in;
                let new_b = self.b_vault_balance.saturating_sub(output);
                if new_a == 0 {
                    return Err("Insufficient liquidity in pool".into());
                }
                new_b as f64 / new_a as f64
            }
        };

        Ok(calculate_price_impact_bps(pre_swap_price, post_swap_price))
    }

    fn current_price(&self) -> Result<f64, GenericError> {
        // Prefer on-chain token amounts when available (layout_version >= 1).
        // These are the actual reserves, no sqrt_price math needed.
        if self.pool.token_a_amount > 0 && self.pool.token_b_amount > 0 {
            let a_human = self.pool.token_a_amount as f64 / 10f64.powi(self.token_a_decimals as i32);
            let b_human = self.pool.token_b_amount as f64 / 10f64.powi(self.token_b_decimals as i32);
            // a_human/b_human = token_a per token_b (human-readable).
            // NOT flipped (quote=b, base=a): want b_per_a = b_human / a_human.
            // Flipped (quote=a, base=b): want a_per_b = a_human / b_human.
            return if self.flipped {
                Ok(a_human / b_human)
            } else {
                Ok(b_human / a_human)
            };
        }

        // Fallback to sqrt_price for older pools.
        let raw = self.sqrt_price_to_price(self.token_a_decimals, self.token_b_decimals);
        if self.flipped {
            Ok(raw)
        } else {
            if raw == 0.0 {
                return Err("Pool has zero price".into());
            }
            Ok(1.0 / raw)
        }
    }

    fn build_swap_instruction(
        &self,
        context: SwapContext,
        args: SwapArgs,
        direction: SwapDirection,
    ) -> Result<Vec<Instruction>, GenericError> {
        use borsh::to_vec;
        use spl_associated_token_account::instruction::create_associated_token_account_idempotent;

        let mut instructions = Vec::new();

        if self.pool.pool_status != 1 {
            return Err(
                format!("Pool is not active (status: {})", self.pool.pool_status).into(),
            );
        }

        // Normalize direction when flipped.
        let physical_direction = if self.flipped {
            match direction {
                SwapDirection::Buy => SwapDirection::Sell,
                SwapDirection::Sell => SwapDirection::Buy,
            }
        } else {
            direction
        };

        match physical_direction {
            SwapDirection::Buy => {
                if !context.destination_ata_exists {
                    instructions.push(create_associated_token_account_idempotent(
                        &context.user,
                        &context.user,
                        &self.pool.token_a_mint,
                        &context.token_program_id,
                    ));
                }

                let swap_args = SwapInstructionArgs {
                    in_amount: args.amount_in,
                    minimum_out_amount: args.min_amount_out,
                };

                let keys: Vec<AccountMeta> = vec![
                    AccountMeta::new(Pubkey::from_str_const(&self.pool_address), false),
                    AccountMeta::new(context.source_ata, false),
                    AccountMeta::new(context.destination_ata, false),
                    AccountMeta::new(self.pool.token_a_vault, false),
                    AccountMeta::new(self.pool.token_b_vault, false),
                    AccountMeta::new_readonly(
                        Pubkey::from_str_const(thunder_core::TOKEN_PROGRAM),
                        false,
                    ),
                    AccountMeta::new(context.user, true),
                ];

                let mut data = vec![248, 198, 158, 145, 225, 117, 135, 200];
                data.append(&mut to_vec(&swap_args)?);

                instructions.push(Instruction {
                    program_id: Pubkey::from_str_const(METEORA_DYNAMIC_AMM_V2),
                    accounts: keys,
                    data,
                });
            }

            SwapDirection::Sell => {
                if !context.destination_ata_exists {
                    instructions.push(create_associated_token_account_idempotent(
                        &context.user,
                        &context.user,
                        &self.pool.token_b_mint,
                        &spl_token::ID,
                    ));
                }

                let swap_args = SwapInstructionArgs {
                    in_amount: args.amount_in,
                    minimum_out_amount: args.min_amount_out,
                };

                let keys: Vec<AccountMeta> = vec![
                    AccountMeta::new(Pubkey::from_str_const(&self.pool_address), false),
                    AccountMeta::new(context.source_ata, false),
                    AccountMeta::new(context.destination_ata, false),
                    AccountMeta::new(self.pool.token_a_vault, false),
                    AccountMeta::new(self.pool.token_b_vault, false),
                    AccountMeta::new_readonly(
                        Pubkey::from_str_const(thunder_core::TOKEN_PROGRAM),
                        false,
                    ),
                    AccountMeta::new(context.user, true),
                ];

                let mut data = vec![248, 198, 158, 145, 225, 117, 135, 200];
                data.append(&mut to_vec(&swap_args)?);

                instructions.push(Instruction {
                    program_id: Pubkey::from_str_const(METEORA_DYNAMIC_AMM_V2),
                    accounts: keys,
                    data,
                });
            }
        }

        Ok(instructions)
    }

    fn required_accounts(
        &self,
        _user: Pubkey,
        direction: SwapDirection,
    ) -> Result<RequiredAccounts, GenericError> {
        let (source_mint, destination_mint) = match direction {
            SwapDirection::Buy => {
                if self.flipped {
                    (self.pool.token_a_mint, self.pool.token_b_mint)
                } else {
                    (self.pool.token_b_mint, self.pool.token_a_mint)
                }
            }
            SwapDirection::Sell => {
                if self.flipped {
                    (self.pool.token_b_mint, self.pool.token_a_mint)
                } else {
                    (self.pool.token_a_mint, self.pool.token_b_mint)
                }
            }
        };

        Ok(RequiredAccounts {
            source_mint,
            destination_mint,
            account_data: vec![],
        })
    }
}

// =============================================================================
// Helpers
// =============================================================================

/// Try to extract the LP mint from a vault authority account stored in SwapContext.extra_accounts.
fn extract_lp_mint_from_context(context: &SwapContext, vault: &Pubkey) -> Option<Pubkey> {
    let account_data = context.extra_accounts.get(&vault.to_string())?;
    if account_data.len() <= 8 {
        return None;
    }
    let vault_auth = models::VaultAuthority::deserialize(&mut &account_data[8..]).ok()?;
    Some(vault_auth.lp_mint)
}
