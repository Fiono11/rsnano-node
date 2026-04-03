# Plan: Remove deprecated `write()` from AecService

## Context

`AecService` is the facade over `Arc<RwLock<ActiveElectionsContainer>>`. All operations should be encapsulated as forwarded methods. A deprecated `write()` method still exposes the raw `RwLockWriteGuard` to callers. The only remaining caller is `AecVoter::tick()` (`node/src/consensus/vote_generation/aec_voter.rs:54`).

Currently `AecVoter` holds a write lock for the entire voting round because it interleaves reads (`iter_bucket`) with writes (`set_last_voted`). The root cause is that vote-scheduling state (when was an election last voted?) lives inside `Election`/`ActiveElectionsContainer`, forcing `AecVoter` to write back into the AEC after each vote.

---

## Approach: Move vote-scheduling state into AecVoter

`AecVoter` owns a `VotingScheduler` that tracks when it last voted for each election locally. AEC no longer needs to be written during the voting round — only a read lock is needed. Cleanup happens via `AecFact::ElectionEnded`.

### 1. Add `VotingScheduler` logic struct

New file: `node/src/consensus/vote_generation/voting_scheduler.rs`

```rust
pub(crate) struct VotingScheduler {
    last_voted: HashMap<QualifiedRoot, (Timestamp, VoteType)>,
}

impl VotingScheduler {
    pub fn can_vote(&self, root: &QualifiedRoot, interval: Duration, now: Timestamp) -> bool { ... }
    pub fn mark_voted(&mut self, root: &QualifiedRoot, vote_type: VoteType, now: Timestamp) { ... }
    pub fn remove(&mut self, root: &QualifiedRoot) { ... }
}
```

Pure logic — no infrastructure, no locks. Unit-testable directly.

### 2. Add `with_elections_starting_from_bucket` to AecService (read-only, callback)

Mirrors the existing `with_elections` pattern but starts the round-robin at a given bucket:

```rust
// On AecService (forwarded from ActiveElectionsContainer):
pub fn with_elections_starting_from_bucket<F, T>(&self, starting_bucket: usize, f: F) -> T
where
    F: FnOnce(&mut dyn Iterator<Item = (usize, &Election)>) -> T
```

Acquires a **read lock**, builds an iterator that yields `(bucket_id, &Election)` starting from `starting_bucket` and continues round-robin through the remaining buckets, and passes it to the closure. The bucket index is included so callers can track position. No allocations unless the caller creates them. No side effects on the container.

This replaces the inner `loop` in `AecVoter::tick()`: instead of manually stepping through buckets, the closure receives a single iterator spanning all buckets from the starting position.

### 3. Refactor AecVoter

`AecVoter` owns a `VotingScheduler` and subscribes to `AecFact` events for cleanup. The inner bucket-stepping loop is replaced by `with_elections_starting_from_bucket`. `self.current_bucket` is preserved: on CPS hit it stays at the found bucket (next tick retries from there); on a successful vote it advances to the next bucket; when no more votes are found it resets to `bucket_count() - 1`.

```rust
fn tick(&mut self, cancel_token: &CancellationToken) {
    let now = self.clock.now();
    let mut vote_queue = Vec::new();
    loop {
        let vote_target = self.aec.with_elections_starting_from_bucket(
            self.current_bucket,
            |elections| elections.find_map(|(bucket, e)| {
                let root = e.qualified_root();
                if self.scheduler.can_vote(root, self.vote_broadcast_interval, now) {
                    Some((bucket, root.clone(), e.winner().hash(), e.vote_type()))
                } else {
                    None
                }
            }),
        );

        let Some((bucket, root, hash, vote_type)) = vote_target else {
            self.current_bucket = bucket_count() - 1;
            break;
        };

        if vote_type == VoteType::NonFinal && !self.cps_limiter.try_vote(now) {
            self.current_bucket = bucket; // resume from same bucket next tick
            self.flush(&mut vote_queue);
            return;
        }

        self.current_bucket = if bucket == 0 { bucket_count() - 1 } else { bucket - 1 };
        vote_queue.push((root.root, hash, vote_type));
        self.scheduler.mark_voted(&root, vote_type, now);

        if cancel_token.is_cancelled() { return; }
    }
    self.flush(&mut vote_queue);
}
```

For cleanup, `AecVoter` receives `AecFact` events and calls `self.scheduler.remove(root)` on `AecFact::ElectionEnded`.

### 4. Remove `set_last_voted` and `can_vote` from Election / ActiveElectionsContainer

Once no caller uses them, the vote timestamp field and these methods can be deleted from `Election` and `ActiveElectionsContainer`, reducing the AEC surface area.

---

## Files to Change

- `node/src/consensus/vote_generation/voting_scheduler.rs` — new `VotingScheduler` logic struct
- `node/src/consensus/vote_generation/mod.rs` — expose `VotingScheduler`
- `node/src/consensus/active_elections/active_elections_container.rs` — add `iter_bucket` usage via `with_elections_starting_from_bucket`; remove `set_last_voted`, `can_vote`, vote timestamp from `Election`
- `node/src/consensus/active_elections/aec_service.rs` — add forwarded `with_elections_starting_from_bucket`, remove `write()`
- `node/src/consensus/vote_generation/aec_voter.rs` — add `VotingScheduler`, wire `AecFact` cleanup, refactor `tick()`

## Verification

- `cargo check --tests` — no compile errors
- `cargo test --lib -q` — all unit tests pass
- Add unit tests for `VotingScheduler`: `can_vote` respects interval, `mark_voted` updates state, `remove` cleans up entry
