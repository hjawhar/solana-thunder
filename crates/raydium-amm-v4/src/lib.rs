
use borsh::{BorshDeserialize, to_vec};
use solana_pubkey::Pubkey;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_system_interface::instruction as system_instruction;
use spl_associated_token_account::instruction::create_associated_token_account_idempotent;
use spl_token::instruction::initialize_account;
use solana_sdk::program_pack::Pack;
use spl_token::state::Account as TokenAccount;

use thunder_core::{
    GenericError, Market, SwapArgs, SwapDirection, PoolMetadata, PoolFinancials, PoolFees,
    SwapContext, RequiredAccounts,
    constant_product_swap, calculate_price_impact_bps,
    WSOL, TOKEN_PROGRAM,
};

// ---------------------------------------------------------------------------
// DEX-specific constants
// ---------------------------------------------------------------------------

pub const RAYDIUM_LIQUIDITY_POOL_V4: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
pub const RAYDIUM_AUTHORITY_V4: &str = "5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1";

// ---------------------------------------------------------------------------
// On-chain models
// ---------------------------------------------------------------------------




#[derive(Debug, BorshDeserialize, Clone, serde::Serialize, serde::Deserialize)]
pub struct RaydiumAMMV4 {
    pub status: u64,
    pub nonce: u64,
    pub max_order: u64,
    pub depth: u64,
    pub base_decimal: u64,
    pub quote_decimal: u64,
    pub state: u64,
    pub reset_flag: u64,
    pub min_size: u64,
    pub vol_max_cut_ratio: u64,
    pub amount_wave: u64,
    pub base_lot_size: u64,
    pub quote_lot_size: u64,
    pub min_price_multiplier: u64,
    pub max_price_multiplier: u64,
    pub system_decimal_value: u64,
    pub min_separate_numerator: u64,
    pub min_separate_denominator: u64,
    pub trade_fee_numerator: u64,
    pub trade_fee_denominator: u64,
    pub pnl_numerator: u64,
    pub pnl_denominator: u64,
    pub swap_fee_numerator: u64,
    pub swap_fee_denominator: u64,
    pub base_need_take_pnl: u64,
    pub quote_need_take_pnl: u64,
    pub quote_total_pnl: u64,
    pub base_total_pnl: u64,
    pub pool_open_time: u64,
    pub punish_pc_amount: u64,
    pub punish_coin_amount: u64,
    pub orderbook_to_init_time: u64,
    pub swap_base_in_amount: u128,
    pub swap_quote_out_amount: u128,
    pub swap_base_2_quote_fee: u64,
    pub swap_quote_in_amount: u128,
    pub swap_base_out_amount: u128,
    pub swap_quote_2_base_fee: u64,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub lp_mint: Pubkey,
    pub open_orders: Pubkey,
    pub market_id: Pubkey,
    pub market_program_id: Pubkey,
    pub target_oders: Pubkey,
    pub withdraw_queue: Pubkey,
    pub lp_vault: Pubkey,
    pub owner: Pubkey,
    pub lp_reserve: u64,
    pub padding: [u64; 3],
}


// ---------------------------------------------------------------------------
// Swap instruction data
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, borsh::BorshSerialize)]
struct SwapInstructionArgs {
    amount_in: u64,
    min_amount_out: u64,
}

// ---------------------------------------------------------------------------
// Market wrapper
// ---------------------------------------------------------------------------

pub struct RaydiumAmmV4Market {
    pub pool: RaydiumAMMV4,
    pub pool_address: String,
    pub quote_balance: u64,
    pub base_balance: u64,
}

impl RaydiumAmmV4Market {
    pub fn new(
        pool: RaydiumAMMV4,
        pool_address: String,
        quote_balance: u64,
        base_balance: u64,
    ) -> Self {
        Self { pool, pool_address, quote_balance, base_balance }
    }
}

// ---------------------------------------------------------------------------
// Market trait implementation
// ---------------------------------------------------------------------------

