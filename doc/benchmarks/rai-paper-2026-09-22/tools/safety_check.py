#!/usr/bin/env python3
"""RAI safety: what the run finalized may not contradict itself, across all epochs and PRs.

The other checks compare the PRs with each other: they catch a replica that disagrees, not a
network that agrees on something wrong. These are the invariants themselves, over the union of
what every PR reports, so a violation every PR shares still fails.

`final_state` with an `epoch` lists that epoch's state entries per PR: "finalized" (an instance
of the epoch finalized the block by a certificate) and "single" (a settled instance of the epoch
holds exactly one notarization certificate, so the block is the account's next one). A slot is
an (account, height); two blocks of one slot conflict.

  S1  No two conflicting blocks are finalized, over all epochs and all PRs.
  S2  A slot with a finalized block and a different block entering the state as "single" is
      reported, not failed: a Byzantine representative can notarize a fork's loser in another
      epoch's instance (three honest first votes plus its own), which pollutes that epoch's
      state but never the ledger - only S1 is the safety invariant.
  S3  A block finalized in more than one epoch is reported, not failed: an instance of the same
      block may run in two epochs when the epoch changes under it, and both may finalize it
      (`EpochStates`: "A block decided while the epoch changed can be finalized in both").
      The count is printed so that a change in this behaviour is visible.
  S4  Every finalized block is in every PR's ledger, cemented, at the height it was finalized at.
      Sampled (default 300 slots), plus every slot involved in S1-S3.
  S5  No block rolled back as late (EPOCH_DISCARDED in the run log, if one is given) had been
      finalized: a finalization in the discard's epoch or an earlier one. Finalized in a later
      epoch is fine - the discard was right, the block was republished and decided afresh.

Usage: safety_check.py [run.log] [sample]
Prints the counts, then SAFE or SAFETY_VIOLATION.
"""
import json
import random
import re
import sys
import urllib.request
from collections import defaultdict

# The nodes running: PRS in the environment, six by default
PORTS = [17076 + 10 * i for i in range(int(__import__("os").environ.get("PRS", "6")))]
LOG = sys.argv[1] if len(sys.argv) > 1 else None
SAMPLE = int(sys.argv[2]) if len(sys.argv) > 2 else 300


