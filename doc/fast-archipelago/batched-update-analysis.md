# One update with repeated responses: investigation

4 October 2026. Proposed semantics only; no production or PDF changes.

## Findings

There are three different rules:

1. **Freeze after the first Q distinct phase requests; later replies acknowledge receipt.** This repairs the previous missing-witness deadlock but admits a repeatable nondeciding rank in the abstract model below. It does not implement clean request-before-update rounds, even with bounded message delays.
2. **Freeze and refuse acknowledgements for requests not incorporated.** The preceding `request-barrier-deadlock.json` still applies.
3. **Update once at a synchronized round boundary from the accumulated requests; answer repeatedly afterward; buffer late requests for a later update.** This avoids both particular schedules if the boundary actually follows delivery of all relevant awake-correct requests. Establishing that condition, handling earlier frozen state and historic certificates, and proving global progress remain necessary. A count of Q alone is not that condition.

A receipt response certifies delivery/validation, not that its triggering value was incorporated in the state. The original source origin witnesses are responses to broadcasts; its update-on-delivery handler connects receipt and incorporation. Freezing the state breaks that connection unless explicitly restored. Our earlier exclusion argument assumed processing witnesses and cannot be carried over to receipt-only witnesses.

## Repeatable rank, n=6, f=p=1, Q=4

Identities 1–5 are correct and 6 is Byzantine. Let v<w. Correct carry values entering every rank are `1:v, 2:v, 3:v, 4:w, 5:w`. Identity 6 sends a legal v R request, using an available v predecessor when rank>0.

Every correct requester sends once per phase. Each responder batches exactly four distinct request identities, including itself. Responders 1,2,3 first process requests `{1,2,3,6}`; responders 4,5 first process `{1,2,4,5}`. They then freeze state for that phase and continue replying to all requests, including late ones. These receipt replies come from all five correct identities, so every origin can obtain Q witnesses. No signature of a correct identity is forged.

### R and recovery

R maxima and immutable first votes are v at 1,2,3 and w at 4,5. Byzantine 6 reports v to the v group and equivocates with a w first vote for recovery elsewhere. Each record can carry legal predecessor evidence for its own value. There is no fast certificate: even with Byzantine help v has at most four signers, below F=5.

Singleton recovery v uses `{1:v,2:v,3:v,4:w}`. Singleton recovery w uses `{1:v,4:w,5:w,6:w}`. At m=4 support is two; these resolve to v and w respectively. Correct identities do not change their first votes. All can later receive all records without invalidating the already valid recovery certificates.

### A

Requesters 1,2,3,6 send A(v); 4,5 send A(w), justified by the corresponding recovery certificates. Frozen correct A responses are `{v}` at 1,2,3 and `{v,w}` at 4,5. Byzantine 6 returns `{v}`.

The quorum `{1,2,3,6}` certifies true-v. The quorum `{1,2,4,5}` certifies false-w. All origins have sufficient receipt witnesses, including late replies. Schedule the mixed certificate first at correct requesters 1,2,3, and the true certificate first at 4,5. Thus their B requests are false-w at 1,2,3 and true-v at 4,5. Byzantine 6 sends false-w with valid mixed A evidence.

### B

Responders 1,2,3 freeze after `{1,2,3,6}`, all false-w requests. Responders 4,5 freeze after `{1,2,4,5}`, giving mixed false-w/true-v state. Byzantine 6 responds false-w.

Quorum `{1,2,3,6}` adopts w. Quorum `{1,2,4,5}` adopts v. No quorum commits: no correct responder emits a singleton true response in this B step. Deliver the mixed certificate first to 1,2,3 and the false-only certificate first to 4,5. The resulting carry values are again `v,v,v,w,w`.

### Why the pattern can repeat

Each correct next-rank introduction uses its actual B result at the immediately previous rank. Both predecessor values have valid certificates under these proposed receipt-witness semantics. Per-rank state and first-vote slots start fresh, so the same construction applies inductively. Old lower-rank messages cannot supersede the newly certified higher rank merely by carrying a larger value.

Every phase can deliver all its remaining messages and evidence before starting the next phase. Correct replicas are never suspended. A finite message ordering with fixed bounded delays realizes each rank, repeated indefinitely; there is no need to withhold a correct message forever. Fair retransmission adds no new first-vote identities and does not change frozen phase states.

This is a certificate-level nontermination construction for the stated count-based variant, not a counterexample satisfying the source's stronger clean-round delivery-before-state-update assumption. A synchronizer enforcing that assumption would exclude the schedule. Any additional request/response-binding rule that excludes the certificates also needs to be made explicit and checked against progress.

## Nine-replica check

For n=9,f=2,p=1,Q=6,F=8, the same model uses five correct v carriers, two correct w carriers and two Byzantine identities. The singleton-v response quorum has five correct identities and one Byzantine. A w recovery has two correct w votes, two Byzantine w votes and two correct v votes: support three selects only w. No fast certificate is possible from five correct v voters plus two Byzantine voters. The construction returns the same carry partition.

## Synchronized batched updates

If the mixed A requests in the trace are all incorporated before the single update, every correct A state contains v and w. Every Q-response certificate contains a correct mixed response, so no new true-v certificate can form from those snapshots. A returns false-w and B adopts w; with only w admissible in a subsequent fresh rank, B can commit w. The script checks this particular execution, not arbitrary historical evidence. If R itself has a clean full batch with no conflicting historical maximum, convergence may occur sooner.

A useful candidate design is one update **per communication round**, not one lifetime update per phase/rank: retain monotone A/B state, batch pending valid requests at the boundary, and reply with a signed snapshot carrying explicit instance/rank/phase/round identity. Already issued snapshots remain historical evidence. The signed round field alone does not invalidate them, authorize rank skipping, or prove convergence.

Required further specification:

- How round boundaries and request-before-update ordering are obtained after GST without blocking forever before GST.
- Whether acknowledgements mean receipt or incorporation, and exactly which ones satisfy W/Q witness checks.
- How a replica waiting for evidence gets another update opportunity at the same rank/phase.
- How proposers select responses across communication rounds and what older certificates may still authorize.
- How the resulting synchronizer interacts with immutable first votes, maximum-rank selection, and suspended/returning replicas.

## Reproduction and limits

Run `python3 tools/rai/check_batched_updates.py`. Output is saved in `batched-update-traces.json`. For both configurations it checks three successive ranks, singleton recovery arithmetic, correct first-vote consistency, distinct Q batches including self, certificate evaluations, absence of possible fast certificates, absence of any committing B quorum from the selected snapshots, per-rank predecessor carry, and repetition of the carry partition. The repeatability argument above extends the constructed pattern; merely running three ranks would not prove nontermination.

Cryptographic authentication is abstracted by identities. Requests and response values are checked against the constructed valid input maps; this is not the production recursive verifier, a formal model checker, or an exhaustive schedule search. RAI application validity is assumed for rank-zero v and w. Neither the full source algorithm nor the corrected Fast Archipelago document has been proved nonterminating by this experiment.
