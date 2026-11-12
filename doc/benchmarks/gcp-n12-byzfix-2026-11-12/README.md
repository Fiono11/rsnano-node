# Twelve seats, fork10 with a Byzantine validator, after the evidence-lane fix (Nov 12 2026)

Build b02c83039: the build of `../gcp-n12-confirm-2026-11-12` plus a vote-queue lane for
requested evidence (8,192 replies), manifests asked of every principal representative after 1 s
with the proposal's sender, and a log of what a manifest names that a validator lacks
(`EPOCH_MANIFEST_MISSING_WHO`). Same cell and seed as the collapsed confirmation run. The second
Montréal VM was switched to standard provisioning after two Spot preemptions (a first attempt
of this run was lost to one at 12:23:47 UTC).

| | Collapsed run (b49726dce) | This run (b02c83039) |
|---|---|---|
| Status | settled | settled (client hit its drain limit at 33,262 confirmed) |
| Blocks/s | 209 | – (client gave up) |
| Close median / max | 19.3 / 154 s | 45.3 / 129 s |
| Rounds, epochs 2 and 3 | 3, 11 | 4, 6 |
| Votes dropped per honest validator | 0.5 - 2.0 M | 0.1 - 1.8 M |

**Not fixed.** The manifest now arrives within 5 to 10 s of a round (20.6 s before), but the flood
that drops votes is upstream of the vote queue (`diagnosis.txt`):

- Open elections grow to about 14,000 by 35 s and 24,000 to 28,000 after 70 s and never shrink:
  the epoch-2 close took 41 to 45 s, so new blocks wait for the checkpoint while forked
  positions stay open.
- Every open election is solicited, at the 5,000-instances-per-second cap for the whole run
  (0.8 to 1.3 M asked per validator); each request draws vote replies from eleven validators.
- The evidence lane sits at its 8,192 cap: a missing vote is asked of every validator, and every
  holder replies.
- What a manifest names that validators lack is a few validators' final votes: PR7
  (`E51262F8`, 3,446 at PR0) and PR10 (`B72FC2F3`) in epoch 3, the Byzantine validator's own
  (`6F4C5FE3`) in epoch 2.

Next candidates: ask the proposer, which holds every vote its manifest names, for the missing
votes before asking everyone; scale the solicitation cap with the committee size.
