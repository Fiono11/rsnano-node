# Twelve seats: vote records outside the elections lock, A/B, Nov 23 2026

`final-wan-2000-n12` cells fork10-grace and nofork, reps 1-2 on the matrix seeds, interleaved base/lock, same client (`8e63e375`), 11:35-13:10 UTC.
base = matrix build `a0181c9b` (e4601219e); lock = ce61702ec + `lock.patch` (`50623980`): a vote is recorded under its own mutex, placements read under the shared elections lock, the exclusive lock taken only to apply it.

| cell | base run 1 | base run 2 | lock run 1 | lock run 2 |
|---|---|---|---|---|
| fork10-grace | 1,937/s, p95 834 ms, close 2.9 s | 1,936/s, p95 1,324 ms, close 3.9 s | **549/s, p95 51.3 s, close 46.5 s** | 1,887/s, p95 982 ms, close 5.7 s |
| nofork | **1,112/s, p95 11.5 s, close 37.9 s** | 1,937/s, p95 585 ms, close 3.7 s | 1,936/s, p95 566 ms, close 2.9 s | 1,936/s, p95 590 ms, close 3.7 s |

Each build saturated once in four runs: the storms are not caused by vote recording under the exclusive lock, and the change was dropped. Without a storm RAI at twelve seats runs at 1,936-1,937 blocks/s with p95 566-1,324 ms (Nano in the matrix: p95 573-582 ms).
