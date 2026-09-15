import argparse
from datetime import datetime
import json
from pathlib import Path
import re

base = Path(__file__).resolve().parent


def workload_anchor(folder):
    log_path = folder / 'run.log'
    if not log_path.exists():
        return None, None
    text = re.sub(r'\x1b\[[0-9;]*m', '', log_path.read_text())
    schedule = re.search(r'EPOCH_SCHEDULE (\{[^\n]+)', text)
    if schedule:
        return json.loads(schedule.group(1))['start_unix_ms'], 'epoch_schedule'
    started = re.search(r'(\d{4}-\d\d-\d\dT[0-9:.]+Z).*Starting with ', text)
    if started:
        return datetime.fromisoformat(started.group(1).replace('Z', '+00:00')).timestamp() * 1000, 'starting_with_log'
    return None, None


def comparison_summary(name):
    """Summarize a completed run, or report pending results without failing."""
    folder = base / name
    result_path = folder / 'results.json'
    metadata_path = folder / 'metadata.json'
    result = json.loads(result_path.read_text()) if result_path.exists() else {}
    metadata = json.loads(metadata_path.read_text()) if metadata_path.exists() else {}
    command = metadata.get('command', [])
    delay_ms = (
        int(command[command.index('--vote-generator-delay-ms') + 1])
        if '--vote-generator-delay-ms' in command else None
    )
    summary = {
        'run': name,
        'state': 'complete' if result_path.exists() and metadata_path.exists() else 'pending',
        'profile': metadata.get('profile'),
        'block_processor_threads': metadata.get('block_processor_threads'),
        'vote_processor_threads': metadata.get('vote_processor_threads'),
        'vote_generator_delay_ms': delay_ms,
        'epochs_enabled': bool(result.get('BENCHMARK_RESULT', {}).get('epoch_terminated_elections')
                               or result.get('BENCHMARK_RESULT', {}).get('epoch_length')) if result else None,
        'exit_code': metadata.get('exit_code'),
        'data_deleted': metadata.get('data_deleted'),
        'two_closes_agree': result.get('EPOCH_CLOSE_RESULT', {}).get('success', False)
        and result.get('EPOCH_CLOSE_RESULT', {}).get('epochs_checked') == 2,
    }
    if summary['state'] == 'pending' or summary['epochs_enabled'] is False:
        summary['two_closes_agree'] = None
    perf = result.get('EPOCH_PERFORMANCE_RESULT', {})
    epochs = {
        row['epoch']: row['finalized']
        for row in perf.get('rows', []) if not row['forked']
    }
    summary['nonfork_finalized_by_epoch'] = epochs
    if all(epochs.get(e, {}).get('mean_ms') for e in (0, 1)):
        summary['epoch1_over_epoch0_mean'] = epochs[1]['mean_ms'] / epochs[0]['mean_ms']
    if 'outcome_windows' in perf:
        windows = {}
        for row in perf['outcome_windows']:
            if not row['forked']:
                start = row['outcome_start_seconds']
                windows[start] = windows.get(start, 0) + row['finalization']['count']
        summary['nonfork_finalized_per_second'] = {
            '5_10s': windows.get(5, 0) / 5,
            '15_20s': windows.get(15, 0) / 5,
            '20_25s': windows.get(20, 0) / 5,
            '15_25s': (windows.get(15, 0) + windows.get(20, 0)) / 10,
        }

    anchor, anchor_basis = workload_anchor(folder)
    summary['workload_anchor_basis'] = anchor_basis
    cpu_samples = []
    epoch_changes = []
    last_state = None
    sample_path = folder / 'samples.jsonl'
    lines = sample_path.read_text().splitlines() if sample_path.exists() else []
    for index, line in enumerate(lines):
        try:
            sample = json.loads(line)
        except json.JSONDecodeError:
            if index == len(lines) - 1:
                continue  # An ongoing run may be writing its last sample.
            raise
        if anchor is None:
            continue
        offset = (sample['unix_ms'] - anchor) / 1000
        state = {k: v for k, v in sample.get('counts', {}).items() if 'epoch' in k}
        if state and state != last_state:
            epoch_changes.append({'seconds_after_schedule': offset, 'state': state})
            last_state = state
        if not sample.get('node_cpu'):
            continue
        cpu = sum(float(row.split()[1]) for row in sample['node_cpu'].splitlines() if row.strip())
        cpu_samples.append((offset, cpu))
    summary['pr0_epoch_state_changes'] = epoch_changes
    summary['six_node_ps_cpu_percent_sample_mean'] = {}
    for start, end in ((5, 10), (15, 25)):
        values = [cpu for t, cpu in cpu_samples if start <= t < end]
        summary['six_node_ps_cpu_percent_sample_mean'][f'{start}_{end}s'] = (
            sum(values) / len(values) if values else None
        )
    # Curated artifacts retain derived observations without multi-megabyte logs.
    saved_path = folder / 'summary.json'
    if not sample_path.exists() and saved_path.exists():
        saved = json.loads(saved_path.read_text())
        for key in ('pr0_epoch_state_changes', 'six_node_ps_cpu_percent_sample_mean', 'workload_anchor_basis'):
            if key in saved:
                summary[key] = saved[key]
    return summary