def rpc(port, body):
    req = urllib.request.Request(
        f"http://[::1]:{port}",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    return json.loads(urllib.request.urlopen(req, timeout=60).read())


def collect():
    """(slot -> kind -> hash -> [(pr, epoch)], block -> epochs, epochs seen)"""
    by_slot = defaultdict(lambda: defaultdict(lambda: defaultdict(list)))
    block_epochs = defaultdict(set)
    epochs = set()
    for pr, port in enumerate(PORTS):
        state = rpc(port, {"action": "final_state"})
        for entry in state.get("epochs", []):
            epochs.add(int(entry["epoch"]))
        for epoch in sorted(int(e["epoch"]) for e in state.get("epochs", [])):
            listed = rpc(port, {"action": "final_state", "epoch": str(epoch)})
            for e in listed.get("entries", []):
                slot = (e["account"], int(e["height"]))
                by_slot[slot][e["kind"]][e["hash"]].append((pr, epoch))
                if e["kind"] == "finalized":
                    block_epochs[e["hash"]].add(epoch)
    return by_slot, block_epochs, sorted(epochs)


def cemented_at(slot, block, violations):
    """S4: every PR holds the block, cemented, at that height"""
    account, height = slot
    for pr, port in enumerate(PORTS):
        try:
            info = rpc(port, {"action": "block_info", "hash": block, "json_block": "true"})
        except Exception as error:
            violations.append(f"S4 {account[-8:]}#{height} {block[:8]}: not in PR{pr}'s ledger ({str(error)[:60]})")
            continue
        # An RPC error comes back as an object without the block's fields:
        # the block is not in that PR's ledger at all
        if "confirmed" not in info:
            violations.append(
                f"S4 {account[-8:]}#{height} {block[:8]}: not in PR{pr}'s ledger "
                f"({str(info)[:80]})"
            )
            continue
        if not json.loads(str(info["confirmed"]).lower()):
            violations.append(f"S4 {account[-8:]}#{height} {block[:8]}: not cemented on PR{pr}")
        if int(info["height"]) != height:
            violations.append(f"S4 {account[-8:]}#{height} {block[:8]}: height {info['height']} on PR{pr}")
        if info["block_account"] != account:
            violations.append(f"S4 {account[-8:]}#{height} {block[:8]}: account {info['block_account'][-8:]} on PR{pr}")


def discarded_hashes():
    """block -> the epoch it was discarded in (the earliest, if several)"""
    if not LOG:
        return {}
    discarded = {}
    with open(LOG, "rb") as log:
        for line in log.read().decode("utf8", "replace").splitlines():
            if "EPOCH_DISCARDED" not in line:
                continue
            epoch = re.search(r"EPOCH_DISCARDED epoch=(\d+)", line)
            if not epoch:
                continue
            for h in re.findall(r"\b[0-9A-F]{64}\b", line):
                discarded[h] = min(discarded.get(h, 1 << 62), int(epoch.group(1)))
    return discarded


def main():
    by_slot, block_epochs, epochs = collect()
    violations = []
    s2 = []
    suspect = set()

    forked_slots = 0
    for slot, kinds in by_slot.items():
        finalized = set(kinds.get("finalized", {}))
        single = set(kinds.get("single", {}))
        if len(finalized) > 1:
            where = {h[:8]: sorted(set(kinds["finalized"][h])) for h in finalized}
            violations.append(f"S1 {slot[0][-8:]}#{slot[1]}: {len(finalized)} finalized blocks {where}")
            suspect.add(slot)
        if finalized and single - finalized:
            s2.append(
                f"S2 {slot[0][-8:]}#{slot[1]}: finalized {[h[:8] for h in finalized]} "
                f"and single {[h[:8] for h in single - finalized]}"
            )
            suspect.add(slot)
        if len(finalized | single) > 1:
            forked_slots += 1

    multi_epoch = {h: sorted(e) for h, e in block_epochs.items() if len(e) > 1}
    for block, in_epochs in list(multi_epoch.items())[:5]:
        print(f"S3 block {block[:8]} finalized in epochs {in_epochs}")

    discarded = discarded_hashes()
    refinalized = 0
    for block, discard_epoch in sorted(discarded.items()):
        if block not in block_epochs:
            continue
        earliest = min(block_epochs[block])
        if earliest <= discard_epoch:
            violations.append(
                f"S5 block {block[:8]} finalized in {sorted(block_epochs[block])} was discarded as late in epoch {discard_epoch}"
            )
        else:
            refinalized += 1

    slots = [s for s in by_slot if "finalized" in by_slot[s]]
    sampled = set(random.sample(slots, min(SAMPLE, len(slots)))) | suspect
    for slot in sampled:
        for block in by_slot[slot]["finalized"]:
            cemented_at(slot, block, violations)

    print(f"epochs={epochs} slots={len(by_slot)} finalized slots={len(slots)} "
          f"finalized blocks={len(block_epochs)} slots with more than one block in the state={forked_slots}")
    for line in s2[:5]:
        print(line)
    print(f"S2 slots finalized one way and single-notarized another in some epoch: {len(s2)}")
    print(f"S3 blocks finalized in more than one epoch: {len(multi_epoch)}")
    print(f"S5 blocks discarded as late: {len(discarded)}, of which decided afresh in a later epoch: {refinalized}" + ("" if LOG else " (no run log given)"))
    print(f"S4 slots checked against every PR's ledger: {len(sampled)} of {len(slots)}")
    if violations:
        for line in violations[:30]:
            print(f"  {line}")
        print(f"SAFETY_VIOLATION {len(violations)}")
        return 1
    print("SAFE")
    return 0


if __name__ == "__main__":
    sys.exit(main())
