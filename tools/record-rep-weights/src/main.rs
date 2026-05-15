use std::path::PathBuf;
use std::{fmt::Write, str::FromStr};

use clap::Parser;
use rsnano_rpc_client::{NanoRpcClient, Url};
use rsnano_rpc_messages::{RepresentativesArgs, RepresentativesResponse};
use rsnano_types::currency_constants::CURRENCY_NAME;
use rsnano_types::{Amount, NetworkType};

const CUTOFF: u64 = 250_000;
const PERCENTAGE_LIMIT: u128 = 99;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Network name. Eg 'live' or 'beta'
    #[arg(short, long, default_value = "live")]
    network: String,

    /// node rpc. Eg 'http://[::1]:7076'
    #[arg(short, long)]
    rpc: Option<String>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let network = NetworkType::from_str(&args.network).unwrap();
    let rpc_addr = args
        .rpc
        .unwrap_or_else(|| rpc_addr_for(network).to_string());
    let (confirmed, representatives) = get_raw_data(rpc_addr.parse()?).await?;

    let rep_file = create_rep_file(&representatives, confirmed);

    write_file(network, rep_file)?;
    Ok(())
}

fn rpc_addr_for(network: NetworkType) -> &'static str {
    match network {
        NetworkType::NanoBetaNetwork => "http://[::1]:55000",
        _ => "http://[::1]:7076",
    }
}

async fn get_raw_data(rpc_url: Url) -> anyhow::Result<(u64, RepresentativesResponse)> {
    let client = NanoRpcClient::new(rpc_url);
    let representatives = client
        .representatives_with(RepresentativesArgs {
            count: None,
            sorting: Some(true.into()),
        })
        .await?;
    let confirmed = client.block_count().await?.cemented;
    Ok((confirmed.into(), representatives))
}

fn create_rep_file(reps: &RepresentativesResponse, confirmed: u64) -> RepsFile {
    let block_height = confirmed - CUTOFF;
    let supply_max = get_supply_max(reps, PERCENTAGE_LIMIT);

    let mut content = String::new();
    writeln!(&mut content, "{}", block_height).unwrap();
    let mut total = Amount::ZERO;
    let mut rep_count = 0;
    for (account, amount) in &reps.representatives {
        writeln!(
            &mut content,
            "{}:{}",
            account.encode_account(),
            amount.number()
        )
        .unwrap();
        total += *amount;
        rep_count += 1;
        if total >= supply_max {
            break;
        }
    }
    RepsFile {
        content,
        rep_count,
        supply_max,
    }
}

fn write_file(network: NetworkType, rep_file: RepsFile) -> std::io::Result<()> {
    let output_path = output_path(network)?;
    std::fs::write(output_path.clone(), rep_file.content)?;
    println!("wrote {} rep weights", rep_file.rep_count);
    println!(
        "max supply {} {}",
        rep_file.supply_max.format_balance(0),
        CURRENCY_NAME
    );
    println!("weight file generated: {:?}", output_path);
    Ok(())
}

struct RepsFile {
    content: String,
    rep_count: u64,
    supply_max: Amount,
}

fn get_supply_max(reps: &RepresentativesResponse, percentage_limit: u128) -> Amount {
    let supply_max: Amount = reps.representatives.iter().map(|(_, amount)| *amount).sum();
    (supply_max / 100) * percentage_limit
}

fn output_path(network: NetworkType) -> std::io::Result<PathBuf> {
    let mut path = std::env::current_exe()?;
    path.pop();
    path.pop();
    path.pop();
    path.push("node");
    path.push("rep_weights");
    #[cfg(feature = "banano")]
    {
        path.push("Banano")
    }
    #[cfg(not(feature = "banano"))]
    {
        path.push("Nano")
    }

    path.push(format!("{}.txt", network.as_str()));
    Ok(path)
}