impl Market for RaydiumAmmV4Market {
    fn metadata(&self) -> Result<PoolMetadata, GenericError> {
        let fee_bps = (self.pool.trade_fee_numerator as f64
            / self.pool.trade_fee_denominator as f64
            * 10000.0) as u64;

        Ok(PoolMetadata {
            address: self.pool_address.clone(),
            dex_name: "Raydium AMM V4".to_string(),
            quote_mint: self.pool.quote_mint,
            base_mint: self.pool.base_mint,
            quote_vault: self.pool.quote_vault,
            base_vault: self.pool.base_vault,
            fees: PoolFees {
                trade_fee_bps: fee_bps,
                protocol_fee_bps: None,
            },
        })
    }

    fn financials(&self) -> Result<PoolFinancials, GenericError> {
        Ok(PoolFinancials {
            quote_balance: self.quote_balance,
            base_balance: self.base_balance,
            quote_decimals: 9, // SOL decimals
            base_decimals: self.pool.base_decimal as u8,
        })
    }

    fn calculate_output(
        &self,
        amount_in: u64,
        direction: SwapDirection,
    ) -> Result<u64, GenericError> {
        let fee_bps = (self.pool.trade_fee_numerator as f64
            / self.pool.trade_fee_denominator as f64
            * 10000.0) as u64;

        match direction {
            SwapDirection::Buy => {
                // Quote -> Base (SOL -> Token)
                constant_product_swap(
                    self.quote_balance,
                    self.base_balance,
                    amount_in,
                    fee_bps,
                )
            }
            SwapDirection::Sell => {
                // Base -> Quote (Token -> SOL)
                constant_product_swap(
                    self.base_balance,
                    self.quote_balance,
                    amount_in,
                    fee_bps,
                )
            }
        }
    }

    fn calculate_price_impact(
        &self,
        amount_in: u64,
        direction: SwapDirection,
    ) -> Result<u64, GenericError> {
        let pre_swap_price = self.current_price()?;
        let output = self.calculate_output(amount_in, direction)?;

        let post_swap_price = match direction {
            SwapDirection::Buy => {
                let new_quote = self.quote_balance + amount_in;
                let new_base = self.base_balance - output;
                new_quote as f64 / new_base as f64
            }
            SwapDirection::Sell => {
                let new_base = self.base_balance + amount_in;
                let new_quote = self.quote_balance - output;
                new_quote as f64 / new_base as f64
            }
        };

        Ok(calculate_price_impact_bps(pre_swap_price, post_swap_price))
    }

    fn current_price(&self) -> Result<f64, GenericError> {
        if self.base_balance == 0 {
            return Err("Pool has zero base balance".into());
        }
        Ok(self.quote_balance as f64 / self.base_balance as f64)
    }

    fn build_swap_instruction(
        &self,
        context: SwapContext,
        args: SwapArgs,
        direction: SwapDirection,
    ) -> Result<Vec<Instruction>, GenericError> {
        let mut instructions = Vec::new();

        // Standard rent for token account (165 bytes)
        const TOKEN_ACCOUNT_RENT: u64 = 2_039_280;

        let native_mint = spl_token::native_mint::ID;
        let token_program = Pubkey::from_str_const(TOKEN_PROGRAM);
        let amm_authority = Pubkey::from_str_const(RAYDIUM_AUTHORITY_V4);
        let amm_id = Pubkey::from_str_const(&self.pool_address);

        match direction {
            SwapDirection::Buy => {
                // Buy: Quote (SOL) -> Base (Token)
                let base_mint = if Pubkey::from_str_const(WSOL) == self.pool.base_mint {
                    self.pool.quote_mint
                } else {
                    self.pool.base_mint
                };

                // Create temporary WSOL account for input
                let seed = &format!("{}", context.user)[..32];
                let wsol_pubkey = Pubkey::create_with_seed(
                    &context.user,
                    seed,
                    &spl_token::id(),
                )?;

                let total_amount = TOKEN_ACCOUNT_RENT + args.amount_in;

                // 1. Create temporary WSOL account
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
                        &base_mint,
                        &context.token_program_id,
                    ));
                }

