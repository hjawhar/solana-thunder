//! Raydium CLMM (Concentrated Liquidity Market Maker) DEX implementation.
//!
//! Tick-based pricing with concentrated liquidity positions.

pub mod tick_arrays;


use borsh::BorshDeserialize;
use solana_pubkey::Pubkey;
use solana_sdk::instruction::Instruction;

use thunder_core::{
    ConcentratedLiquidity, GenericError, Market, PoolFees, PoolFinancials, PoolMetadata,
    RequiredAccounts, SwapArgs, SwapContext, SwapDirection, calculate_price_impact_bps,
    MEMO_PROGRAM_V2, TOKEN_PROGRAM, TOKEN_PROGRAM_2022,
};

use crate::tick_arrays::{compute_clmm_remaining_accounts, pda_array_bitmap_address};

// ============================================================================
// Constants
// ============================================================================

pub const RAYDIUM_CLMM: &str = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";

// ============================================================================
// Models
// ============================================================================

#[derive(
    Debug, BorshDeserialize, serde::Deserialize, serde::Serialize, PartialEq, Eq, Clone, Hash,
)]
pub struct RewardInfo {
    /// Reward state
    pub reward_state: u8,
    /// Reward open time
    pub open_time: u64,
    /// Reward end time
    pub end_time: u64,
    /// Reward last update time
    pub last_update_time: u64,
    /// Q64.64 number indicates how many tokens per second are earned per unit of liquidity.
    pub emissions_per_second_x64: u128,
    /// The total amount of reward emissioned
    pub reward_total_emissioned: u64,
    /// The total amount of claimed reward
    pub reward_claimed: u64,
    /// Reward token mint.
    pub token_mint: Pubkey,
    /// Reward vault token account.
    pub token_vault: Pubkey,
    /// The owner that has permission to set reward param
    pub authority: Pubkey,
    /// Q64.64 number that tracks the total tokens earned per unit of liquidity since the reward
    /// emissions were turned on.
    pub reward_growth_global_x64: u128,
}

#[derive(
    Debug, BorshDeserialize, serde::Deserialize, serde::Serialize, PartialEq, Eq, Clone, Hash,
)]
pub struct RaydiumCLMMPool {
    pub bump: [u8; 1],
    pub amm_config: Pubkey,
    pub owner: Pubkey,
    pub token_mint_0: Pubkey,
    pub token_mint_1: Pubkey,
    pub token_vault_0: Pubkey,
    pub token_vault_1: Pubkey,
    pub observation_key: Pubkey,
    pub mint_decimals_0: u8,
    pub mint_decimals_1: u8,
    pub tick_spacing: u16,
    pub liquidity: u128,
    pub sqrt_price_x64: u128,
    pub tick_current: i32,
    pub padding3: u16,
    pub padding4: u16,
    pub fee_growth_global_0_x64: u128,
    pub fee_growth_global_1_x64: u128,
    pub protocol_fees_token_0: u64,
    pub protocol_fees_token_1: u64,
    pub swap_in_amount_token_0: u128,
    pub swap_out_amount_token_1: u128,
    pub swap_in_amount_token_1: u128,
    pub swap_out_amount_token_0: u128,
    pub status: u8,
    pub padding: [u8; 7],
    pub reward_infos: [RewardInfo; 3],
    pub tick_array_bitmap: [u64; 16],
    pub total_fees_token_0: u64,
    pub total_fees_claimed_token_0: u64,
    pub total_fees_token_1: u64,
    pub total_fees_claimed_token_1: u64,
    pub fund_fees_token_0: u64,
    pub fund_fees_token_1: u64,
    pub open_time: u64,
    pub recent_epoch: u64,
    pub padding1: [u64; 24],
    pub padding2: [u64; 32],
}

// ============================================================================
// Swap Instruction Args (BorshSerialize for on-chain)
// ============================================================================

