use rai::{
    run_timed_six_node_simulation, timed_simulation_help, AccountKeyStore, Block, DemoKeyStore,
    ElectionId, GenesisAccount, GlobalResult, RaiEngine, Result, Send, SignedBlock, SignedVote,
    Slot, TimedSimulationConfig, VoteKind, VoteValue,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "demo".into());
    let remaining = args.collect::<Vec<_>>();
    match command.as_str() {
        "demo" | "fast" => fast_demo(),
        "slow" | "conflict" | "six-nodes" | "six" | "first-arrival" | "arrival"
        | "timed-six-nodes" | "timed" | "simulation" => {
            if remaining.iter().any(|arg| arg == "--help" || arg == "-h") {
                println!("{}", timed_simulation_help());
                return Ok(());
            }
            run_timed_six_node_simulation(TimedSimulationConfig::from_args(&remaining)?)?;
            Ok(())
        }
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => {
            eprintln!("unknown command: {other}\n");
            print_help();
            Ok(())
        }
    }
}

fn print_help() {
    println!("RAI Rust proof of concept");
    println!();
    println!("USAGE:");
    println!("  cargo run -- demo             # joint fast path");
    println!("  cargo run -- timed-six-nodes  # adversarial six-replica simulation");
    println!("  cargo test");
}

fn fast_demo() -> Result<()> {
    let replicas = 1..=6;
    let crypto = DemoKeyStore::deterministic(replicas.clone());
    let account_keys = AccountKeyStore::deterministic(replicas.clone());
    let genesis = replicas
        .clone()
        .map(|account| {
            GenesisAccount::new(
                account,
                1_000,
                account,
                account_keys.public_key(account).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let mut engine = RaiEngine::with_genesis(crypto, 7, genesis, 1_000, 1_000)?;
    let slot = Slot::new(1, 1);
    let election = ElectionId::Slot { slot, epoch: 0 };
    engine.register_derived_election(election.clone())?;
    let block = engine.submit_block(SignedBlock::sign(
        &account_keys,
        Block {
            slot,
            parent: GenesisAccount::deterministic(1, 1_000, 1).hash(),
            balance: 995,
            representative: 1,
            sends: vec![Send {
                destination: 2,
                amount: 5,
            }],
            receives: Vec::new(),
        },
    )?)?;
    for signer in 1..=5 {
        engine.submit_vote(SignedVote::new(
            &engine.crypto,
            signer,
            election.clone(),
            7,
            VoteValue::Candidate(block),
            VoteKind::First,
        )?)?;
    }
    assert_eq!(
        engine.derive_result(&election)?,
        Some(GlobalResult::Fast(block))
    );
    engine.complete_block(&election, block)?;
    println!("fast-finalized send in slot {slot} as {}", block.short());
    Ok(())
}
