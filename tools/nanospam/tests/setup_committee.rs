#![cfg(feature = "rai_protocol")]

use std::process::Command;

#[test]
#[ignore = "launches six rsnano nodes and prepares real ledgers"]
fn setup_with_six_prs_is_used_as_the_run_genesis_committee() {
    let directory = std::env::temp_dir().join(format!(
        "nanospam-six-pr-genesis-committee-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);

    let setup = Command::new(env!("CARGO_BIN_EXE_nanospam"))
        .args([
            "setup",
            "--data-dir",
            directory.to_str().unwrap(),
            "--prs",
            "6",
            "--accounts",
            "6",
        ])
        .status()
        .unwrap();
    assert!(setup.success(), "nanospam setup failed: {setup}");

    // `run` queries rai_status on every restarted PR and fails unless all
    // nodes report exactly the six configured genesis committee members.
    let run = Command::new(env!("CARGO_BIN_EXE_nanospam"))
        .args([
            "run",
            "--data-dir",
            directory.to_str().unwrap(),
            "--prs",
            "6",
            "--accounts",
            "6",
            "--blocks",
            "0",
        ])
        .status()
        .unwrap();
    assert!(run.success(), "nanospam run failed: {run}");

    std::fs::remove_dir_all(directory).unwrap();
}