#[derive(Clone, Debug, borsh::BorshSerialize)]
struct SwapInstructionArgs {
    pub amount: u64,
    pub other_amount_threshold: u64,
    pub sqrt_price_limit_x64: u128,
    pub is_base_input: bool,
}

// ============================================================================
// Market Struct
// ============================================================================

/// Wrapper for RaydiumCLMMPool that implements the Market trait.
pub struct RaydiumClmmMarket {
    pub pool: RaydiumCLMMPool,
    pub pool_address: String,
    /// Current vault balances (cached from last fetch)
    pub vault_0_balance: u64,
    pub vault_1_balance: u64,
}

impl RaydiumClmmMarket {
    pub fn new(pool: RaydiumCLMMPool, pool_address: String) -> Self {
        Self {
            pool,
            pool_address,
            vault_0_balance: 0,
            vault_1_balance: 0,
        }
    }

    /// Convert sqrt_price_x64 to regular price.
    ///
    /// CLMM uses Q64.64 fixed-point format for sqrt(price):
    /// price = (sqrt_price_x64 / 2^64)^2
    fn sqrt_price_to_price(&self) -> f64 {
        let sqrt_price_f64 = self.pool.sqrt_price_x64 as f64 / (1u128 << 64) as f64;
        sqrt_price_f64 * sqrt_price_f64
    }

    /// Calculate output for CLMM swap.
    ///
    /// Note: This is a simplified calculation. Real CLMM swaps traverse multiple ticks.
    /// For production, use the actual CLMM math library or fetch from RPC simulation.
    fn calculate_clmm_output(
        &self,
        amount_in: u64,
        direction: SwapDirection,
    ) -> Result<u64, GenericError> {
        let price = self.sqrt_price_to_price();

        // Simple estimation based on current tick liquidity.
        // In reality, CLMM swaps can traverse multiple ticks.
        let liquidity = self.pool.liquidity;

        if liquidity == 0 {
            return Err("Pool has zero liquidity".into());
        }

        // Calculate fee (from total fees, estimate ~0.25% = 25 bps)
        let fee_bps = 25u64; // Raydium CLMM typically uses 0.25% fee
        let fee_multiplier = 10000 - fee_bps;
        let amount_in_with_fee = (amount_in as u128 * fee_multiplier as u128) / 10000;

        // Simplified output calculation using current price
        let output = match direction {
            SwapDirection::Buy => {
                // Quote -> Base (token_0 -> token_1)
                let decimal_adjustment = 10u64.pow(self.pool.mint_decimals_1 as u32) as f64
                    / 10u64.pow(self.pool.mint_decimals_0 as u32) as f64;
                (amount_in_with_fee as f64 * price * decimal_adjustment) as u64
            }
            SwapDirection::Sell => {
                // Base -> Quote (token_1 -> token_0)
                let decimal_adjustment = 10u64.pow(self.pool.mint_decimals_0 as u32) as f64
                    / 10u64.pow(self.pool.mint_decimals_1 as u32) as f64;
                (amount_in_with_fee as f64 / price * decimal_adjustment) as u64
            }
        };

        Ok(output)
    }
}

// ============================================================================
// Market Trait Implementation
// ============================================================================

impl Market for RaydiumClmmMarket {
    fn metadata(&self) -> Result<PoolMetadata, GenericError> {
        // CLMM fees are typically 0.25% (25 bps)
        let trade_fee_bps = 25u64;

        Ok(PoolMetadata {
            address: self.pool_address.clone(),
            dex_name: "Raydium CLMM".to_string(),
            quote_mint: self.pool.token_mint_0,
            base_mint: self.pool.token_mint_1,
            quote_vault: self.pool.token_vault_0,
            base_vault: self.pool.token_vault_1,
            fees: PoolFees {
                trade_fee_bps,
                protocol_fee_bps: None,
            },
        })
    }

    fn financials(&self) -> Result<PoolFinancials, GenericError> {
        Ok(PoolFinancials {
            quote_balance: self.vault_0_balance,
            base_balance: self.vault_1_balance,
            quote_decimals: self.pool.mint_decimals_0,
            base_decimals: self.pool.mint_decimals_1,
        })
    }

