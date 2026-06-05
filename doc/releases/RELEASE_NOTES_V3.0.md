# RsNano V3.0 Release Notes

This release focuses on first-class Banano support, official Docker images, a
much-improved Insight GUI, and a rewritten bootstrap engine. Below are the
changes that are relevant from a node-operator / user perspective. Internal
refactoring is not listed.

## Highlights

### Banano support
RsNano is now a full **Banano** node in addition to Nano. The currency is
selected at build time via a `banano` feature flag.

- Built-in Banano rep weights (live + beta) and currency constants.
- The node logs the active currency name on startup.
- The `version` RPC now reports `RsBan` when running as a Banano node.
- A dedicated Banano Docker image is available.

### Separate binaries: `rsnano` and `rsban`
The node binaries were renamed. You now run **`rsnano`** for Nano and
**`rsban`** for Banano (and `rsnano-insight` / `rsban-insight` for the GUI).
The corresponding Insight GUI binaries are built per currency as well.

```bash
cargo install --path cli
rsnano --network=live node run
```

### Official Docker images
RsNano now ships official Docker images, published automatically via GitHub
Actions.

- Images are built on **Alpine** (much smaller) using `cargo-chef` for fast,
  cached builds.
- Images are tagged `latest`.
- Separate images for Nano and Banano.

```bash
docker run -p 7075:7075 -v ~/Nano:/home/nanocurrency/Nano rsnano/rsnano-dev:latest node run
```

### Insight GUI app
The Insight monitoring GUI received major additions, giving operators live
visibility into the node:

- **Peer score view** — visualize per-peer scoring.
- **Active Elections (AEC) view** with per-bucket details.
- **Election detail view.**
- **Bootstrap view** — see blocked / downloading / prioritized accounts, with
  account filtering, a consistency check, and a "clear blocked" action.
- Account count display.
- Message log filtering by **direction** and by **type**.

### Rewritten bootstrap engine
The bootstrap (ledger sync) subsystem was substantially reworked for better
reliability and throughput. User-visible behavior changes:

- Live network traffic is **ignored while bootstrapping** to avoid interference.
- Winner blocks are **no longer rebroadcast during bootstrap**.
- "Safe" pulls are only triggered when an election is stale.
- Tuned bootstrap priorities, parallel query counts, and pull sizes to match
  upstream nano_node behavior.
- Numerous fixes for sync stalls, freezes, and block-handoff leaks.

## Removed features

- **Ledger pruning** has been removed.
- The **`republish`** and **`wallet_republish`** RPC commands have been removed.

## Configuration & operational changes

- **New `handshake_timeout` network setting.** Slow/incomplete handshakes are
  now purged, with centralized idle-channel cleanup replacing the old
  per-channel timeout.
- **LMDB defaults changed for performance:** `nosync_unsafe` is now the default
  sync strategy and `NO_READAHEAD` is always used. The active sync strategy is
  logged on startup.
- **Ledger consistency checks** at startup (balance, total weight, and rep
  weight consistency). A warning is logged when the consistency check is
  skipped.
- Rep weights are now loaded from the dedicated `rep_weight` table.

## CLI & tooling

- New CLI command to display **representative weight info**.
- New **ledger diff** tool for comparing two ledgers.

## Performance

- Wallets now **process blocks and generate proof-of-work in parallel**.
- Unlocking a wallet no longer immediately triggers a receivable search.
- Various block-processing and queue performance improvements.

## Consensus tweaks

- Optimistic elections now schedule **largest-gap accounts first**.

## Under the hood

- Upgraded to **Rust edition 2024** and refreshed dependencies.
- A lot of code cleanup in various components
- Continued ongoing port and sync with upstream nano_node (tracked in
  `doc/upstream.md`).
