//! Meteora Dynamic Liquidity Market Maker (DLMM) DEX crate.
//!
//! Implements the `Market` and `ConcentratedLiquidity` traits from `thunder-core`
//! for Meteora DLMM bin-based concentrated liquidity pools.


use borsh::BorshDeserialize;
use solana_pubkey::Pubkey;
use solana_sdk::instruction::{AccountMeta, Instruction};

use thunder_core::{
    calculate_price_impact_bps, quote_priority, ConcentratedLiquidity, GenericError, Market,
    PoolFees, PoolFinancials, PoolMetadata, RequiredAccounts, SwapArgs, SwapContext,
    SwapDirection, MEMO_PROGRAM_V2, TOKEN_PROGRAM, WSOL, infer_mint_decimals,
};

// ---------------------------------------------------------------------------
// DEX-specific constants
// ---------------------------------------------------------------------------

pub const METEORA_DYNAMIC_LMM: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";
pub const METEORA_EVENTS_AUTHORITY: &str = "D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6";

// ---------------------------------------------------------------------------
// On-chain model structs
// ---------------------------------------------------------------------------

#[derive(
    BorshDeserialize, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq, Clone, Hash,
)]
pub struct StaticParameters {
    pub base_factor: u16,
    pub filter_period: u16,
    pub decay_period: u16,
    pub reduction_factor: u16,
    pub variable_fee_control: u32,
    pub max_volatility_accumulator: u32,
    pub min_bin_id: i32,
    pub max_bin_id: i32,
    pub protocol_share: u16,
    pub base_fee_power_factor: u8,
    pub padding: [u8; 5],
}

#[derive(
    BorshDeserialize, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq, Clone, Hash,
)]
pub struct VariableParameters {
    pub volatility_accumulator: u32,
    pub volatility_reference: u32,
    pub index_reference: i32,
    pub padding: [u8; 4],
    pub last_update_timestamp: i64,
    pub padding1: [u8; 8],
}

#[derive(
    BorshDeserialize, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq, Clone, Hash,
)]
pub struct ProtocolFee {
    pub amount_x: u64,
    pub amount_y: u64,
}

#[derive(
    BorshDeserialize, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq, Clone, Hash,
)]
pub struct RewardInfo {
    pub mint: Pubkey,
    pub vault: Pubkey,
    pub funder: Pubkey,
    pub reward_duration: u64,
    pub reward_duration_end: u64,
    pub reward_rate: u128,
    pub last_update_time: u64,
    pub cumulative_seconds_with_empty_liquidity_reward: u64,
}

#[derive(BorshDeserialize, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MeteoraDLMMPool {
    pub parameters: StaticParameters,
    pub v_parameters: VariableParameters,
    pub bump_seed: [u8; 1],
    pub bin_step_seed: [u8; 2],
    pub pair_type: u8,
    pub active_id: i32,
    pub bin_step: u16,
    pub status: u8,
    pub require_base_factor_seed: u8,
    pub base_factor_seed: [u8; 2],
    pub activation_type: u8,
    pub creator_pool_on_off_control: u8,
    pub token_x_mint: Pubkey,
    pub token_y_mint: Pubkey,
    pub reserve_x: Pubkey,
    pub reserve_y: Pubkey,
    pub protocol_fee: ProtocolFee,
    pub padding1: [u8; 32],
    pub reward_infos: [RewardInfo; 2],
    pub oracle: Pubkey,
    pub bin_array_bitmap: [u64; 16],
    pub last_updated_at: i64,
    pub padding2: [u8; 32],
    pub pre_activation_swap_address: Pubkey,
    pub base_key: Pubkey,
    pub activation_point: u64,
    pub pre_activation_duration: u64,
    pub padding3: [u8; 8],
    pub padding4: u64,
    pub creator: Pubkey,
    pub token_mint_x_program_flag: u8,
    pub token_mint_y_program_flag: u8,
    pub reserved: [u8; 22],
}

// ---------------------------------------------------------------------------
// Utility: bin array PDA derivation
// ---------------------------------------------------------------------------

const BIN_ARRAY: &[u8] = b"bin_array";