    fn calculate_output(
        &self,
        amount_in: u64,
        direction: SwapDirection,
    ) -> Result<u64, GenericError> {
        self.calculate_clmm_output(amount_in, direction)
    }

    fn calculate_price_impact(
        &self,
        amount_in: u64,
        direction: SwapDirection,
    ) -> Result<u64, GenericError> {
        let pre_swap_price = self.current_price()?;

        // Calculate post-swap price (simplified - real CLMM would traverse ticks)
        let output = self.calculate_output(amount_in, direction)?;

        let post_swap_price = match direction {
            SwapDirection::Buy => {
                // After buying base with quote, price increases slightly
                let new_vault_0 = self.vault_0_balance + amount_in;
                let new_vault_1 = self.vault_1_balance.saturating_sub(output);
                if new_vault_1 == 0 {
                    return Err("Insufficient liquidity in pool".into());
                }
                new_vault_0 as f64 / new_vault_1 as f64
            }
            SwapDirection::Sell => {
                // After selling base for quote, price decreases slightly
                let new_vault_1 = self.vault_1_balance + amount_in;
                let new_vault_0 = self.vault_0_balance.saturating_sub(output);
                if new_vault_1 == 0 {
                    return Err("Insufficient liquidity in pool".into());
                }
                new_vault_0 as f64 / new_vault_1 as f64
            }
        };

        Ok(calculate_price_impact_bps(pre_swap_price, post_swap_price))
    }

    fn current_price(&self) -> Result<f64, GenericError> {
        // Use sqrt_price_x64 for accurate CLMM pricing
        let price = self.sqrt_price_to_price();

        // Adjust for decimals
        let decimal_adjustment = 10u64.pow(self.pool.mint_decimals_0 as u32) as f64
            / 10u64.pow(self.pool.mint_decimals_1 as u32) as f64;

        Ok(price * decimal_adjustment)
    }

