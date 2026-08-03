use std::{fs, path::Path};

use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};

use crate::cli_args::CliArgs;

const MANIFEST_NAME: &str = "nanospam-setup.json";
const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct PreparedNetwork {
    version: u32,
    prs: usize,
    accounts: usize,
    cpp: bool,
    rocksdb: bool,
    rai_epoch_duration_ms: Option<u64>,
    rai_tick_interval_ms: Option<u64>,
}

impl PreparedNetwork {
    fn from_args(args: &CliArgs) -> Self {
        Self {
            version: MANIFEST_VERSION,
            prs: args.prs,
            accounts: args.accounts,
            cpp: args.cpp,
            rocksdb: args.rocksdb,
            rai_epoch_duration_ms: args.rai_epoch_duration_ms,
            rai_tick_interval_ms: args.rai_tick_interval_ms,
        }
    }
}

pub(crate) fn write_prepared_network(data_dir: &Path, args: &CliArgs) -> anyhow::Result<()> {
    let path = data_dir.join(MANIFEST_NAME);
    let manifest = serde_json::to_vec_pretty(&PreparedNetwork::from_args(args))?;
    fs::write(&path, manifest)
        .with_context(|| format!("could not write prepared-network manifest {path:?}"))?;
    Ok(())
}

pub(crate) fn validate_prepared_network(data_dir: &Path, args: &CliArgs) -> anyhow::Result<()> {
    let path = data_dir.join(MANIFEST_NAME);
    let bytes = fs::read(&path).with_context(|| {
        format!("prepared network not found at {path:?}; run `nanospam setup` first")
    })?;
    let prepared: PreparedNetwork = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid prepared-network manifest {path:?}"))?;
    ensure!(
        prepared.version == MANIFEST_VERSION,
        "unsupported setup manifest version"
    );
    let requested = PreparedNetwork::from_args(args);
    ensure!(
        prepared == requested,
        "run configuration does not match setup manifest: prepared={prepared:?}, requested={requested:?}"
    );
    for pr in 0..args.prs {
        ensure!(
            data_dir.join(format!("pr{pr}/data.ldb")).exists(),
            "prepared ledger for PR{pr} is missing"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_args::CommandLine;
    use clap::Parser;

    fn args(command: &str, prs: &str) -> CliArgs {
        let mut argv = vec![
            "nanospam",
            command,
            "--data-dir",
            "/tmp/unused",
            "--prs",
            prs,
            "--accounts",
            "6",
            "--rai-epoch-duration-ms",
            "5000",
            "--rai-tick-interval-ms",
            "100",
        ];
        if command == "run" {
            argv.extend(["--blocks", "1"]);
        }
        CommandLine::parse_from(argv).into_args()
    }

    #[test]
    fn manifest_detects_configuration_mismatch() {
        let directory =
            std::env::temp_dir().join(format!("nanospam-manifest-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(directory.join("pr0")).unwrap();
        fs::write(directory.join("pr0/data.ldb"), []).unwrap();
        let setup = args("setup", "1");
        write_prepared_network(&directory, &setup).unwrap();
        let run = args("run", "1");
        validate_prepared_network(&directory, &run).unwrap();
        let mismatch = args("run", "2");
        assert!(validate_prepared_network(&directory, &mismatch).is_err());
        fs::remove_dir_all(directory).unwrap();
    }
}