pub fn derive_bin_array_pda(lb_pair: Pubkey, bin_array_index: i64) -> (Pubkey, u8) {
    let meteora_dlmm_program = Pubkey::from_str_const(METEORA_DYNAMIC_LMM);
    Pubkey::find_program_address(
        &[BIN_ARRAY, lb_pair.as_ref(), &bin_array_index.to_le_bytes()],
        &meteora_dlmm_program,
    )
}

// ---------------------------------------------------------------------------
// Swap instruction args (Borsh-serialized on-chain)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, borsh::BorshSerialize)]
struct SwapInstructionArgs {
    amount_in: u64,
    min_amount_out: u64,
    remaining_accounts_info: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Market wrapper
// ---------------------------------------------------------------------------

pub struct MeteoraDlmmMarket {
    pub pool: MeteoraDLMMPool,
    pub pool_address: String,
    pub reserve_x_balance: u64,
    pub reserve_y_balance: u64,
    pub token_x_decimals: u8,
    pub token_y_decimals: u8,
    pub flipped: bool,
}

impl MeteoraDlmmMarket {
    pub fn new(pool: MeteoraDLMMPool, pool_address: String) -> Self {
        let flipped = quote_priority(&pool.token_x_mint).unwrap_or(usize::MAX) < quote_priority(&pool.token_y_mint).unwrap_or(usize::MAX);
        let token_x_decimals = infer_mint_decimals(&pool.token_x_mint);
        let token_y_decimals = infer_mint_decimals(&pool.token_y_mint);
        Self {
            pool,
            pool_address,
            reserve_x_balance: 0,
            reserve_y_balance: 0,
            token_x_decimals,
            token_y_decimals,
            flipped,
        }
    }

    /// Calculate output for DLMM swap.
    ///
    /// Simplified calculation — real DLMM swaps traverse multiple bins.
    /// For production, use the actual DLMM math library or RPC simulation.
    fn calculate_dlmm_output(
        &self,
        amount_in: u64,
        direction: SwapDirection,
    ) -> Result<u64, GenericError> {
        // Use raw bin price (no decimal adjustment) since amount_in/output are raw token units.
        let bin_step = self.pool.bin_step as f64;
        let active_id = self.pool.active_id;
        let base = 1.0 + (bin_step / 10000.0);
        let price = base.powi(active_id);

        // Fee = base_factor * bin_step / 10_000 (in bps).
        let fee_bps = (self.pool.parameters.base_factor as u64 * self.pool.bin_step as u64) / 10_000;

        let fee_multiplier = 10000 - fee_bps;
        let amount_in_with_fee = (amount_in as u128 * fee_multiplier as u128) / 10000;

        // After normalization, Buy = spend quote to get base, Sell = spend base to get quote.
        // Flip the physical direction when the pool sides are swapped.
        let effective_direction = if self.flipped {
            match direction {
                SwapDirection::Buy => SwapDirection::Sell,
                SwapDirection::Sell => SwapDirection::Buy,
            }
        } else {
            direction
        };

        let output = match effective_direction {
            SwapDirection::Buy => {
                // Y → X (SOL → Token): output ≈ amount_in / price
                (amount_in_with_fee as f64 / price) as u64
            }
            SwapDirection::Sell => {
                // X → Y (Token → SOL): output ≈ amount_in * price
                (amount_in_with_fee as f64 * price) as u64
            }
        };

        Ok(output)
    }

