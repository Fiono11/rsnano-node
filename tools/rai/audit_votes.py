#!/usr/bin/env python3
"""Equivocation audit of a run: did any node sign two votes its own records
forbid? Reads the signed-votes.log every node wrote with nanospam
--audit-votes (node config `signed_vote_log = true`): one SIGNED line per
vote, appended before the vote left the process, and an OPENED line at each
process start, so a node killed and restarted keeps one file with one
lifetime per start.

A representative equivocates when, in one instance (an epoch, or a round of
an epoch's close election) and at one root, it signs
  - first votes for two different hashes (first, early and abstain are all
    first votes; abstain is the first vote for the timeout block), or
  - final votes for two different hashes.
Notarization and timeout votes may name several blocks and are not checked.
The Byzantine representatives run no node and write no log.

Prints one VOTE_AUDIT json line; exit status 1 when any conflict is found.

    audit_votes.py <data-dir> [--json out.json]
"""
import argparse
import json
from collections import defaultdict
from pathlib import Path

FIRST_KINDS = {'first', 'early', 'abstain'}
FINAL_KINDS = {'final'}
CLOSE_FLAG = 1 << 63
ROUND_BITS = 16


def instance(raw):
    """The raw epoch field: an epoch, or the close round flagged by the top bit"""
    raw = int(raw)
    if raw & CLOSE_FLAG:
        bits = raw & ~CLOSE_FLAG
        return f'close({bits >> ROUND_BITS},{bits & ((1 << ROUND_BITS) - 1)})'
    return f'epoch({raw})'


def parse_line(line):
    """('opened', fields) or ('signed', fields, pairs) or None"""
    parts = line.split()
    if not parts:
        return None
    # name=value fields first, then root:hash pairs (hex, no '=' in them)
    fields = dict(p.split('=', 1) for p in parts[1:] if '=' in p)
    pairs = [tuple(p.split(':', 1)) for p in parts[1:] if '=' not in p]
    if parts[0] == 'OPENED':
        return 'opened', fields, []
    if parts[0] == 'SIGNED':
        return 'signed', fields, pairs
    return None


def audit(logs):
    """logs: {node name: iterable of lines}. Returns the audit summary."""
    # (family, voter, instance, root) -> {hash: (node, lifetime)}
    seen = defaultdict(dict)
    nodes = {}
    for node, lines in sorted(logs.items()):
        lifetime, signed, hashes, torn = 0, 0, 0, 0
        for line in lines:
            if not line.endswith('\n'):
                # A line cut off by the kill: it never reached the network
                # whole, since the vote is sent after the line is written
                torn += 1
                continue
            parsed = parse_line(line)
            if parsed is None:
                continue
            kind, fields, pairs = parsed
            if kind == 'opened':
                lifetime += 1
                continue
            signed += 1
            hashes += len(pairs)
            vote_kind = fields['kind']
            family = 'first' if vote_kind in FIRST_KINDS else 'final' if vote_kind in FINAL_KINDS else None
            if family is None:
                continue
            where = instance(fields['epoch'])
            for root, hash_ in pairs:
                seen[(family, fields['voter'], where, root)].setdefault(hash_, (node, lifetime))
        nodes[node] = dict(lifetimes=lifetime, votes=signed, hashes=hashes, torn_lines=torn)
    conflicts = []
    for (family, voter, where, root), signers in seen.items():
        if len(signers) > 1:
            lifetimes = sorted({life for _, life in signers.values()})
            conflicts.append(dict(family=family, voter=voter, instance=where, root=root,
                                  hashes=sorted(signers), nodes=sorted({n for n, _ in signers.values()}),
                                  lifetimes=lifetimes, across_restart=len(lifetimes) > 1))
    conflicts.sort(key=lambda c: (c['voter'], c['instance'], c['root']))
    return dict(nodes=nodes, slots_checked=len(seen), conflicts=len(conflicts),
                across_restart=sum(c['across_restart'] for c in conflicts),
                examples=conflicts[:20], all_conflicts=conflicts)


def read_logs(data_dir):
    logs = {}
    for path in sorted(Path(data_dir).glob('pr*/signed-votes.log')):
        with path.open() as f:
            logs[path.parent.name] = f.readlines()
    return logs


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('data_dir', type=Path)
    parser.add_argument('--json', type=Path, help='write the full audit, every conflict included')
    args = parser.parse_args()
    logs = read_logs(args.data_dir)
    if not logs:
        print(f'no pr*/signed-votes.log under {args.data_dir}: was the run started with --audit-votes?')
        return 2
    result = audit(logs)
    if args.json:
        args.json.write_text(json.dumps(result, indent=2) + '\n')
    summary = {k: v for k, v in result.items() if k != 'all_conflicts'}
    print('VOTE_AUDIT ' + json.dumps(summary))
    return 1 if result['conflicts'] else 0


if __name__ == '__main__':
    raise SystemExit(main())