                // 4. Build swap instruction
                let keys = vec![
                    AccountMeta::new_readonly(token_program, false),
                    AccountMeta::new(amm_id, false),
                    AccountMeta::new(amm_authority, false),
                    AccountMeta::new(amm_id, false),
                    AccountMeta::new(self.pool.base_vault, false),
                    AccountMeta::new(self.pool.quote_vault, false),
                    AccountMeta::new(amm_id, false),
                    AccountMeta::new(amm_id, false),
                    AccountMeta::new(amm_id, false),
                    AccountMeta::new(amm_id, false),
                    AccountMeta::new(amm_id, false),
                    AccountMeta::new(amm_id, false),
                    AccountMeta::new(amm_id, false),
                    AccountMeta::new(amm_id, false),
                    AccountMeta::new(wsol_pubkey, false),
                    AccountMeta::new(context.destination_ata, false),
                    AccountMeta::new(context.user, true),
                ];

                // Discriminator 9 = swap_base_in (exact input), 11 = swap_base_out (exact output)
                let disc = if args.exact_output { 11u8 } else { 9u8 };
                let swap_args = SwapInstructionArgs {
                    amount_in: args.amount_in,
                    min_amount_out: args.min_amount_out,
                };

                let mut data = vec![disc];
                let mut args_bytes = to_vec(&swap_args)?;
                data.append(&mut args_bytes);

                instructions.push(Instruction {
                    program_id: Pubkey::from_str_const(RAYDIUM_LIQUIDITY_POOL_V4),
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
                // Sell: Base (Token) -> Quote (SOL)
                let quote_mint = self.pool.quote_mint;

                // Create temporary WSOL account for output
                let seed = &format!("{}", context.user)[..32];
                let wsol_pubkey = Pubkey::create_with_seed(
                    &context.user,
                    seed,
                    &spl_token::id(),
                )?;

                // 1. Create temporary WSOL account
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

                // 3. Create destination ATA if needed (WSOL always uses standard Token program)
                if !context.destination_ata_exists {
                    instructions.push(create_associated_token_account_idempotent(
                        &context.user,
                        &context.user,
                        &quote_mint,
                        &spl_token::ID,
                    ));
                }

                // 4. Build swap instruction
                let keys = vec![
                    AccountMeta::new_readonly(token_program, false),
                    AccountMeta::new(amm_id, false),
                    AccountMeta::new(amm_authority, false),
                    AccountMeta::new(amm_id, false),
                    AccountMeta::new(self.pool.base_vault, false),
                    AccountMeta::new(self.pool.quote_vault, false),
                    AccountMeta::new(amm_id, false),
                    AccountMeta::new(amm_id, false),
                    AccountMeta::new(amm_id, false),
                    AccountMeta::new(amm_id, false),
                    AccountMeta::new(amm_id, false),
                    AccountMeta::new(amm_id, false),
                    AccountMeta::new(amm_id, false),
                    AccountMeta::new(amm_id, false),
                    AccountMeta::new(context.source_ata, false),
                    AccountMeta::new(wsol_pubkey, false),
                    AccountMeta::new(context.user, true),
                ];

                let swap_args = SwapInstructionArgs {
                    amount_in: args.amount_in,
                    min_amount_out: args.min_amount_out,
                };

                let mut data = vec![9u8];
                let mut args_bytes = to_vec(&swap_args)?;
                data.append(&mut args_bytes);

                instructions.push(Instruction {
                    program_id: Pubkey::from_str_const(RAYDIUM_LIQUIDITY_POOL_V4),
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
        }

        Ok(instructions)
    }

    fn required_accounts(
        &self,
        _user: Pubkey,
        direction: SwapDirection,
    ) -> Result<RequiredAccounts, GenericError> {
        let wsol = Pubkey::from_str_const(WSOL);
        let (sol_mint, token_mint) = if self.pool.base_mint == wsol {
            (self.pool.base_mint, self.pool.quote_mint) // flipped: base=SOL, quote=Token
        } else {
            (self.pool.quote_mint, self.pool.base_mint) // standard: quote=SOL, base=Token
        };

        let (source_mint, destination_mint) = match direction {
            SwapDirection::Buy => (sol_mint, token_mint),   // SOL -> Token
            SwapDirection::Sell => (token_mint, sol_mint),   // Token -> SOL
        };

        Ok(RequiredAccounts {
            source_mint,
            destination_mint,
            account_data: vec![],
        })
    }
}