    /// Bin price: (1 + bin_step / 10000)^active_id * 10^(dec_x - dec_y)
    /// Result = token_y per token_x (human-readable).
    /// E.g. for SOL/USDC: returns ~82 (USDC per SOL).
    fn calculate_dlmm_price(&self) -> f64 {
        let bin_step = self.pool.bin_step as f64;
        let active_id = self.pool.active_id;
        let base = 1.0 + (bin_step / 10000.0);
        let raw = base.powi(active_id);
        raw * 10f64.powi(self.token_x_decimals as i32 - self.token_y_decimals as i32)
    }
}

// ---------------------------------------------------------------------------
// Market trait
// ---------------------------------------------------------------------------

impl Market for MeteoraDlmmMarket {
    fn metadata(&self) -> Result<PoolMetadata, GenericError> {
        // Fee = base_factor * bin_step / 10_000 (in bps).
        let trade_fee_bps = (self.pool.parameters.base_factor as u64 * self.pool.bin_step as u64) / 10_000;

        let (quote_mint, base_mint, quote_vault, base_vault) = if self.flipped {
            (self.pool.token_x_mint, self.pool.token_y_mint, self.pool.reserve_x, self.pool.reserve_y)
        } else {
            (self.pool.token_y_mint, self.pool.token_x_mint, self.pool.reserve_y, self.pool.reserve_x)
        };

        Ok(PoolMetadata {
            address: self.pool_address.clone(),
            dex_name: "Meteora DLMM".to_string(),
            quote_mint,
            base_mint,
            quote_vault,
            base_vault,
            fees: PoolFees {
                trade_fee_bps,
                protocol_fee_bps: Some(self.pool.parameters.protocol_share as u64),
            },
        })
    }

    fn financials(&self) -> Result<PoolFinancials, GenericError> {
        let (quote_balance, base_balance, quote_decimals, base_decimals) = if self.flipped {
            (self.reserve_x_balance, self.reserve_y_balance, self.token_x_decimals, self.token_y_decimals)
        } else {
            (self.reserve_y_balance, self.reserve_x_balance, self.token_y_decimals, self.token_x_decimals)
        };
        Ok(PoolFinancials {
            quote_balance,
            base_balance,
            quote_decimals,
            base_decimals,
        })
    }

    fn calculate_output(
        &self,
        amount_in: u64,
        direction: SwapDirection,
    ) -> Result<u64, GenericError> {
        self.calculate_dlmm_output(amount_in, direction)
    }

    fn calculate_price_impact(
        &self,
        amount_in: u64,
        direction: SwapDirection,
    ) -> Result<u64, GenericError> {
        let pre_swap_price = self.current_price()?;
        let output = self.calculate_output(amount_in, direction)?;

        // Use the effective (physical) direction for reserve math.
        let effective_direction = if self.flipped {
            match direction {
                SwapDirection::Buy => SwapDirection::Sell,
                SwapDirection::Sell => SwapDirection::Buy,
            }
        } else {
            direction
        };

        let post_swap_price = match effective_direction {
            SwapDirection::Buy => {
                let new_reserve_y = self.reserve_y_balance + amount_in;
                let new_reserve_x = self.reserve_x_balance.saturating_sub(output);
                if new_reserve_x == 0 {
                    return Err("Insufficient liquidity in pool".into());
                }
                new_reserve_y as f64 / new_reserve_x as f64
            }
            SwapDirection::Sell => {
                let new_reserve_x = self.reserve_x_balance + amount_in;
                let new_reserve_y = self.reserve_y_balance.saturating_sub(output);
                if new_reserve_x == 0 {
                    return Err("Insufficient liquidity in pool".into());
                }
                new_reserve_y as f64 / new_reserve_x as f64
            }
        };

        Ok(calculate_price_impact_bps(pre_swap_price, post_swap_price))
    }

    fn current_price(&self) -> Result<f64, GenericError> {
        let price = self.calculate_dlmm_price();
        // price = token_y per token_x (human-readable).
        // NOT flipped (quote=y, base=x): price is already quote_per_base. Return as-is.
        // Flipped (quote=x, base=y): want x_per_y = 1/price.
        if self.flipped {
            if price == 0.0 {
                return Err("Pool has zero price".into());
            }
            Ok(1.0 / price)
        } else {
            Ok(price)
        }
    }