    fn build_swap_instruction(
        &self,
        context: SwapContext,
        args: SwapArgs,
        direction: SwapDirection,
    ) -> Result<Vec<Instruction>, GenericError> {
        use borsh::to_vec;
        use solana_sdk::instruction::AccountMeta;
        use solana_sdk::program_pack::Pack;
        use spl_associated_token_account::instruction::create_associated_token_account_idempotent;
        use spl_token::state::Account as TokenAccount;

        const TOKEN_ACCOUNT_RENT: u64 = 2_039_280;

        let mut instructions = Vec::new();

        // Check pool has liquidity
        if self.pool.liquidity == 0 {
            return Err("Pool has zero liquidity".into());
        }

        let pair = Pubkey::from_str_const(&self.pool_address);
        let token_program = Pubkey::from_str_const(TOKEN_PROGRAM);
        let token_program_2022 = Pubkey::from_str_const(TOKEN_PROGRAM_2022);
        let memo_program_v2 = Pubkey::from_str_const(MEMO_PROGRAM_V2);

        // Compute tick arrays and bitmap extension PDA
        let tick_array_bitmap = pda_array_bitmap_address(&pair)?.0;

        // Get bitmap extension data if fetched (for tick arrays outside +-512 range)
        let ext_data = context
            .extra_accounts
            .get(&tick_array_bitmap.to_string());

        match direction {
            SwapDirection::Buy => {
                // Buy: token_0 (SOL) -> token_1 (Token)

                // Create temp WSOL account
                let seed = &format!("{}", context.user)[..32];
                let wsol_pubkey =
                    Pubkey::create_with_seed(&context.user, seed, &spl_token::id())?;

                let total_amount = TOKEN_ACCOUNT_RENT + args.amount_in;

                // Create temp WSOL account
                instructions.push(
                    solana_system_interface::instruction::create_account_with_seed(
                        &context.user,
                        &wsol_pubkey,
                        &context.user,
                        seed,
                        total_amount,
                        TokenAccount::LEN as u64,
                        &spl_token::id(),
                    ),
                );

                // Initialize WSOL account
                instructions.push(spl_token::instruction::initialize_account(
                    &spl_token::id(),
                    &wsol_pubkey,
                    &spl_token::native_mint::ID,
                    &context.user,
                )?);

                // Create destination ATA if needed (token uses detected program)
                if !context.destination_ata_exists {
                    instructions.push(create_associated_token_account_idempotent(
                        &context.user,
                        &context.user,
                        &self.pool.token_mint_1,
                        &context.token_program_id,
                    ));
                }

                // Build swap instruction
                // exact_output: is_base_input=false -> amount=exact output, threshold=max input
                let swap_args = if args.exact_output {
                    SwapInstructionArgs {
                        amount: args.min_amount_out, // exact tokens to receive
                        other_amount_threshold: args.amount_in, // max SOL to spend
                        sqrt_price_limit_x64: 0,
                        is_base_input: false,
                    }
                } else {
                    SwapInstructionArgs {
                        amount: args.amount_in,
                        other_amount_threshold: args.min_amount_out,
                        sqrt_price_limit_x64: 0,
                        is_base_input: true,
                    }
                };

                let mut keys: Vec<AccountMeta> = vec![
                    AccountMeta::new(context.user, true),
                    AccountMeta::new_readonly(self.pool.amm_config, false),
                    AccountMeta::new(pair, false),
                    AccountMeta::new(wsol_pubkey, false),
                    AccountMeta::new(context.destination_ata, false),
                    AccountMeta::new(self.pool.token_vault_0, false),
                    AccountMeta::new(self.pool.token_vault_1, false),
                    AccountMeta::new(self.pool.observation_key, false),
                    AccountMeta::new_readonly(token_program, false),
                    AccountMeta::new_readonly(token_program_2022, false),
                    AccountMeta::new_readonly(memo_program_v2, false),
                    AccountMeta::new_readonly(self.pool.token_mint_0, false),
                    AccountMeta::new_readonly(self.pool.token_mint_1, false),
                    AccountMeta::new(tick_array_bitmap, false),
                ];

                // Compute tick array remaining accounts (pure!)
                let remaining_accounts = compute_clmm_remaining_accounts(
                    &self.pool,
                    &pair,
                    true,
                    ext_data.map(|v| v.as_slice()),
                )?;
                for remaining_account in remaining_accounts {
                    keys.push(AccountMeta::new(remaining_account, false));
                }

                // Discriminator for CLMM swap
                let mut data = vec![43, 4, 237, 11, 26, 201, 30, 98];
                let mut args_bytes = to_vec(&swap_args)?;
                data.append(&mut args_bytes);

                instructions.push(Instruction {
                    program_id: Pubkey::from_str_const(RAYDIUM_CLMM),
                    accounts: keys,
                    data,
                });

                // Close temp WSOL account
                instructions.push(spl_token::instruction::close_account(
                    &spl_token::ID,
                    &wsol_pubkey,
                    &context.user,
                    &context.user,
                    &[&context.user],
                )?);
            }

            SwapDirection::Sell => {
                // Sell: token_1 (Token) -> token_0 (SOL)

                // Create temp WSOL account for receiving SOL
                let seed = &format!("{}", context.user)[..32];
                let wsol_pubkey =
                    Pubkey::create_with_seed(&context.user, seed, &spl_token::id())?;

                // Create temp WSOL account
                instructions.push(
                    solana_system_interface::instruction::create_account_with_seed(
                        &context.user,
                        &wsol_pubkey,
                        &context.user,
                        seed,
                        TOKEN_ACCOUNT_RENT,
                        TokenAccount::LEN as u64,
                        &spl_token::id(),
                    ),
                );

                // Initialize WSOL account
                instructions.push(spl_token::instruction::initialize_account(
                    &spl_token::id(),
                    &wsol_pubkey,
                    &spl_token::native_mint::ID,
                    &context.user,
                )?);

                // Create destination ATA if needed (for SOL wrapped token)
                if !context.destination_ata_exists {
                    instructions.push(create_associated_token_account_idempotent(
                        &context.user,
                        &context.user,
                        &self.pool.token_mint_0,
                        &spl_token::ID,
                    ));
                }

                // Build swap instruction
                let swap_args = SwapInstructionArgs {
                    amount: args.amount_in,
                    other_amount_threshold: args.min_amount_out,
                    sqrt_price_limit_x64: 0,
                    is_base_input: true,
                };

                let mut keys: Vec<AccountMeta> = vec![
                    AccountMeta::new(context.user, true),
                    AccountMeta::new_readonly(self.pool.amm_config, false),
                    AccountMeta::new(pair, false),
                    AccountMeta::new(context.destination_ata, false),
                    AccountMeta::new(wsol_pubkey, false),
                    AccountMeta::new(self.pool.token_vault_1, false),
                    AccountMeta::new(self.pool.token_vault_0, false),
                    AccountMeta::new(self.pool.observation_key, false),
                    AccountMeta::new_readonly(token_program, false),
                    AccountMeta::new_readonly(token_program_2022, false),
                    AccountMeta::new_readonly(memo_program_v2, false),
                    AccountMeta::new_readonly(self.pool.token_mint_1, false),
                    AccountMeta::new_readonly(self.pool.token_mint_0, false),
                    AccountMeta::new(tick_array_bitmap, false),
                ];

                // Compute tick array remaining accounts (pure!)
                let remaining_accounts = compute_clmm_remaining_accounts(
                    &self.pool,
                    &pair,
                    false,
                    ext_data.map(|v| v.as_slice()),
                )?;
                for remaining_account in remaining_accounts {
                    keys.push(AccountMeta::new(remaining_account, false));
                }

                // Discriminator for CLMM swap
                let mut data = vec![43, 4, 237, 11, 26, 201, 30, 98];
                let mut args_bytes = to_vec(&swap_args)?;
                data.append(&mut args_bytes);

                instructions.push(Instruction {
                    program_id: Pubkey::from_str_const(RAYDIUM_CLMM),
                    accounts: keys,
                    data,
                });

                // Close temp WSOL account
                instructions.push(spl_token::instruction::close_account(
                    &spl_token::ID,
                    &wsol_pubkey,
                    &context.user,
                    &context.user,
                    &[&context.user],
                )?);
            }
        }

        Ok(instructions)
    }

