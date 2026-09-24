# Baseline settlement failure and reconstruction correction

Evidence: `step1-rnf-projection-p50-p95/pair-01-baseline/` under
`/tmp/rai-cross-epoch-artifacts/`. Frozen baseline remains `5e037cfe0527d7b06b573456c7c876568ce179ea`.

PR4 received all 45,065 blocks, but cemented only 16,243 and reported 13,528
pending entries after the settlement window. Its final-state hash differed
from the other five nodes. The client confirmed all 45,000 primary blocks;
client completion alone therefore failed to detect this stalled peer.

The combined node log repeatedly contains `UnknownSource` reconstruction
refusals for epochs 0 and 1, alongside `UnknownTarget` refusals from nodes
that do not hold the requested snapshot. Rate-limited log-record counts are
159/135 for UnknownSource and 153/129 for UnknownTarget, respectively. These
are not network request counts and the combined log does not identify each
responding process. Exact refusal lines and RPC summaries are retained in
`reconciliation-diagnosis.json` and `settlement-diagnosis.json`.

The source audit identifies a concrete convergence gap: the periodic report
loop refreshes only epochs not yet decided locally, while incoming requests
consult only cached report-exchange states. A completed validator can retain
a stale live source forever, despite having newer evidence in its election
history. A late peer cannot bridge from that validator's actual current view.
This is consistent with the failure, but the logs do not establish it as the
sole root cause of PR4's stalled state.

The candidate now retries an UnknownSource request after refreshing from the
complete canonical epoch projection. This refresh works after local decision,
is limited to once per epoch per 200 ms, and does not run for unknown targets
or successful cached requests. Projection work occurs outside the election
lock and outside the report-exchange lock. No full-target fallback or wire
format change is introduced. Frozen signed roots and snapshots are unchanged.

A second correction replaces the cached live projection rather than unioning
it with prior entries. A union can retain a branch that selected-prefix
finality excluded and prevent root convergence. Historical/signed snapshots
remain separately retained. Tests exercise a request served without periodic
tick, unknown-target rejection without projection work, and replacement with
continued reconstruction of the original signed snapshot.

This correction does not resolve the separate 600-edit reply bound, complete
signed-evidence retention/gossip, or other protocol gaps in IMPLEMENTATION.md.
The baseline binary is not patched. A new five-pair batch is a separate
experiment; the earlier FAIL_SETTLEMENT remains part of the evaluation.
