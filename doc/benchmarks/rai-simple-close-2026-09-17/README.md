# RAI simple close (2026-09-17)

Close phase reduced to "assembler proposes its own membership root, others
validate and vote": no announcements, sketches, pages or view reconstruction.
Memberships converge during the epoch through the elections themselves:

- an election is *settled* when finalized (FINAL weight above total minus
  certificate weight leaves no room for another certificate) or once every
  representative was asked for its votes and had time to answer;
- an election is asked only after it has been open longer than elections take
  to settle (`SettleTime`, an EWMA of start-to-settlement with a 3-deviation
  margin), in one batch per scan; once the close is due every unsettled
  election is asked at once and readiness follows one reply wait;
- a replica is close-ready only when every election is decided and settled.

`new*`/`est1` = this build; `base*` = the announcement/sketch build
(target-baseline, commit before d3e058784). 2000 blocks/s, 5 % forks, two
25 000-election epochs, six PRs, alternating runs on a quiet machine.
`analysis.txt` has close times and per-epoch latency. Tooling: `run_one.sh`
(one run, data dir deleted), `series.sh` (alternating pairs), `summarize.py`
(close timeline from the `RAI_CLOSE_TRACE_DIR` trace), `missing.py` (members a
lagging replica lacks, from the per-node membership dumps the closer writes to
the trace dir at readiness).

Found and fixed on the way: `first_recovery_targets` (2-3 s per closer tick at
drain start under the generators' lock) and the inline `ready` membership dump
(another 2 s) delayed proposal delivery past the 3 s first-vote budget, costing
a round per close.
