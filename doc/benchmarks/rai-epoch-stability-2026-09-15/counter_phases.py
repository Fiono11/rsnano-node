"""Estimate PR0 counter rates from cumulative samples during two workload phases."""
import argparse
from bisect import bisect_left
import json
from pathlib import Path
from analyze import workload_anchor


COUNTERS = [
    'active_elections/started/in',
    'active_elections/confirmed/in',
    'active_elections/cooldown/in',
    'block_processor/enqueued/in',
    'block_processor_source/live_originator/in',
    'block_processor_source/live/in',
    'block_processor_source/forced/in',
    'block_processor_result/progress/in',
    'block_processor_result/old/in',
    'block_processor_result/fork/in',
    'block_processor_result/conflict/in',
    'vote_processor/process/in',
    'vote_processor/overfill/in',
    'election/vote/in',
    'election_vote/replayed/in',
    'election_vote/forwarded/in',
    'election/generate_vote_normal/in',
    'election/generate_vote_final/in',
    'election/confirmation_request/in',
    'election/kudzu_fast_confirmed/in',
    'election/kudzu_slow_confirmed/in',
    'vote_generator/generator_broadcasts/in',
    'vote_generator_final/generator_broadcasts/in',
    'vote_generator/generator_replies/in',
    'vote_generator_final/generator_replies/in',
    'requests/requests_generated_hashes/in',
    'requests/requests_generated_votes/in',
    'request_aggregator/request_hashes/in',
    'request_aggregator/request/in',
    'request_aggregator/channel_full/out',
    'message_processor_type/confirm_req/in',
    'message_processor_type/confirm_ack/in',
    'message_processor_type/publish/in',
    'message_processor_type/epoch_close/in',
    'vote_rebroadcaster/rebroadcast/in',
    'vote_rebroadcaster/rebroadcast_hashes/in',
    'vote_rebroadcaster/overfill/in',
    'drop/publish/out',
    'drop/confirm_ack/out',
    'ledger/rollback/in',
    'confirming_set/cemented/in',
    'confirming_set/loop/in',
]


def summarize(folder):
    anchor, anchor_basis = workload_anchor(folder)
    if anchor is None:
        raise ValueError(f'{folder.name}: no workload time anchor')
    samples = []
    for line in (folder / 'samples.jsonl').read_text().splitlines():
        row = json.loads(line)
        if not row.get('stats', {}).get('entries'):
            continue
        values = {
            '/'.join((entry['type'], entry['detail'], entry['dir'])): int(entry['value'])
            for entry in row['stats']['entries']
        }
        samples.append(((row['unix_ms'] - anchor) / 1000, values))
    times = [t for t, _ in samples]

    def interpolate(t, key):
        right = bisect_left(times, t)
        if right == len(times) or (right == 0 and times[0] != t):
            raise ValueError(f'{folder.name}: no samples bracket {t}s')
        if times[right] == t:
            return samples[right][1].get(key, 0)
        before, after = samples[right - 1], samples[right]
        weight = (t - before[0]) / (after[0] - before[0])
        return before[1].get(key, 0) * (1 - weight) + after[1].get(key, 0) * weight

    phases = {}
    for start, end in ((5, 10), (15, 25)):
        phases[f'{start}_{end}s'] = {
            key: (interpolate(end, key) - interpolate(start, key)) / (end - start)
            for key in COUNTERS
        }
        phase = phases[f'{start}_{end}s']
        phase['derived/local_vote_batches_emitted'] = sum(phase[key] for key in (
            'vote_generator/generator_broadcasts/in',
            'vote_generator_final/generator_broadcasts/in',
            'requests/requests_generated_votes/in',
        ))
    rows = []
    for key in [*COUNTERS, 'derived/local_vote_batches_emitted']:
        early, late = phases['5_10s'][key], phases['15_25s'][key]
        rows.append({
            'counter': key,
            'early_per_second': early,
            'late_per_second': late,
            'late_over_early': late / early if early else None,
            'early_per_election_started': early / phases['5_10s']['active_elections/started/in'],
            'late_per_election_started': late / phases['15_25s']['active_elections/started/in'],
        })
    return {'run': folder.name, 'workload_anchor_basis': anchor_basis, 'counters': rows}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('names', nargs='+')
    parser.add_argument('--input-dir', type=Path, default=Path(__file__).resolve().parent)
    parser.add_argument('--json-output', type=Path)
    args = parser.parse_args()
    result = {
        'method': 'Rates use cumulative-counter differences over 5–10s and 15–25s after the workload anchor (epoch schedule, or Starting with log timestamp for no-epoch controls). Endpoints are linearly interpolated between approximately two-second samples.',
        'limitations': [
            'vote_processor/process counts accepted queue entries, not completed signature verifications.',
            'election/vote counts candidate-hash vote applications; packets can carry many hashes.',
            'requests_generated_hashes counts hashes submitted to request-reply voting before authorization; it is not a count of hashes actually signed.',
            'No writer-transaction, writer-wait, or signature-verification-duration counters are present in these captures.',
            'derived/local_vote_batches_emitted sums normal/final generator_broadcasts and requests_generated_votes; it counts emitted local vote batches, not signatures verified or hashes signed.',
        ],
        'runs': [summarize(args.input_dir / name) for name in args.names],
    }
    if args.json_output:
        args.json_output.write_text(json.dumps(result, indent=2) + '\n')
    for run in result['runs']:
        print('\n' + run['run'] + ': early/s -> late/s (late/early)')
        for row in run['counters']:
            if row['early_per_second'] or row['late_per_second']:
                ratio = f"{row['late_over_early']:.2f}x" if row['late_over_early'] is not None else 'new'
                print(f"{row['counter']}: {row['early_per_second']:.1f} -> {row['late_per_second']:.1f} ({ratio})")
