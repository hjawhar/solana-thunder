//! Dynamic multi-hop swap on Surfpool.
//!
//! Run:
//!   INPUT=SOL OUTPUT=6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN \
//!   AMOUNT=0.1 MAX_HOPS=2 \
//!   cargo test --release --test surfpool_swap -- --nocapture

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::str::FromStr;

use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_rpc_client_api::filter::{Memcmp, RpcFilterType};
use solana_account_decoder_client_types::UiAccountEncoding;
use solana_sdk::{
    instruction::Instruction,
    message::{v0, VersionedMessage},
    signature::{Keypair, Signer},
    transaction::VersionedTransaction,
};
use solana_system_interface::instruction as system_ix;
use spl_associated_token_account::get_associated_token_address_with_program_id;
use spl_associated_token_account::instruction::create_associated_token_account_idempotent;
use spl_token::instruction::sync_native;
use thunder_aggregator::{
    cache, loader, pool_index::PoolIndex, router::Router,
    swap_builder::{self, DlmmSwapAccounts, ClmmSwapAccounts},
    types::Route,
};
use thunder_core::{infer_mint_decimals, Market, WSOL, TOKEN_PROGRAM, TOKEN_PROGRAM_2022};

const DLMM_PROGRAM: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";
const CLMM_PROGRAM: &str = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";