    fn build_swap_instruction(
        &self,
        context: SwapContext,
        args: SwapArgs,
        direction: SwapDirection,
    ) -> Result<Vec<Instruction>, GenericError> {
        use borsh::to_vec;
        use solana_system_interface::instruction as system_instruction;
        use spl_associated_token_account::instruction::create_associated_token_account_idempotent;
        use spl_token::instruction::initialize_account;
        use spl_token::state::Account as TokenAccount;
        use solana_sdk::program_pack::Pack;

        let mut instructions = Vec::new();
        const TOKEN_ACCOUNT_RENT: u64 = 2_039_280;

        let native_mint = spl_token::native_mint::ID;
        let wsol = Pubkey::from_str_const(WSOL);
        let meteora_dlmm_program = Pubkey::from_str_const(METEORA_DYNAMIC_LMM);

        // Derive bin array PDA from active bin
        const MAX_BIN_PER_ARRAY: i64 = 70;
        let bin_array_index = (self.pool.active_id as i64).div_euclid(MAX_BIN_PER_ARRAY);
        let (bin_array_pda, _) =
            derive_bin_array_pda(Pubkey::from_str_const(&self.pool_address), bin_array_index);

        // Determine correct token programs for Token X and Token Y
        let token_x_program = if self.pool.token_x_mint == wsol {
            Pubkey::from_str_const(TOKEN_PROGRAM)
        } else {
            context.token_program_id
        };
        let token_y_program = if self.pool.token_y_mint == wsol {
            Pubkey::from_str_const(TOKEN_PROGRAM)
        } else {
            context.token_program_id
        };

        // When flipped, the Market's Buy (spend quote to get base) maps to the
        // physical Sell direction (X→Y becomes Y→X) and vice versa.
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
                // Buy: Y (SOL) → X (Token)

                // 1. Create temporary WSOL account for input
                let seed = &format!("{}", context.user)[..32];
                let wsol_pubkey =
                    Pubkey::create_with_seed(&context.user, seed, &spl_token::id())?;

                let total_amount = TOKEN_ACCOUNT_RENT + args.amount_in;

                instructions.push(system_instruction::create_account_with_seed(
                    &context.user,
                    &wsol_pubkey,
                    &context.user,
                    seed,
                    total_amount,
                    TokenAccount::LEN as u64,
                    &spl_token::id(),
                ));

                // 2. Initialize WSOL account
                instructions.push(initialize_account(
                    &spl_token::id(),
                    &wsol_pubkey,
                    &native_mint,
                    &context.user,
                )?);

                // 3. Create destination ATA if needed
                if !context.destination_ata_exists {
                    instructions.push(create_associated_token_account_idempotent(
                        &context.user,
                        &context.user,
                        &self.pool.token_x_mint,
                        &context.token_program_id,
                    ));
                }

                // 4. Build swap instruction
                let swap_args = SwapInstructionArgs {
                    amount_in: args.amount_in,
                    min_amount_out: args.min_amount_out,
                    remaining_accounts_info: vec![],
                };

                let keys: Vec<AccountMeta> = vec![
                    AccountMeta::new(Pubkey::from_str_const(&self.pool_address), false),
                    AccountMeta::new_readonly(meteora_dlmm_program, false),
                    AccountMeta::new(self.pool.reserve_x, false),
                    AccountMeta::new(self.pool.reserve_y, false),
                    AccountMeta::new(wsol_pubkey, false), // user_token_in = temp WSOL
                    AccountMeta::new(context.destination_ata, false),
                    AccountMeta::new_readonly(self.pool.token_x_mint, false),
                    AccountMeta::new_readonly(self.pool.token_y_mint, false),
                    AccountMeta::new(self.pool.oracle, false),
                    AccountMeta::new_readonly(meteora_dlmm_program, false), // Host Fee In
                    AccountMeta::new(context.user, true),
                    AccountMeta::new_readonly(token_x_program, false),
                    AccountMeta::new_readonly(token_y_program, false),
                    AccountMeta::new_readonly(Pubkey::from_str_const(MEMO_PROGRAM_V2), false),
                    AccountMeta::new_readonly(
                        Pubkey::from_str_const(METEORA_EVENTS_AUTHORITY),
                        false,
                    ),
                    AccountMeta::new_readonly(meteora_dlmm_program, false), // Program
                    AccountMeta::new(bin_array_pda, false),
                ];

                // Discriminator for DLMM swap
                let mut data = vec![65, 75, 63, 76, 235, 91, 91, 136];
                let mut args_bytes = to_vec(&swap_args)?;
                data.append(&mut args_bytes);

                instructions.push(Instruction {
                    program_id: meteora_dlmm_program,
                    accounts: keys,
                    data,
                });

                // 5. Close temporary WSOL account
                instructions.push(spl_token::instruction::close_account(
                    &spl_token::id(),
                    &wsol_pubkey,
                    &context.user,
                    &context.user,
                    &[],
                )?);
            }

