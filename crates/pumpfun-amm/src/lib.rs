pub mod pda;

use borsh::BorshDeserialize;
use solana_pubkey::Pubkey;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_system_interface::instruction as system_instruction;
use spl_associated_token_account::instruction::create_associated_token_account_idempotent;

use thunder_core::{
    BondingCurve, GenericError, Market, PoolFees, PoolFinancials, PoolMetadata, RequiredAccounts,
    SwapArgs, SwapContext, SwapDirection, TOKEN_PROGRAM,
    calculate_price_impact_bps, infer_mint_decimals,
};

use crate::pda::{
    get_global_volume_accumulator_pda, get_pool_v2_pda, get_pumpfun_config_pda,
    get_pumpfun_creator_vault_ata, get_pumpfun_creator_vault_authority_pda,
    get_user_volume_accumulator_pda,
};

// ---------------------------------------------------------------------------
// DEX-specific constants
// ---------------------------------------------------------------------------

pub const PUMPFUN_AMM_PROGRAM: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";
pub const PUMPFUN_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
pub const PUMPFUN_FEE_PROGRAM: &str = "pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ";

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

#[derive(BorshDeserialize, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PumpfunBondingCurve {
    pub virtual_token_reserves: u64,
    pub virtual_sol_reserves: u64,
    pub real_token_reserves: u64,
    pub real_sol_reserves: u64,
    pub token_total_supply: u64,
    pub complete: bool,
    pub creator: Pubkey,
}

#[derive(BorshDeserialize, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PumpfunAmmPool {
    pub pool_bump: u8,
    pub index: u16,
    pub creator: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub lp_mint: Pubkey,
    pub pool_base_token_account: Pubkey,
    pub pool_quote_token_account: Pubkey,
    pub lp_supply: u64,
    pub coin_creator: Pubkey,
    pub is_mayhem_mode: bool,
    pub is_cashback_coin: bool,
    /// Bonding curve data — fetched separately, not part of pool account borsh layout
    #[borsh(skip)]
    #[serde(default)]
    pub bonding_curve: Option<PumpfunBondingCurve>,
}

// ---------------------------------------------------------------------------
// Instruction args
// ---------------------------------------------------------------------------

/// Args for buy instruction (Pumpfun AMM).
/// Specify exact tokens out, max SOL in.
#[derive(Clone, Debug, borsh::BorshSerialize)]
pub struct BuyArgs {
    pub base_amount_out: u64,
    pub max_quote_amount_in: u64,
    pub track_volume: bool,
}

#[derive(Clone, Debug, borsh::BorshSerialize)]
pub struct SellSwapInstructionArgs {
    pub base_amount_in: u64,
    pub min_quote_amount_out: u64,
}

// ---------------------------------------------------------------------------
// Market wrapper
// ---------------------------------------------------------------------------

pub struct PumpfunAmmMarket {
    pub pool: PumpfunAmmPool,
    pub pool_address: String,
    pub base_decimals: u8,
}

impl PumpfunAmmMarket {
    pub fn new(pool: PumpfunAmmPool, pool_address: String) -> Self {
        let base_decimals = infer_mint_decimals(&pool.base_mint);
        Self { pool, pool_address, base_decimals }
    }

    /// Calculate bonding curve output using virtual reserves and constant-product formula.
    fn calculate_bonding_curve_output(
        &self,
        amount_in: u64,
        direction: SwapDirection,
    ) -> Result<u64, GenericError> {
        let bonding_curve = self
            .pool
            .bonding_curve
            .as_ref()
            .ok_or("Bonding curve data not available")?;

        // Pumpfun uses 1% fee (100 bps)
        let fee_bps = 100u64;
        let fee_multiplier = 10000 - fee_bps;
        let amount_in_with_fee = (amount_in as u128 * fee_multiplier as u128) / 10000;

        let virtual_sol = bonding_curve.virtual_sol_reserves as u128;
        let virtual_token = bonding_curve.virtual_token_reserves as u128;

        if virtual_sol == 0 || virtual_token == 0 {
            return Err("Bonding curve has zero virtual reserves".into());
        }

        let output = match direction {
            SwapDirection::Buy => {
                // SOL -> Token: constant product k = virtual_sol * virtual_token
                let k = virtual_sol * virtual_token;
                let new_sol = virtual_sol + amount_in_with_fee;
                let new_token = k / new_sol;
                virtual_token.saturating_sub(new_token) as u64
            }
            SwapDirection::Sell => {
                // Token -> SOL: constant product
                let k = virtual_sol * virtual_token;
                let new_token = virtual_token + amount_in_with_fee;
                let new_sol = k / new_token;
                virtual_sol.saturating_sub(new_sol) as u64
            }
        };

        Ok(output)
    }
}