#[tokio::test]
async fn test_dynamic_swap() {
    dotenvy::dotenv().ok();

    let input_str = env::var("INPUT").unwrap_or_else(|_| "SOL".into());
    let output_str = env::var("OUTPUT").unwrap_or_else(|_| "6p6xgHyF7AeE6TZkSmFsko444wqoP15icUSqi2jfGiPN".into());
    let amount_str = env::var("AMOUNT").unwrap_or_else(|_| "0.1".into());
    let max_hops: usize = env::var("MAX_HOPS").ok().and_then(|s| s.parse().ok()).unwrap_or(2);
    let slippage_bps: u64 = env::var("SLIPPAGE").ok().and_then(|s| s.parse().ok()).unwrap_or(500);

    let input_mint = parse_mint(&input_str);
    let output_mint = parse_mint(&output_str);
    let input_decimals = infer_mint_decimals(&input_mint);
    let amount_in = (amount_str.parse::<f64>().expect("Invalid AMOUNT")
        * 10f64.powi(input_decimals as i32)) as u64;

    let keypair = Keypair::from_base58_string(&env::var("PRIVATE_KEY").expect("PRIVATE_KEY"));
    let user = keypair.pubkey();
    let surfpool_url = env::var("SURFPOOL_URL").unwrap_or_else(|_| "http://127.0.0.1:8899".into());
    let rpc_url = env::var("RPC_URL").unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".into());

    let rpc = RpcClient::new_with_commitment(surfpool_url, CommitmentConfig::confirmed());
    let mainnet = RpcClient::new_with_commitment(rpc_url.clone(), CommitmentConfig::confirmed());

    println!("Thunder - Dynamic Surfpool Swap");
    println!("===============================\n");
    println!("Wallet:  {user}");
    println!("Input:   {input_str}");
    println!("Output:  {}", &output_mint.to_string()[..12]);
    println!("Amount:  {amount_str} ({amount_in} raw)");
    println!("Hops:    {max_hops}\n");

    let sol_before = rpc.get_balance(&user).await.unwrap_or(0);
    println!("SOL balance: {:.4}\n", sol_before as f64 / 1e9);

    // Check if we already have the output token
    let out_prog_check = if let Ok(acc) = mainnet.get_account(&output_mint).await {
        acc.owner
    } else {
        Pubkey::from_str_const(TOKEN_PROGRAM)
    };
    let out_ata_check = get_associated_token_address_with_program_id(&user, &output_mint, &out_prog_check);
    let output_before = rpc.get_account(&out_ata_check).await.ok()
        .filter(|a| a.data.len() >= 72)
        .map(|a| u64::from_le_bytes(a.data[64..72].try_into().unwrap()))
        .unwrap_or(0);

    // ── Load pools ──────────────────────────────────────────────────────
    let cache_path = PathBuf::from(env::var("CACHE_PATH").unwrap_or_else(|_| "pools.cache".into()));
    let index = match cache::load_cache(&cache_path) {
        Ok((idx, _)) => { println!("Loaded {} pools from cache", idx.pool_count()); idx }
        Err(_) => {
            println!("Loading from RPC...");
            let cb: loader::ProgressCallback = Box::new(|_| {});
            let idx = loader::PoolLoader::new(&rpc_url).load_all(&cb).await.expect("Load failed");
            let _ = cache::save_cache(&idx, &cache_path);
            println!("Loaded {} pools", idx.pool_count()); idx
        }
    };

    // ── Pre-fetch all DLMM bitmap extensions (53 on-chain) ──────────────
    let bitmap_map = fetch_all_bitmap_extensions(&mainnet).await;
    println!("Bitmap extensions: {}\n", bitmap_map.len());
    // Debug: check if specific pool is in the index
    let test_pool = "3C5YE97HADPDxZehYq9Cis8AXr9aNyrUsczKzE1nDbW9";
    if let Some(entry) = index.get_pool(test_pool) {
        let meta = entry.market.metadata().unwrap();
        println!("DEBUG pool {}: quote={} base={}", &test_pool[..8], &meta.quote_mint.to_string()[..8], &meta.base_mint.to_string()[..8]);
        let direct = index.direct_pools(&meta.quote_mint, &meta.base_mint);
        println!("DEBUG direct pools for this pair: {}", direct.len());
    } else {
        println!("DEBUG pool {} NOT in index", &test_pool[..8]);
    }
    let wsol_pk = Pubkey::from_str_const(WSOL);
    let usdc_pk = Pubkey::from_str_const("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
    println!("DEBUG SOL<->USDC pools: {}", index.direct_pools(&wsol_pk, &usdc_pk).len());
    println!("DEBUG USDC<->TRUMP pools: {}", index.direct_pools(&usdc_pk, &output_mint).len());

    // ── Find route ──────────────────────────────────────────────────────
    println!("Finding route (max {max_hops} hops)...\n");
    let router = Router::new(&index, max_hops);
    let quote = router.find_routes(input_mint, output_mint, amount_in, 200).expect("Route failed");

    if quote.routes.is_empty() {
        println!("No routes found.");
        return;
    }

    for (i, route) in quote.routes.iter().take(5).enumerate() {
        print_route(i + 1, route, if i == 0 { " <- best" } else { "" });
    }
    if quote.routes.len() > 5 {
        println!("  ... and {} more routes\n", quote.routes.len() - 5);
    }

    // Pre-fetch CLMM tick arrays in parallel for all unique CLMM pools.
    let mut clmm_pools: Vec<(Pubkey, i32, u16)> = Vec::new();
    for route in &quote.routes {
        for hop in &route.hops {
            if hop.dex_name == "Raydium CLMM" {
                let pk = Pubkey::from_str(&hop.pool_address).unwrap();
                if !clmm_pools.iter().any(|(p, _, _)| *p == pk) {
                    if let Ok(acc) = rpc.get_account(&pk).await {
                        if acc.data.len() >= 273 {
                            let ts = u16::from_le_bytes(acc.data[235..237].try_into().unwrap());
                            let tc = i32::from_le_bytes(acc.data[269..273].try_into().unwrap());
                            clmm_pools.push((pk, tc, ts));
                        }
                    }
                }
            }
        }
    }
    println!("Fetching tick arrays for {} CLMM pools (parallel)...", clmm_pools.len());
    let tick_futures: Vec<_> = clmm_pools.iter()
        .map(|(pk, tc, ts)| fetch_clmm_tick_arrays(&mainnet, pk, *tc, *ts))
        .collect();
    let tick_results = futures::future::join_all(tick_futures).await;
    let mut tick_array_map: HashMap<Pubkey, Vec<Pubkey>> = HashMap::new();
    for ((pk, _, _), tas) in clmm_pools.iter().zip(tick_results) {
        if !tas.is_empty() {
            tick_array_map.insert(*pk, tas);
        }
    }
    println!("Found tick arrays for {}/{} pools", tick_array_map.len(), clmm_pools.len());

    // Pre-filter routes: skip routes with hops we know will fail.
    let viable_routes: Vec<&Route> = quote.routes.iter().filter(|route| {
        route.hops.iter().all(|hop| {
            match hop.dex_name.as_str() {
                "Raydium CLMM" => {
                    // Skip if we couldn't find tick arrays
                    let pk = Pubkey::from_str(&hop.pool_address).unwrap();
                    tick_array_map.contains_key(&pk)
                }
                "Meteora DLMM" => true, // Can't pre-filter bitmap ext reliably
                _ => true,
            }
        })
    }).collect();
    println!("{} viable routes (filtered from {})\n", viable_routes.len(), quote.routes.len());

    // ── Try each viable route ─────────────────────────────────────────────────────
    let wsol = Pubkey::from_str_const(WSOL);
    let tp = Pubkey::from_str_const(TOKEN_PROGRAM);

    for (ri, route) in viable_routes.iter().enumerate() {
        let hops_desc: Vec<String> = route.hops.iter().map(|h| format!("{}({})", &h.pool_address[..6], h.dex_name.chars().take(4).collect::<String>())).collect();
        println!("--- Route {} [{}] ---", ri + 1, hops_desc.join(" -> "));

        // Detect token programs
        let mut all_mints: Vec<Pubkey> = Vec::new();
        for hop in &route.hops {
            if !all_mints.contains(&hop.input_mint) { all_mints.push(hop.input_mint); }
            if !all_mints.contains(&hop.output_mint) { all_mints.push(hop.output_mint); }
        }
        let mint_programs = detect_mint_programs(&rpc, &all_mints).await;

        // Pre-instructions: ATAs + WSOL wrapping
        let mut ixs: Vec<Instruction> = Vec::new();
        let input_is_sol = input_mint == wsol;
        let input_prog = *mint_programs.get(&input_mint).unwrap_or(&tp);
        let user_input_ata = get_associated_token_address_with_program_id(&user, &input_mint, &input_prog);

        if input_is_sol {
            ixs.push(create_associated_token_account_idempotent(&user, &user, &wsol, &tp));
            ixs.push(system_ix::transfer(&user, &user_input_ata, amount_in));
            ixs.push(sync_native(&tp, &user_input_ata).unwrap());
        }

        for hop in &route.hops {
            let prog = *mint_programs.get(&hop.output_mint).unwrap_or(&tp);
            ixs.push(create_associated_token_account_idempotent(&user, &user, &hop.output_mint, &prog));
        }

        // Build swap instructions per hop
        let mut build_ok = true;
        for (hi, hop) in route.hops.iter().enumerate() {
            let in_prog = *mint_programs.get(&hop.input_mint).unwrap_or(&tp);
            let out_prog = *mint_programs.get(&hop.output_mint).unwrap_or(&tp);
            let user_in = get_associated_token_address_with_program_id(&user, &hop.input_mint, &in_prog);
            let user_out = get_associated_token_address_with_program_id(&user, &hop.output_mint, &out_prog);

            let hop_amount = if hi == 0 { amount_in } else { hop.input_amount };
            let hop_min_out = if hi == route.hops.len() - 1 {
                thunder_core::calculate_min_amount_out(hop.output_amount, slippage_bps)
            } else { 1 };

            match build_hop(&rpc, &mainnet, &bitmap_map, &tick_array_map, hop, &user, user_in, user_out, in_prog, out_prog, hop_amount, hop_min_out).await {
                Ok(ix) => ixs.push(ix),
                Err(e) => {
                    println!("  Hop {} failed: {e}", hi + 1);
                    build_ok = false;
                    break;
                }
            }
        }
        if !build_ok { continue; }

        if input_is_sol {
            ixs.push(spl_token::instruction::close_account(&tp, &user_input_ata, &user, &user, &[]).unwrap());
        }

        // Build, sign, send
        let blockhash = rpc.get_latest_blockhash().await.expect("Blockhash");
        #[allow(deprecated)]
        let message = match v0::Message::try_compile(&user, &ixs, &[], blockhash) {
            Ok(m) => m,
            Err(e) => { println!("  Compile: {e}"); continue; }
        };

        let mut tx = VersionedTransaction {
            signatures: vec![solana_sdk::signature::Signature::default()],
            message: VersionedMessage::V0(message),
        };
        tx.signatures[0] = keypair.sign_message(tx.message.serialize().as_slice());

        let tx_size = bincode::serialize(&tx).map(|b| b.len()).unwrap_or(0);
        println!("  {} ixs, {} bytes", ixs.len(), tx_size);
        if tx_size > 1232 { println!("  Too large"); continue; }

        match rpc.send_and_confirm_transaction(&tx).await {
            Ok(sig) => {
                println!("\n  SWAP SUCCEEDED!");
                println!("  Signature: {sig}\n");

                // Show before/after diff
                let sol_after = rpc.get_balance(&user).await.unwrap_or(0);
                let out_prog = *mint_programs.get(&output_mint).unwrap_or(&tp);
                let out_ata = get_associated_token_address_with_program_id(&user, &output_mint, &out_prog);
                let output_after = rpc.get_account(&out_ata).await.ok()
                    .filter(|a| a.data.len() >= 72)
                    .map(|a| u64::from_le_bytes(a.data[64..72].try_into().unwrap()))
                    .unwrap_or(0);

                let out_dec = infer_mint_decimals(&output_mint);
                let out_diff = output_after.saturating_sub(output_before);
                let sol_diff = sol_before.saturating_sub(sol_after);

                println!("  ┌─────────────────────────────────────────┐");
                println!("  │  SOL   : -{:.6} ({:.4} -> {:.4})", sol_diff as f64 / 1e9, sol_before as f64 / 1e9, sol_after as f64 / 1e9);
                println!("  │  Token : +{:.6} ({:.6} -> {:.6})",
                    out_diff as f64 / 10f64.powi(out_dec as i32),
                    output_before as f64 / 10f64.powi(out_dec as i32),
                    output_after as f64 / 10f64.powi(out_dec as i32),
                );
                println!("  └─────────────────────────────────────────┘");
                return;
            }
            Err(e) => {
                let msg = e.to_string();
                // Show the error code
                if let Some(pos) = msg.find("custom program error") {
                    let end = (pos + 50).min(msg.len());
                    println!("  Failed: {}", &msg[pos..end]);
                } else if let Some(pos) = msg.find("Error processing") {
                    let end = (pos + 80).min(msg.len());
                    println!("  Failed: {}", &msg[pos..end]);
                } else {
                    println!("  Failed: {}", &msg[..msg.len().min(120)]);
                }
            }
        }
    }
    println!("\nAll {} viable routes exhausted ({} total found).", viable_routes.len(), quote.routes.len());
}

// =========================================================================
// Build a single hop's swap instruction
// =========================================================================

async fn build_hop(
    rpc: &RpcClient,
    _mainnet: &RpcClient,
    bitmap_map: &HashMap<Pubkey, Pubkey>,
    tick_array_map: &HashMap<Pubkey, Vec<Pubkey>>,
    hop: &thunder_aggregator::types::RouteHop,
    user: &Pubkey,
    user_token_in: Pubkey,
    user_token_out: Pubkey,
    in_program: Pubkey,
    out_program: Pubkey,
    amount_in: u64,
    min_amount_out: u64,
) -> Result<Instruction, Box<dyn std::error::Error + Send + Sync>> {
    let pool_pubkey = Pubkey::from_str(&hop.pool_address)?;

    match hop.dex_name.as_str() {
        "Meteora DLMM" => {
            let account = rpc.get_account(&pool_pubkey).await?;
            let data = &account.data;
            if data.len() < 216 { return Err("DLMM pool too short".into()); }

            let active_id = i32::from_le_bytes(data[76..80].try_into().unwrap());
            let reserve_x = Pubkey::try_from(&data[152..184])?;
            let reserve_y = Pubkey::try_from(&data[184..216])?;
            let token_x_mint = Pubkey::try_from(&data[88..120])?;
            let token_y_mint = Pubkey::try_from(&data[120..152])?;

            let x_prog = if token_x_mint == hop.input_mint { in_program } else { out_program };
            let y_prog = if token_y_mint == hop.input_mint { in_program } else { out_program };

            let bin_array = swap_builder::dlmm_bin_array_pda(&pool_pubkey, active_id);
            let bitmap_ext = bitmap_map.get(&pool_pubkey).copied();

            swap_builder::build_dlmm_swap(
                &DlmmSwapAccounts {
                    pool: pool_pubkey, reserve_x, reserve_y,
                    token_x_mint, token_y_mint,
                    user_token_in, user_token_out, user: *user,
                    token_x_program: x_prog, token_y_program: y_prog,
                    bitmap_extension: bitmap_ext, bin_array,
                },
                amount_in, min_amount_out,
            )
        }

        "Raydium CLMM" => {
            let account = rpc.get_account(&pool_pubkey).await?;
            let data = &account.data;
            if data.len() < 273 { return Err("CLMM pool too short".into()); }

            let amm_config = Pubkey::try_from(&data[9..41])?;
            let token_mint_0 = Pubkey::try_from(&data[73..105])?;
            let token_mint_1 = Pubkey::try_from(&data[105..137])?;
            let token_vault_0 = Pubkey::try_from(&data[137..169])?;
            let token_vault_1 = Pubkey::try_from(&data[169..201])?;
            let observation_key = Pubkey::try_from(&data[201..233])?;
            let tick_spacing = u16::from_le_bytes(data[235..237].try_into().unwrap());
            let tick_current = i32::from_le_bytes(data[269..273].try_into().unwrap());

            let (input_vault, output_vault, input_mint, output_mint, in_p, out_p) =
                if hop.input_mint == token_mint_0 {
                    (token_vault_0, token_vault_1, token_mint_0, token_mint_1, in_program, out_program)
                } else {
                    (token_vault_1, token_vault_0, token_mint_1, token_mint_0, in_program, out_program)
                };

            // Use pre-fetched tick arrays.
            let tick_arrays = tick_array_map.get(&pool_pubkey).cloned().unwrap_or_default();
            if tick_arrays.is_empty() {
                return Err("No initialized tick arrays found".into());
            }

            swap_builder::build_clmm_swap(
                &ClmmSwapAccounts {
                    pool: pool_pubkey, amm_config,
                    input_vault, output_vault,
                    observation: observation_key,
                    input_mint, output_mint,
                    user_input_token: user_token_in,
                    user_output_token: user_token_out,
                    user: *user,
                    input_token_program: in_p,
                    output_token_program: out_p,
                    tick_arrays,
                },
                amount_in, min_amount_out, 0u128,
            )
        }

        other => Err(format!("DEX '{}' not supported", other).into()),
    }
}

// =========================================================================
// Fetch all 53 DLMM bitmap extension accounts into a pool→address map
// =========================================================================

async fn fetch_all_bitmap_extensions(rpc: &RpcClient) -> HashMap<Pubkey, Pubkey> {
    let dlmm = Pubkey::from_str_const(DLMM_PROGRAM);
    let mut map = HashMap::new();

    #[allow(deprecated)]
    let config = RpcProgramAccountsConfig {
        filters: Some(vec![RpcFilterType::DataSize(12488)]),
        account_config: RpcAccountInfoConfig {
            encoding: Some(UiAccountEncoding::Base64),
            data_slice: Some(solana_account_decoder_client_types::UiDataSliceConfig { offset: 8, length: 32 }),
            ..Default::default()
        },
        ..Default::default()
    };

    #[allow(deprecated)]
    if let Ok(accounts) = rpc.get_program_accounts_with_config(&dlmm, config).await {
        for (ext_pubkey, account) in accounts {
            if account.data.len() >= 32 {
                if let Ok(pool_pubkey) = Pubkey::try_from(&account.data[..32]) {
                    map.insert(pool_pubkey, ext_pubkey);
                }
            }
        }
    }

    map
}

async fn detect_mint_programs(rpc: &RpcClient, mints: &[Pubkey]) -> HashMap<Pubkey, Pubkey> {
    let default = Pubkey::from_str_const(TOKEN_PROGRAM);
    let mut map = HashMap::new();
    if let Ok(accounts) = rpc.get_multiple_accounts(mints).await {
        for (mint, maybe) in mints.iter().zip(accounts) {
            map.insert(*mint, maybe.map(|a| a.owner).unwrap_or(default));
        }
    }
    map
}

/// Fetch initialized tick arrays for a CLMM pool from mainnet.
/// Returns the 3 tick arrays closest to the current tick (sorted by proximity).
async fn fetch_clmm_tick_arrays(
    rpc: &RpcClient,
    pool: &Pubkey,
    tick_current: i32,
    tick_spacing: u16,
) -> Vec<Pubkey> {
    let clmm = Pubkey::from_str_const(CLMM_PROGRAM);

    // Search for tick array accounts that reference this pool.
    // Tick array layout: 8-byte disc + pool_id(32) + start_index(i32) + ...
    #[allow(deprecated)]
    let config = RpcProgramAccountsConfig {
        filters: Some(vec![
            RpcFilterType::Memcmp(Memcmp::new_raw_bytes(8, pool.to_bytes().to_vec())),
        ]),
        account_config: RpcAccountInfoConfig {
            encoding: Some(UiAccountEncoding::Base64),
            data_slice: Some(solana_account_decoder_client_types::UiDataSliceConfig {
                offset: 8,
                length: 36, // pool_id(32) + start_index(i32)
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    #[allow(deprecated)]
    let accounts = match rpc.get_program_accounts_with_config(&clmm, config).await {
        Ok(a) => a,
        Err(_) => return vec![],
    };

    // Parse start indices and sort by proximity to current tick.
    let tick_per_array = tick_spacing as i32 * 60;
    let mut tick_array_info: Vec<(Pubkey, i32)> = accounts.iter().filter_map(|(pubkey, acc)| {
        if acc.data.len() >= 36 {
            let start = i32::from_le_bytes(acc.data[32..36].try_into().ok()?);
            Some((*pubkey, start))
        } else {
            None
        }
    }).collect();

    // Sort by distance from current tick's array start.
    let current_start = tick_current.div_euclid(tick_per_array) * tick_per_array;
    tick_array_info.sort_by_key(|(_, start)| ((*start - current_start) as i64).abs());

    // Return up to 3 closest tick arrays.
    tick_array_info.into_iter().take(3).map(|(pk, _)| pk).collect()
}

fn parse_mint(s: &str) -> Pubkey {
    if s.eq_ignore_ascii_case("SOL") || s.eq_ignore_ascii_case("WSOL") {
        Pubkey::from_str_const(WSOL)
    } else {
        Pubkey::from_str(s).unwrap_or_else(|_| panic!("Invalid mint: {s}"))
    }
}

fn print_route(num: usize, route: &Route, tag: &str) {
    let n = route.hops.len();
    println!("  Route {num} ({n} hop{}):{tag}", if n == 1 { "" } else { "s" });
    for (j, hop) in route.hops.iter().enumerate() {
        let id = infer_mint_decimals(&hop.input_mint);
        let od = infer_mint_decimals(&hop.output_mint);
        println!(
            "    {}: {:.6} {} -> {:.6} {} via {}..{} ({})",
            j + 1,
            hop.input_amount as f64 / 10f64.powi(id as i32),
            &hop.input_mint.to_string()[..6],
            hop.output_amount as f64 / 10f64.powi(od as i32),
            &hop.output_mint.to_string()[..6],
            &hop.pool_address[..6], &hop.pool_address[hop.pool_address.len()-4..],
            hop.dex_name,
        );
    }
    let od = infer_mint_decimals(&route.output_mint);
    println!("    -> {:.6}\n", route.output_amount as f64 / 10f64.powi(od as i32));
}

async fn print_balance(rpc: &RpcClient, label: &str, ata: &Pubkey, decimals: i32) {
    if let Ok(acc) = rpc.get_account(ata).await {
        if acc.data.len() >= 72 {
            let bal = u64::from_le_bytes(acc.data[64..72].try_into().unwrap());
            println!("  {label}: {:.6} (raw: {bal})", bal as f64 / 10f64.powi(decimals));
        }
    }
}
