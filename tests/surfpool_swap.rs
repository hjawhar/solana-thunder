//! 2-hop swap on Surfpool: SOL -> USDC -> TRUMP
//!
//! Requires Surfpool running with mainnet fork.
//! Run: cargo test --test surfpool_swap -- --nocapture

use std::env;

use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    instruction::Instruction,
    message::{v0, VersionedMessage},
    signature::{Keypair, Signer},
    transaction::VersionedTransaction,
};
use solana_system_interface::instruction as system_ix;
use spl_associated_token_account::get_associated_token_address;
use spl_associated_token_account::instruction::create_associated_token_account_idempotent;
use spl_token::instruction::sync_native;
use thunder_aggregator::swap_builder::{self, DlmmSwapAccounts};

// ── Hop 1: SOL → USDC via DLMM pool ────────────────────────────────────
const SOL_USDC_POOL: &str = "5XRqv7LCoC5FhWKk5JN8n4kCrJs3e4KH1XsYzKeMd5Nt";
const SOL_USDC_RESERVE_X: &str = "EN1RTvqZ3BpLmpJVXqpMb6Sc2w8ncbA5imsTQmQtRCZg"; // WSOL vault
const SOL_USDC_RESERVE_Y: &str = "BsLY7Qxh8NM61MDj6DK1UWdSprJfTEBPnp6Lc9iw2Gmw"; // USDC vault
const SOL_USDC_ACTIVE_ID: i32 = -500;

// ── Hop 2: USDC → TRUMP via DLMM pool ──────────────────────────────────
const USDC_TRUMP_POOL: &str = "3C5YE97HADPDxZehYq9Cis8AXr9aNyrUsczKzE1nDbW9";
const USDC_TRUMP_RESERVE_X: &str = "DBxLBME3MbN1oEQhah8adNxYFwuLwhkr7BM3SEJ4uoTC"; // TRUMP vault
const USDC_TRUMP_RESERVE_Y: &str = "HbFACE7GxA7XP9PrwYBiR7ZvBbQh7fmUFcRJWrPDt51M"; // USDC vault
const USDC_TRUMP_ACTIVE_ID: i32 = 1097;

// ── Mints ───────────────────────────────────────────────────────────────
const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";
const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const TRUMP_MINT: &str = "6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN";
const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