// ---------------------------------------------------------------------------
// Market trait
// ---------------------------------------------------------------------------

impl Market for PumpfunAmmMarket {
    fn metadata(&self) -> Result<PoolMetadata, GenericError> {
        Ok(PoolMetadata {
            address: self.pool_address.clone(),
            dex_name: "Pumpfun AMM".to_string(),
            quote_mint: self.pool.quote_mint,
            base_mint: self.pool.base_mint,
            quote_vault: self.pool.pool_quote_token_account,
            base_vault: self.pool.pool_base_token_account,
            fees: PoolFees {
                trade_fee_bps: 100,
                protocol_fee_bps: None,
            },
        })
    }

    fn financials(&self) -> Result<PoolFinancials, GenericError> {
        let bonding_curve = self
            .pool
            .bonding_curve
            .as_ref()
            .ok_or("Bonding curve data not available")?;

        Ok(PoolFinancials {
            quote_balance: bonding_curve.real_sol_reserves,
            base_balance: bonding_curve.real_token_reserves,
            quote_decimals: 9,
            base_decimals: self.base_decimals,
        })
    }

    fn calculate_output(
        &self,
        amount_in: u64,
        direction: SwapDirection,
    ) -> Result<u64, GenericError> {
        self.calculate_bonding_curve_output(amount_in, direction)
    }

    fn calculate_price_impact(
        &self,
        amount_in: u64,
        direction: SwapDirection,
    ) -> Result<u64, GenericError> {
        let pre_swap_price = self.current_price()?;
        let output = self.calculate_output(amount_in, direction)?;

        let bonding_curve = self
            .pool
            .bonding_curve
            .as_ref()
            .ok_or("Bonding curve data not available")?;

        let post_swap_price = match direction {
            SwapDirection::Buy => {
                let new_sol = bonding_curve.virtual_sol_reserves + amount_in;
                let new_token = bonding_curve.virtual_token_reserves.saturating_sub(output);
                if new_token == 0 {
                    return Err("Insufficient liquidity in bonding curve".into());
                }
                new_sol as f64 / new_token as f64
            }
            SwapDirection::Sell => {
                let new_token = bonding_curve.virtual_token_reserves + amount_in;
                let new_sol = bonding_curve.virtual_sol_reserves.saturating_sub(output);
                if new_token == 0 {
                    return Err("Insufficient liquidity in bonding curve".into());
                }
                new_sol as f64 / new_token as f64
            }
        };

        Ok(calculate_price_impact_bps(pre_swap_price, post_swap_price))
    }

    fn current_price(&self) -> Result<f64, GenericError> {
        let bonding_curve = self
            .pool
            .bonding_curve
            .as_ref()
            .ok_or("Bonding curve data not available")?;

        if bonding_curve.virtual_token_reserves == 0 {
            return Err("Bonding curve has zero virtual token reserves".into());
        }

        // Raw price in lamports: SOL_lamports / token_raw_units
        let raw = bonding_curve.virtual_sol_reserves as f64 / bonding_curve.virtual_token_reserves as f64;
        // Adjust: (sol / 10^9) / (tokens / 10^base_dec) = raw * 10^base_dec / 10^9
        let decimal_adj = 10f64.powi(self.base_decimals as i32 - 9);
        Ok(raw * decimal_adj)
    }

