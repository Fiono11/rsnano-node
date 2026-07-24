# RAI frontier-ledger-root Rust PoC

This project is the uploaded Rust PoC edited for the revised RAI specification in
`rai_spec_frontier_ledger_root.tex`.

## Main protocol changes

- `CloseRecord` now commits to:
  - `previous_close_hash`
  - `close_cut_hash`
  - `ledger_root`
- `ledger_root` is the canonical hash of the complete `(account_id, frontier_hash)` map.
- `ClosePackage` carries the concrete canonical frontier map rather than finalized and selected consensus lists.
- Package validation reconstructs every committed account chain from genesis and replays all ledger transitions.
- Package-local `Selected` evidence is converted to durable `Finalized(CloseRecord)` status only when its block appears on a certified frontier chain.
- Released elections must have no candidate on the certified ledger.
- Close-package admissibility is stable and no longer merges later local evidence into package validation.
- Final votes are accepted in close elections and a close-round final certificate is treated as carry evidence.
- Committee identifiers are derived from the certified close hash and replay-derived balances/delegations.

## Files changed

- `src/block.rs`
- `src/close.rs`
- `src/engine.rs`
- `src/lib.rs`
- `src/simulation.rs`
- `src/vote.rs`
- `tests/protocol.rs`

The remaining source and test files are included unchanged for a complete Cargo project.

## Validation performed

All Rust source and test files were parsed with the Tree-sitter Rust grammar and contain no syntax-error nodes.
The execution environment did not contain `rustc` or `cargo`, so the project could not be type-checked or executed here.
Run locally:

```bash
cargo fmt --check
cargo check
cargo test
```
