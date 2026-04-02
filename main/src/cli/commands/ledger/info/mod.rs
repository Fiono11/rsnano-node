use clap::{CommandFactory, Parser, Subcommand};

use rsnano_store_lmdb::{default_ledger_lmdb_options, LmdbPeerStore, LmdbRepWeightStore};

use crate::cli::GlobalArgs;
use rsnano_nullable_lmdb::LmdbEnvironmentFactory;
use std::collections::BTreeMap;

#[derive(Subcommand, PartialEq, Debug)]
pub(crate) enum InfoSubcommands {
    /// Displays peer IPv6:port connections
    Peers,
    /// Print representative weights in descending order
    RepWeights,
}

#[derive(Parser, PartialEq, Debug)]
pub(crate) struct InfoCommand {
    #[command(subcommand)]
    pub subcommand: Option<InfoSubcommands>,
}

impl InfoCommand {
    pub(crate) fn run(&self, global_args: GlobalArgs) -> anyhow::Result<()> {
        match &self.subcommand {
            Some(InfoSubcommands::Peers) => self.peers(global_args)?,
            Some(InfoSubcommands::RepWeights) => self.print_rep_weights(global_args)?,
            None => InfoCommand::command().print_long_help()?,
        }

        Ok(())
    }

    fn peers(&self, global_args: GlobalArgs) -> anyhow::Result<()> {
        let path = global_args.data_path.join("data.ldb");
        let options = default_ledger_lmdb_options(path);
        let env = LmdbEnvironmentFactory::default().create(options)?;
        let peer_store = LmdbPeerStore::new(&env)?;
        let txn = env.begin_read();

        for peer in peer_store.iter(&txn) {
            println!("{:?}", peer.0);
        }

        txn.commit();
        Ok(())
    }

    fn print_rep_weights(&self, global_args: GlobalArgs) -> anyhow::Result<()> {
        let path = global_args.data_path.join("data.ldb");
        let options = default_ledger_lmdb_options(path);
        let env = LmdbEnvironmentFactory::default().create(options)?;
        let rep_weight_store = LmdbRepWeightStore::new(&env)?;
        let txn = env.begin_read();

        let mut top_reps = BTreeMap::new();
        for (rep_key, weight) in rep_weight_store.iter(&txn) {
            top_reps.insert(weight, rep_key.as_account());
            if top_reps.len() > 100 {
                top_reps.pop_first();
            }
        }

        txn.commit();

        println!("{:>15} | {}", "WEIGHT", "REPRESENTATIVE");
        for (amount, rep) in top_reps.iter().rev() {
            println!(
                "{:>15} | {}",
                amount.format_balance(0),
                rep.encode_account()
            );
        }
        Ok(())
    }
}