            SwapDirection::Sell => {
                // Sell: X (Token) → Y (SOL)

                // 1. Create temporary WSOL account for output
                let seed = &format!("{}", context.user)[..32];
                let wsol_pubkey =
                    Pubkey::create_with_seed(&context.user, seed, &spl_token::id())?;

                instructions.push(system_instruction::create_account_with_seed(
                    &context.user,
                    &wsol_pubkey,
                    &context.user,
                    seed,
                    TOKEN_ACCOUNT_RENT,
                    TokenAccount::LEN as u64,
                    &spl_token::id(),
                ));

                // 2. Initialize WSOL account
                instructions.push(initialize_account(
                    &spl_token::id(),
                    &wsol_pubkey,
                    &native_mint,
                    &context.user,
                )?);

                // 3. Build swap instruction
                let swap_args = SwapInstructionArgs {
                    amount_in: args.amount_in,
                    min_amount_out: args.min_amount_out,
                    remaining_accounts_info: vec![],
                };

                let keys: Vec<AccountMeta> = vec![
                    AccountMeta::new(Pubkey::from_str_const(&self.pool_address), false),
                    AccountMeta::new_readonly(meteora_dlmm_program, false),
                    AccountMeta::new(self.pool.reserve_x, false),
                    AccountMeta::new(self.pool.reserve_y, false),
                    AccountMeta::new(context.source_ata, false),
                    AccountMeta::new(wsol_pubkey, false), // user_token_out = temp WSOL
                    AccountMeta::new_readonly(self.pool.token_x_mint, false),
                    AccountMeta::new_readonly(self.pool.token_y_mint, false),
                    AccountMeta::new(self.pool.oracle, false),
                    AccountMeta::new_readonly(meteora_dlmm_program, false), // Host Fee In
                    AccountMeta::new(context.user, true),
                    AccountMeta::new_readonly(token_x_program, false),
                    AccountMeta::new_readonly(token_y_program, false),
                    AccountMeta::new_readonly(Pubkey::from_str_const(MEMO_PROGRAM_V2), false),
                    AccountMeta::new_readonly(
                        Pubkey::from_str_const(METEORA_EVENTS_AUTHORITY),
                        false,
                    ),
                    AccountMeta::new_readonly(meteora_dlmm_program, false), // Program
                    AccountMeta::new(bin_array_pda, false),
                ];

                // Discriminator for DLMM swap
                let mut data = vec![65, 75, 63, 76, 235, 91, 91, 136];
                let mut args_bytes = to_vec(&swap_args)?;
                data.append(&mut args_bytes);

                instructions.push(Instruction {
                    program_id: meteora_dlmm_program,
                    accounts: keys,
                    data,
                });

                // 4. Close temporary WSOL account
                instructions.push(spl_token::instruction::close_account(
                    &spl_token::id(),
                    &wsol_pubkey,
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
            SwapDirection::Buy => if self.flipped {
                (self.pool.token_x_mint, self.pool.token_y_mint)
            } else {
                (self.pool.token_y_mint, self.pool.token_x_mint)
            },
            SwapDirection::Sell => if self.flipped {
                (self.pool.token_y_mint, self.pool.token_x_mint)
            } else {
                (self.pool.token_x_mint, self.pool.token_y_mint)
            },
        };

        Ok(RequiredAccounts {
            source_mint,
            destination_mint,
            account_data: vec![],
        })
    }
}

// ---------------------------------------------------------------------------
// ConcentratedLiquidity trait (bin-based concentrated liquidity)
// ---------------------------------------------------------------------------

impl ConcentratedLiquidity for MeteoraDlmmMarket {
    fn active_bin(&self) -> Result<i32, GenericError> {
        Ok(self.pool.active_id)
    }

    fn liquidity_distribution(&self) -> Result<Vec<(i32, u64)>, GenericError> {
        // Simplified: return current bin only. Production would traverse bin arrays.
        let liquidity = (self.reserve_x_balance + self.reserve_y_balance) / 2;
        Ok(vec![(self.pool.active_id, liquidity)])
    }
}