def compare_runs(*names):
    """Compare receipt-window rates and latency; profile runs stay labelled."""
    runs = [comparison_summary(name) for name in names]
    changes = []
    if runs:
        reference = runs[0]
        for candidate in runs[1:]:
            change = {'run': candidate['run'], 'reference': reference['run']}
            for epoch in (0, 1):
                old = reference['nonfork_finalized_by_epoch'].get(epoch, {})
                new = candidate['nonfork_finalized_by_epoch'].get(epoch, {})
                for metric in ('mean_ms', 'p95_ms'):
                    if old.get(metric) and new.get(metric) is not None:
                        change[f'epoch{epoch}_{metric}_percent'] = 100 * (new[metric] / old[metric] - 1)
            old = reference.get('nonfork_finalized_per_second', {}).get('15_25s')
            new = candidate.get('nonfork_finalized_per_second', {}).get('15_25s')
            if old and new is not None:
                change['15_25s_finalization_rate_percent'] = 100 * (new / old - 1)
            changes.append(change)
    return {
        'runs': runs,
        'change_from_first': changes,
        'notes': [
            'Receipt windows use first publication; CPU windows use the epoch schedule anchor, or the Starting with log timestamp for no-epoch controls.',
            'CPU is the sum of six ps %cpu readings, sampled about every two seconds; it is not instantaneous utilization.',
            'Profile runs are diagnostic and should not establish performance improvements.',
            'Missing outcome groups count as zero; partial publication-tail windows are excluded.',
        ],
    }


def print_run(name):
    folder = base / name
    result = json.loads((folder/'results.json').read_text())
    print('\nRUN', name)
    print('cleanup/status', json.loads((folder/'metadata.json').read_text()))
    perf = result.get('EPOCH_PERFORMANCE_RESULT', {})
    print('close', result.get('EPOCH_CLOSE_RESULT'))
    for row in perf.get('rows', []):
        print('epoch', row['epoch'], 'fork', row['forked'], 'finalized', row['finalized'])
    print('Fresh nonfork publication cohorts: epoch, start, count, mean, p95, FIRST mean')
    for row in perf.get('publication_windows', []):
        if row['forked'] or row['prior_epoch_outcome']:
            continue
        f = row['finalization']
        print(row['epoch'], row['publication_start_seconds'], f['count'],
              round(f['mean_ms'] or 0, 1), round(f['p95_ms'] or 0, 1),
              round(row['first_to_finalization']['mean_ms'] or 0, 1))
    windows = {}
    for row in perf.get('outcome_windows', []):
        if row['forked']:
            continue
        b = windows.setdefault(row['outcome_start_seconds'], {})
        b[row['epoch']] = b.get(row['epoch'], 0) + row['finalization']['count']
    print('Nonfork receive-window rates: start, total/s, by epoch/s')
    for start in range(0, max(windows, default=0)+1, 5):
        epochs = windows.get(start, {})
        print(start, sum(epochs.values())/5, {e:n/5 for e,n in epochs.items()})
    print('PR0 observed epoch-state changes (seconds after workload anchor)')
    summary = comparison_summary(name)
    for observed in summary['pr0_epoch_state_changes']:
        print(round(observed['seconds_after_schedule'], 2), observed['state'])
    print('Six-node ps CPU sample means', summary['six_node_ps_cpu_percent_sample_mean'])


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description='Compare PR0 non-fork epoch finalization metrics.')
    parser.add_argument('names', nargs='+')
    parser.add_argument('--compare', action='store_true')
    parser.add_argument('--input-dir', type=Path, default=base)
    args = parser.parse_args()
    base = args.input_dir.resolve()
    if args.compare:
        print(json.dumps(compare_runs(*args.names), indent=2))
    else:
        for name in args.names:
            print_run(name)
