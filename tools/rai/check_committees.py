#!/usr/bin/env python3
"""Check the committees of a run_gate_b.py result: every running node must
hold the same committee (digest) for every epoch any of them derived, and
print each committee's weights, so a reconfiguration shows from one epoch to
the next.

Exit status 0 when the committees agree, 1 otherwise."""
import argparse
import json
from pathlib import Path


def committees_by_epoch(state):
    return {c['derived_by']: c for c in state.get('committees', [])}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('result', type=Path, help='result.json of a run_gate_b.py run')
    args = parser.parse_args()
    result = json.loads(args.result.read_text())
    nodes = [committees_by_epoch(state) for state in result['final_states']]
    epochs = sorted({e for node in nodes for e in node},
                    key=lambda e: -1 if e == 'genesis' else int(e))
    representatives = sorted({w['representative'] for node in nodes for c in node.values() for w in c['weights']})
    print('derived_by  nodes  digests  online  ' + '  '.join(r[:10] for r in representatives))
    consistent = True
    for epoch in epochs:
        held = [node[epoch] for node in nodes if epoch in node]
        digests = {c['digest'] for c in held}
        consistent &= len(digests) == 1
        weights = {w['representative']: w['weight'] for w in held[0]['weights']}
        print(f'{epoch:>10}  {len(held)}/{len(nodes)}  {len(digests):>7}  {held[0]["online"]:>6}  '
              + '  '.join(f'{weights.get(r, "-"):>10}' for r in representatives))
    print('COMMITTEES_CONSISTENT' if consistent else 'COMMITTEES_DIVERGED')
    return 0 if consistent else 1


if __name__ == '__main__':
    raise SystemExit(main())
