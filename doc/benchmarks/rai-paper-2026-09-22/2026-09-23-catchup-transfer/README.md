# Checkpoint difference phase — 2026-09-23

Adds 0x1b/0x1c paginated predecessor-to-checkpoint differences. Pages are bounded
to 600 records and bound epoch, source root, certified target root and offset.
Only the fully rebuilt ledger with the certified state hash is returned; partial
pages cannot change decided state, and predecessor finality cannot be replaced.
Retained forks can be removed or promoted, without application effects while
retained. Duplicate/reordered/corrupt pages and unbounded totals are rejected.

Retention: 256 close proofs and target ledgers, plus one predecessor for the
oldest served difference. Eight canonical differences are cached; this cache
can be reconstructed from the retained states. Transfers admit at most 1,000,000
edits. Nodes outside the retention horizon need an archival peer; missing
history never authorizes skipping proof verification. Durable restart anchors
and live orchestration follow in piece 3.

Required feature tests passed: 98 message tests, 768 node tests. The complete
default library suite passed. Required release build passed; EPOCH_START count 1.

The provisional fork0 run timed out in report reconstruction (reconciliation
refused differences above the existing 600-entry report limit); SAFE passed.
Before fork5, the idle guard repeatedly detected WindowServer and Cursor Helper
above 25% CPU. The matrix was stopped with no nodes running, before starting
compilation of piece 3. These are **not accepted performance measurements**.
The complete benchmark gate remains pending on the final installation build.
