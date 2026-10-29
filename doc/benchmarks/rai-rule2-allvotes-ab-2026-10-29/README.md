# Rule 2 with every first vote counted: diagnostic A/B

Why RAI finalizes leftover forks more slowly on 205eeb3d3. Same build with one line changed in
`build_state_of` (Rule 2 also counts `ResidualKind::EarlyFirst`), not committed. p = 0 fork10-grace
cell, the three seeds of `rai-rule2-matrix-p0-2026-10-29` (2001090-2001092), RAI only.

| | settled only (matrix) | every first vote (here) |
|---|---|---|
| positions dropped at the first close after the load (EPOCH_OMITTED, pr0) | 475 / 529 / 396 | 0 / 0 / 0 |
| last open block final after the load | 12.5 / 8.3 / 10.6 s (harness grace trace) | 4.1 / 6.2 / 6.2 s |
| non-fork blocks/s | 1,545 / 1,405 / 1,532 | 1,499 / 1,519 / 1,531 |

A checkpoint is installed 2-6 s into the next 8 s epoch, so most first votes for a fresh fork are
early; with settled counting a checkpoint that finds no settled vote and nothing to keep from its
predecessor keeps nothing, and the position waits one more epoch. The paper keeps settled counting
(the proof relies on it); both variants pass the RaiRecoverMany safety checks.