    fn required_accounts(
        &self,
        _user: Pubkey,
        _direction: SwapDirection,
    ) -> Result<RequiredAccounts, GenericError> {
        // Raydium CLMM: base_mint = Token, quote_mint = SOL (standard convention)
        // CLMM instruction builder uses destination_ata as the Token-side user account
        // for BOTH directions (SOL side always uses a temp WSOL account).
        // So destination_mint must always be the Token (token_mint_1).
        let (source_mint, destination_mint) = (self.pool.token_mint_0, self.pool.token_mint_1);

        // Fetch bitmap extension account — needed for tick arrays outside +-512 index range
        let pair = Pubkey::from_str_const(&self.pool_address);
        let bitmap_ext_pda = pda_array_bitmap_address(&pair)?.0;

        Ok(RequiredAccounts {
            source_mint,
            destination_mint,
            account_data: vec![bitmap_ext_pda],
        })
    }
}

// ============================================================================
// ConcentratedLiquidity Trait Implementation
// ============================================================================

impl ConcentratedLiquidity for RaydiumClmmMarket {
    fn active_bin(&self) -> Result<i32, GenericError> {
        Ok(self.pool.tick_current)
    }

    fn liquidity_distribution(&self) -> Result<Vec<(i32, u64)>, GenericError> {
        // For now, return current tick with total liquidity.
        // In production, this would traverse tick arrays.
        Ok(vec![(self.pool.tick_current, self.pool.liquidity as u64)])
    }
}
