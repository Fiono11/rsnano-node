# Notes for the paper

Points to work into the manuscript. Remove an entry once the text carries it.

## Arguments

- **Protocol changes (advantage over Nano).** RAI makes protocol changes easy and smooth. In Nano, a change to the protocol needs an epoch block appended to every account. In RAI, every replica agrees on the epoch boundaries and on the checkpoint that closes each epoch, so a change can take effect at an agreed epoch with no per-account blocks. The prototype does not implement a version switch, so present this as a property of the design, not an evaluated result. (User note, Oct 20 2026.)

## Pending text changes

- **Proposals carry the selected reports' signed commitments** (3a8bad905). Each selected report comes with the reporter's committee digest and signature. A replica that holds another version of a report, or none, authenticates the version selected and rebuilds it. Without this, a Byzantine reporter that signs one report for half of the replicas and another for the rest makes every proposal selecting the first version uncheckable at the others. The proposal description in Section VI needs a sentence.
- **Latency claim in Section X.** "Without forks RAI has the lowest median, 138 ms against Nano's 195 ms" holds only with the fast path. At Nano's quorum (n = 6, f = 1, p = 0, fast path off, `rai-paper-matrix-p0-2026-10-20`) the medians are 233 ms (RAI) against 191 ms (Nano).