    fn build_swap_instruction(
        &self,
        context: SwapContext,
        args: SwapArgs,
        direction: SwapDirection,
    ) -> Result<Vec<Instruction>, GenericError> {
        use borsh::to_vec;
        use solana_sdk::pubkey;

        let mut instructions = Vec::new();

        match direction {
            SwapDirection::Buy => {
                // Buy: SOL -> Token
                if !context.source_ata_exists {
                    instructions.push(create_associated_token_account_idempotent(
                        &context.user,
                        &context.user,
                        &self.pool.quote_mint,
                        &spl_token::ID,
                    ));
                }

                if !context.destination_ata_exists {
                    instructions.push(create_associated_token_account_idempotent(
                        &context.user,
                        &context.user,
                        &self.pool.base_mint,
                        &context.token_program_id,
                    ));
                }

                // Wrap SOL: transfer native SOL to WSOL ATA
                instructions.push(system_instruction::transfer(
                    &context.user,
                    &context.source_ata,
                    args.amount_in,
                ));

                // Sync the WSOL account balance
                instructions.push(spl_token::instruction::sync_native(
                    &spl_token::ID,
                    &context.source_ata,
                )?);

                let swap_args = BuyArgs {
                    base_amount_out: args.min_amount_out,
                    max_quote_amount_in: args.amount_in,
                    track_volume: false,
                };

                let protocol_fee_recipient =
                    pubkey!("62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV");
                let protocol_fee_recipient_token_account =
                    pubkey!("94qWNrtmfn42h3ZjUZwWvK1MEo9uVmmrBPd2hpNjYDjb");

                let coin_creator_vault_authority =
                    get_pumpfun_creator_vault_authority_pda(self.pool.coin_creator);
                let coin_creator_vault_ata = get_pumpfun_creator_vault_ata(
                    coin_creator_vault_authority,
                    self.pool.quote_mint,
                );

                let global_volume_acc_pda = get_global_volume_accumulator_pda();
                let user_volume_acc_pda = get_user_volume_accumulator_pda(context.user);
                let pool_v2_pda = get_pool_v2_pda(self.pool.base_mint);

                let keys: Vec<AccountMeta> = vec![
                    AccountMeta::new(Pubkey::from_str_const(&self.pool_address), false),
                    AccountMeta::new(context.user, true),
                    AccountMeta::new_readonly(
                        Pubkey::from_str_const("ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw"),
                        false,
                    ),
                    AccountMeta::new_readonly(self.pool.base_mint, false),
                    AccountMeta::new_readonly(self.pool.quote_mint, false),
                    AccountMeta::new(context.destination_ata, false),
                    AccountMeta::new(context.source_ata, false),
                    AccountMeta::new(self.pool.pool_base_token_account, false),
                    AccountMeta::new(self.pool.pool_quote_token_account, false),
                    AccountMeta::new_readonly(protocol_fee_recipient, false),
                    AccountMeta::new(protocol_fee_recipient_token_account, false),
                    AccountMeta::new_readonly(Pubkey::from_str_const(TOKEN_PROGRAM), false),
                    AccountMeta::new_readonly(Pubkey::from_str_const(TOKEN_PROGRAM), false),
                    AccountMeta::new_readonly(
                        Pubkey::from_str_const("11111111111111111111111111111111"),
                        false,
                    ),
                    AccountMeta::new_readonly(
                        Pubkey::from_str_const("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"),
                        false,
                    ),
                    AccountMeta::new_readonly(
                        Pubkey::from_str_const("GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR"),
                        false,
                    ),
                    AccountMeta::new_readonly(
                        Pubkey::from_str_const(PUMPFUN_AMM_PROGRAM),
                        false,
                    ),
                    AccountMeta::new(coin_creator_vault_ata, false),
                    AccountMeta::new_readonly(coin_creator_vault_authority, false),
                    AccountMeta::new_readonly(global_volume_acc_pda, false),
                    AccountMeta::new(user_volume_acc_pda, false),
                    AccountMeta::new_readonly(get_pumpfun_config_pda(), false),
                    AccountMeta::new_readonly(
                        Pubkey::from_str_const(PUMPFUN_FEE_PROGRAM),
                        false,
                    ),
                    AccountMeta::new_readonly(pool_v2_pda, false),
                ];

                // Discriminator for buy: [102, 6, 61, 18, 1, 218, 235, 234]
                let mut data = vec![102, 6, 61, 18, 1, 218, 235, 234];
                data.append(&mut to_vec(&swap_args)?);

                instructions.push(Instruction {
                    program_id: Pubkey::from_str_const(PUMPFUN_AMM_PROGRAM),
                    accounts: keys,
                    data,
                });

                // Close the WSOL account to recover remaining SOL
                instructions.push(spl_token::instruction::close_account(
                    &spl_token::ID,
                    &context.source_ata,
                    &context.user,
                    &context.user,
                    &[],
                )?);
            }

            SwapDirection::Sell => {
                // Sell: Token -> SOL
                if !context.source_ata_exists {
                    instructions.push(create_associated_token_account_idempotent(
                        &context.user,
                        &context.user,
                        &self.pool.base_mint,
                        &context.token_program_id,
                    ));
                }

                if !context.destination_ata_exists {
                    instructions.push(create_associated_token_account_idempotent(
                        &context.user,
                        &context.user,
                        &self.pool.quote_mint,
                        &spl_token::ID,
                    ));
                }

                let swap_args = SellSwapInstructionArgs {
                    base_amount_in: args.amount_in,
                    min_quote_amount_out: args.min_amount_out,
                };

                let protocol_fee_recipient =
                    pubkey!("62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV");
                let protocol_fee_recipient_token_account =
                    pubkey!("94qWNrtmfn42h3ZjUZwWvK1MEo9uVmmrBPd2hpNjYDjb");

                let coin_creator_vault_authority =
                    get_pumpfun_creator_vault_authority_pda(self.pool.coin_creator);
                let coin_creator_vault_ata = get_pumpfun_creator_vault_ata(
                    coin_creator_vault_authority,
                    self.pool.quote_mint,
                );
                let pool_v2_pda = get_pool_v2_pda(self.pool.base_mint);

                let keys: Vec<AccountMeta> = vec![
                    AccountMeta::new(Pubkey::from_str_const(&self.pool_address), false),
                    AccountMeta::new(context.user, true),
                    AccountMeta::new_readonly(
                        Pubkey::from_str_const("ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw"),
                        false,
                    ),
                    AccountMeta::new_readonly(self.pool.base_mint, false),
                    AccountMeta::new_readonly(self.pool.quote_mint, false),
                    AccountMeta::new(context.source_ata, false),
                    AccountMeta::new(context.destination_ata, false),
                    AccountMeta::new(self.pool.pool_base_token_account, false),
                    AccountMeta::new(self.pool.pool_quote_token_account, false),
                    AccountMeta::new_readonly(protocol_fee_recipient, false),
                    AccountMeta::new(protocol_fee_recipient_token_account, false),
                    AccountMeta::new_readonly(Pubkey::from_str_const(TOKEN_PROGRAM), false),
                    AccountMeta::new_readonly(Pubkey::from_str_const(TOKEN_PROGRAM), false),
                    AccountMeta::new_readonly(
                        Pubkey::from_str_const("11111111111111111111111111111111"),
                        false,
                    ),
                    AccountMeta::new_readonly(
                        Pubkey::from_str_const("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"),
                        false,
                    ),
                    AccountMeta::new_readonly(
                        Pubkey::from_str_const("GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR"),
                        false,
                    ),
                    AccountMeta::new_readonly(
                        Pubkey::from_str_const(PUMPFUN_AMM_PROGRAM),
                        false,
                    ),
                    AccountMeta::new(coin_creator_vault_ata, false),
                    AccountMeta::new_readonly(coin_creator_vault_authority, false),
                    AccountMeta::new_readonly(get_pumpfun_config_pda(), false),
                    AccountMeta::new_readonly(
                        Pubkey::from_str_const(PUMPFUN_FEE_PROGRAM),
                        false,
                    ),
                    AccountMeta::new_readonly(pool_v2_pda, false),
                ];

                // Discriminator for sell: [51, 230, 133, 164, 1, 127, 131, 173]
                let mut data = vec![51, 230, 133, 164, 1, 127, 131, 173];
                data.append(&mut to_vec(&swap_args)?);

                instructions.push(Instruction {
                    program_id: Pubkey::from_str_const(PUMPFUN_AMM_PROGRAM),
                    accounts: keys,
                    data,
                });

                // Close the WSOL account to unwrap SOL
                instructions.push(spl_token::instruction::close_account(
                    &spl_token::ID,
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
            SwapDirection::Buy => (self.pool.quote_mint, self.pool.base_mint),
            SwapDirection::Sell => (self.pool.base_mint, self.pool.quote_mint),
        };

        Ok(RequiredAccounts {
            source_mint,
            destination_mint,
            account_data: vec![],
        })
    }
}

// ---------------------------------------------------------------------------
// BondingCurve trait
// ---------------------------------------------------------------------------

impl BondingCurve for PumpfunAmmMarket {
    fn price_at_supply(&self, supply: u64) -> Result<f64, GenericError> {
        let bonding_curve = self
            .pool
            .bonding_curve
            .as_ref()
            .ok_or("Bonding curve data not available")?;

        let remaining_tokens = bonding_curve.token_total_supply.saturating_sub(supply);
        if remaining_tokens == 0 {
            return Err("Supply exceeds total token supply".into());
        }

        Ok(bonding_curve.virtual_sol_reserves as f64 / remaining_tokens as f64)
    }

    fn is_graduated(&self) -> Result<bool, GenericError> {
        let bonding_curve = self
            .pool
            .bonding_curve
            .as_ref()
            .ok_or("Bonding curve data not available")?;

        Ok(bonding_curve.complete)
    }
}
