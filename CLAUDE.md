# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is RsNano?

RsNano is a full Nano/Banano node written in Rust. It implements the Nano payment protocol with ultrafast transactions and zero fees.

## Commands

### Build
```bash
cargo build --all-targets          # build everything including tests
cargo build --bin rsnano_node  # build the node binary
```

### Test
```bash
cargo test --lib -q                # Run only the unit tests. This is the preferred command during development 
cargo test -q                      # run unit tests and integation tests
cargo test -q -p rsnano_node       # run tests for a specific crate
cargo test -q some_test_name       # run a specific test by name
```

### Lint / Format
```bash
cargo fmt --all                    # format all code
cargo fmt --all --check            # check formatting (used in CI)
```

### Run the node
```bash
cargo run --bin rsnano_node -- --network=live node run
```

## Architecture

### Design Philosophy

The codebase follows **A-frame architecture** and **nullable infrastructure** (testing without mocks). Key idea: infrastructure dependencies are wrapped in nullable versions that can be swapped out in tests without mocking frameworks. See [James Shore's documentation](http://www.jamesshore.com/v2/projects/nullables/testing-without-mocks) for details.

### Crate Structure

The workspace is split into focused crates:

| Crate | Purpose |
|-------|---------|
| `main` | Node executable entry point |
| `daemon` | Starts node and optional RPC server |
| `node` | Core node implementation (consensus, block processing, bootstrap, transport) |
| `ledger` | Ledger consistency and block validation/insertion/rollback |
| `store_lmdb` | LMDB persistence layer |
| `network` | Manages outbound/inbound TCP channels to other peers |
| `network_protocol` | Handshake, message framing, inbound queue |
| `messages` | Network message types |
| `rpc/server` | RPC server implementation |
| `websocket/server` | WebSocket server implementation |
| `wallet` | Multi-wallet, multi-account management |
| `work` | Proof-of-work generation (CPU/GPU) |
| `types` | Core types: `BlockHash`, `Account`, `KeyPair`, `Vote`, etc. |
| `utils` | Stats, thread pool, ticker, fair queue, cancellation tokens |
| `nullables/*` | Nullable wrappers: `clock`, `condvar`, `fs`, `lmdb`, `tcp`, `random`, `output_tracker`, etc. |
| `tools/test_helpers` | Shared test utilities (e.g., `UnsavedBlockLatticeBuilder`) |

### Node internals (`node` crate)

Major subsystems inside `node/src/`:

- **`block_processing/`** — `BlockProcessor`, `BlockProcessorQueue`, `BoundedBacklog`, `BacklogIndex`, `BacklogScan`, `UncheckedMap`, `LocalBlockBroadcaster`
- **`consensus/`** — Active Elections Container (AEC), election schedulers, vote processing pipeline (`VoteProcessor` → `VoteApplier` → `VoteBroadcaster`), fork cache, vote cache, confirmation solicitor, request aggregator, vote rebroadcast
- **`bootstrap/`** — `Bootstrapper`, `BootstrapServer`, bootstrap election activator
- **`cementation/`** — `ConfirmingSet`, confirmation time tracking
- **`transport/`** — Peer connectors, channel management
- **`telemetry/`** — Node telemetry collection

### Ledger internals (`ledger` crate)

- **`block_insertion/`** — `BlockValidator` checks rules → `BlockInserter` follows `BlockInsertInstructions`
- **`block_rollback/`** — `RollbackPlanner` plans rollback → `RollbackInstructionsExecutor` executes it
- **`ledger_sets.rs`** — `LedgerSet` views: confirmed, unconfirmed, any

### Nullable infrastructure pattern

Each infrastructure concern has a nullable wrapper crate under `nullables/`:
- `rsnano_nullable_clock` — `SteadyClock`, `SystemTimeFactory`
- `rsnano_nullable_lmdb` — `LmdbEnvironment`, `LmdbEnvironmentFactory`
- `rsnano_nullable_fs` — `NullableFilesystem`
- `rsnano_nullable_condvar` — `NullableCondvarMutex`
- `rsnano_output_tracker` — `OutputTracker`/`OutputTrackerMt` for recording outputs in tests

In tests, use `Ledger::new_null()` and `*::new_null()` constructors to get in-memory/stub implementations. Use `UnsavedBlockLatticeBuilder::with_stub_work()` from `tools/test_helpers` to build test block chains.

### Stats system

Stats are in `rsnano_utils::stats`. Components hold an `Arc<Stats>` and call `.inc()`, `.add()`, etc. Stats use atomic counters grouped by direction (`Direction`).

### Threading model

Long-running work uses `ThreadPool` (from `rsnano_utils::thread_pool`) and `TickerPool`/`TimerThread` for periodic tasks. `CancellationToken` is used for cooperative shutdown. Background threads use `backpressure_channel` for flow control.

## Feature Flags

- `banano` — Compile as a Banano node instead of Nano
- `ledger_snapshots` — Enable ledger snapshot/fork detection tooling (gated behind this feature)