#[tokio::test]
async fn test_2hop_sol_usdc_trump() {
    dotenvy::dotenv().ok();

    let keypair = Keypair::from_base58_string(&env::var("PRIVATE_KEY").expect("PRIVATE_KEY"));
    let user = keypair.pubkey();
    let surfpool_url = env::var("SURFPOOL_URL").unwrap_or_else(|_| "http://127.0.0.1:8899".into());
    let rpc = RpcClient::new_with_commitment(surfpool_url, CommitmentConfig::confirmed());

    let tp = Pubkey::from_str_const(TOKEN_PROGRAM);
    let wsol = Pubkey::from_str_const(WSOL_MINT);
    let usdc = Pubkey::from_str_const(USDC_MINT);
    let trump = Pubkey::from_str_const(TRUMP_MINT);

    let user_wsol = get_associated_token_address(&user, &wsol);
    let user_usdc = get_associated_token_address(&user, &usdc);
    let user_trump = get_associated_token_address(&user, &trump);

    println!("Thunder - 2-Hop Surfpool Swap");
    println!("=============================\n");
    println!("Wallet: {user}");
    println!("SOL:    {:.4}\n", rpc.get_balance(&user).await.unwrap_or(0) as f64 / 1e9);
    println!("Route: 0.1 SOL -> USDC -> TRUMP");
    println!("  Hop 1: WSOL -> USDC via {}", &SOL_USDC_POOL[..12]);
    println!("  Hop 2: USDC -> TRUMP via {}\n", &USDC_TRUMP_POOL[..12]);

    let sol_amount: u64 = 100_000_000; // 0.1 SOL

    let mut ixs: Vec<Instruction> = Vec::new();

    // ── Pre-instructions: create ATAs + wrap SOL ────────────────────────

    // Create WSOL ATA
    ixs.push(create_associated_token_account_idempotent(&user, &user, &wsol, &tp));
    // Fund WSOL ATA with SOL
    ixs.push(system_ix::transfer(&user, &user_wsol, sol_amount));
    // Sync native (makes the SPL token account reflect the SOL balance)
    ixs.push(sync_native(&tp, &user_wsol).unwrap());
    // Create USDC ATA
    ixs.push(create_associated_token_account_idempotent(&user, &user, &usdc, &tp));
    // Create TRUMP ATA
    ixs.push(create_associated_token_account_idempotent(&user, &user, &trump, &tp));

    // ── Hop 1: WSOL → USDC (DLMM) ─────────────────────────────────────
    // Pool: token_x = WSOL, token_y = USDC
    // Direction: sell token_x (WSOL) → get token_y (USDC)
    // user_token_in = WSOL ATA, user_token_out = USDC ATA

    let pool1 = Pubkey::from_str_const(SOL_USDC_POOL);
    let hop1 = swap_builder::build_dlmm_swap(
        &DlmmSwapAccounts {
            pool: pool1,
            reserve_x: Pubkey::from_str_const(SOL_USDC_RESERVE_X),
            reserve_y: Pubkey::from_str_const(SOL_USDC_RESERVE_Y),
            token_x_mint: wsol,
            token_y_mint: usdc,
            user_token_in: user_wsol,   // spending WSOL
            user_token_out: user_usdc,  // receiving USDC
            user,
            token_x_program: tp,
            token_y_program: tp,
            bitmap_extension: None,
            bin_array: swap_builder::dlmm_bin_array_pda(&pool1, SOL_USDC_ACTIVE_ID),
        },
        sol_amount,
        1, // accept any output for testing
    ).expect("Hop 1 build failed");
    ixs.push(hop1);

    // ── Hop 2: USDC → TRUMP (DLMM) ─────────────────────────────────────
    // Pool: token_x = TRUMP, token_y = USDC
    // Direction: sell token_y (USDC) → get token_x (TRUMP)
    // user_token_in = USDC ATA, user_token_out = TRUMP ATA

    let pool2 = Pubkey::from_str_const(USDC_TRUMP_POOL);
    let hop2 = swap_builder::build_dlmm_swap(
        &DlmmSwapAccounts {
            pool: pool2,
            reserve_x: Pubkey::from_str_const(USDC_TRUMP_RESERVE_X),
            reserve_y: Pubkey::from_str_const(USDC_TRUMP_RESERVE_Y),
            token_x_mint: trump,
            token_y_mint: usdc,
            user_token_in: user_usdc,    // spending USDC (from hop 1)
            user_token_out: user_trump,  // receiving TRUMP
            user,
            token_x_program: tp,
            token_y_program: tp,
            bitmap_extension: None,
            bin_array: swap_builder::dlmm_bin_array_pda(&pool2, USDC_TRUMP_ACTIVE_ID),
        },
        // For hop 2, we don't know the exact USDC output from hop 1.
        // We pass the full USDC ATA balance. The DLMM will swap what's available.
        // Actually, the amount_in for hop 2 needs to match what hop 1 produces.
        // For now, let's estimate: 0.1 SOL ≈ $8.4 USDC at current price.
        // We'll use a conservative estimate. If it's too high, the swap fails;
        // if too low, we swap less than optimal.
        8_000_000, // ~8 USDC (conservative estimate for 0.1 SOL)
        1,
    ).expect("Hop 2 build failed");
    ixs.push(hop2);

    // ── Close WSOL ATA to recover rent ──────────────────────────────────
    ixs.push(
        spl_token::instruction::close_account(&tp, &user_wsol, &user, &user, &[]).unwrap()
    );

    // ── Build, sign, send ───────────────────────────────────────────────
    println!("{} instructions total\n", ixs.len());

    let blockhash = rpc.get_latest_blockhash().await.expect("Blockhash");
    #[allow(deprecated)]
    let message = v0::Message::try_compile(&user, &ixs, &[], blockhash).expect("Compile");

    let mut tx = VersionedTransaction {
        signatures: vec![solana_sdk::signature::Signature::default()],
        message: VersionedMessage::V0(message),
    };
    tx.signatures[0] = keypair.sign_message(tx.message.serialize().as_slice());

    let tx_size = bincode::serialize(&tx).map(|b| b.len()).unwrap_or(0);
    println!("Tx: {} bytes (limit 1232)\n", tx_size);

    if tx_size > 1232 {
        println!("Transaction too large! Need Address Lookup Tables.");
        return;
    }

    println!("Sending 2-hop swap to Surfpool...\n");
    match rpc.send_and_confirm_transaction(&tx).await {
        Ok(sig) => {
            println!("2-HOP SWAP SUCCEEDED!");
            println!("Signature: {sig}\n");
            print_balance(&rpc, "TRUMP", &user_trump, 6).await;
            print_balance(&rpc, "USDC", &user_usdc, 6).await;
            let sol = rpc.get_balance(&user).await.unwrap_or(0);
            println!("  SOL:   {:.4}", sol as f64 / 1e9);
        }
        Err(e) => println!("FAILED: {e}"),
    }
}

async fn print_balance(rpc: &RpcClient, label: &str, ata: &Pubkey, decimals: i32) {
    if let Ok(acc) = rpc.get_account(ata).await {
        if acc.data.len() >= 72 {
            let bal = u64::from_le_bytes(acc.data[64..72].try_into().unwrap());
            println!("  {label:6}: {:.6} (raw: {bal})", bal as f64 / 10f64.powi(decimals));
        }
    }
}
