#!/usr/bin/env python3
"""Split PR0 epoch-1 observations around persisted-close and cleanup markers.

This analyzer requires timeline schema 1 and explicit PR-to-PID metadata.
Publication cohorts and receipt rates answer different questions. Rates covering
quiet drain time are observation rates, never inferred steady-state capacity.
"""
import argparse
from collections import defaultdict
import gzip
import json
import math
from pathlib import Path
import re

PHASES = ('before_closing', 'during_closing', 'post_persist_cleanup', 'after_complete')
UNCERTAIN = 'boundary_uncertain'
COHORTS = ('fresh', 'carried')
MARKERS = ('EPOCH_COUNT_REACHED', 'EPOCH_CLOSED', 'EPOCH_CLOSE_COMPLETE')


def timing(values):
    values = sorted(value / 1000 for value in values if value is not None)
    return {'count': len(values), 'mean_ms': sum(values) / len(values) if values else None,
            'p95_ms': values[math.ceil(len(values) * .95) - 1] if values else None,
            'max_ms': values[-1] if values else None}


def parse_markers(text):
    """Extract short structured records even when neighboring stderr interleaves."""
    decoder = json.JSONDecoder()
    records, malformed = [], []
    for match in re.finditer(r'\b(EPOCH_COUNT_REACHED|EPOCH_CLOSED|EPOCH_CLOSE_COMPLETE)\s+(?=\{)', text):
        try:
            record, _ = decoder.raw_decode(text[match.end():])
        except json.JSONDecodeError:
            malformed.append(match.group(1))
            continue
        if all(key in record for key in ('pid', 'epoch', 'unix_us')):
            records.append({'kind': match.group(1), **record})
    return records, malformed


def analyze_timeline(timeline, markers, pid, epoch=1, boundary_scope='pr0_local'):
    if timeline.get('schema_version') != 1 or timeline.get('unit') != 'microseconds':
        raise ValueError('Requires timeline schema_version=1 and microsecond offsets')
    if timeline.get('origin') != 'first_publication':
        raise ValueError('Unsupported timeline origin')
    low = timeline.get('first_publication_unix_us_lower_bound')
    high = timeline.get('first_publication_unix_us_upper_bound')
    cutoff = timeline.get('observation_cutoff_offset_us')
    if low is None or high is None or cutoff is None:
        raise ValueError('Timeline lacks publication clock anchor or observation cutoff')
    if low > high or cutoff < 0:
        raise ValueError('Invalid clock anchor or observation cutoff')
    rounding = timeline.get('offset_rounding_error_us', 1)
    if rounding < 0:
        raise ValueError('Negative offset rounding uncertainty')
    midpoint = (low + high) / 2
    publications = timeline['publications']
    for offset, forked in publications:
        if offset < 0 or offset > cutoff or not isinstance(forked, bool):
            raise ValueError('Invalid publication row')
    if not publications:
        raise ValueError('No publications')

    selected = {}
    for kind in MARKERS:
        values = sorted({record['unix_us'] for record in markers
                         if record['kind'] == kind and record['pid'] == pid
                         and record['epoch'] == epoch - 1})
        if len(values) != 1:
            raise ValueError(f'Expected one {kind} marker for PR0 PID {pid}, epoch {epoch - 1}; got {len(values)}')
        selected[kind] = values[0]
    reached, closed, complete = (selected[kind] for kind in MARKERS)
    if not reached <= closed <= complete:
        raise ValueError('Close markers are out of order')
    boundaries = (-math.inf, reached, closed, complete, math.inf)

    def interval(offset):
        return low + offset, high + offset + rounding

    def classify_range(lower, upper):
        for index, phase in enumerate(PHASES):
            if lower >= boundaries[index] and upper < boundaries[index + 1]:
                return phase
        return UNCERTAIN

    def classify(offset):
        return classify_range(*interval(offset))

    durations = {}
    for index, phase in enumerate(PHASES):
        start = min(midpoint + cutoff, max(midpoint, boundaries[index]))
        end = max(midpoint, min(midpoint + cutoff, boundaries[index + 1]))
        durations[phase] = {'start_offset_us_estimate': start - midpoint,
                            'end_offset_us_estimate': end - midpoint,
                            'observed_duration_seconds': max(0, end - start) / 1_000_000}

    # Duplicate identical rows cannot inflate counts. Conflicting duplicate rows
    # indicate a malformed export, rather than a second event to count.
    outcomes = {}
    earliest = {}
    for row in timeline['outcomes']:
        if len(row) != 6:
            raise ValueError('Outcome row must contain six columns')
        index, observed_epoch, terminated, finalized, first_term, first_final = row
        if not isinstance(index, int) or not 0 <= index < len(publications):
            raise ValueError('Invalid publication index')
        published = publications[index][0]
        for offset in (terminated, finalized):
            if offset is not None and not published <= offset <= cutoff:
                raise ValueError('Outcome outside publication/cutoff bounds')
        if terminated is not None and finalized is not None and terminated > finalized:
            raise ValueError('Finalization precedes termination')
        if any(value is not None and value < 0 for value in (first_term, first_final)):
            raise ValueError('Negative first-vote duration')
        key = (index, observed_epoch)
        if key in outcomes and outcomes[key] != row:
            raise ValueError('Conflicting duplicate outcome rows')
        outcomes[key] = row
        earliest[index] = min(earliest.get(index, observed_epoch), observed_epoch)
    records = []
    for (index, observed_epoch), row in outcomes.items():
        published, forked = publications[index]
        if observed_epoch != epoch or forked:
            continue
        records.append({'index': index, 'publication': published,
                        'termination': row[2], 'finalization': row[3],
                        'first_term': row[4], 'first_final': row[5],
                        'cohort': 'carried' if earliest[index] < epoch else 'fresh',
                        'publication_phase': classify(published)})

    def summarize_rows(rows):
        return {'observed_roots': len(rows),
                'termination': timing([r['termination'] - r['publication'] for r in rows if r['termination'] is not None]),
                'finalization': timing([r['finalization'] - r['publication'] for r in rows if r['finalization'] is not None]),
                'first_to_finalization': timing([r['first_final'] for r in rows if r['finalization'] is not None])}

    cohort_result, receipt_result, crossings = {}, {}, defaultdict(list)
    for phase in (*PHASES, UNCERTAIN):
        cohort_result[phase], receipt_result[phase] = {}, {}
        for cohort in COHORTS:
            rows = [r for r in records if r['cohort'] == cohort]
            cohort_result[phase][cohort] = summarize_rows([r for r in rows if r['publication_phase'] == phase])
            received = [r for r in rows if r['finalization'] is not None and classify(r['finalization']) == phase]
            duration = durations.get(phase, {}).get('observed_duration_seconds')
            receipt_result[phase][cohort] = {
                **summarize_rows(received),
                'finalizations_per_observed_second': len(received) / duration if duration else None,
                'rate_scope': 'entire observed phase, including publication tail and drain; not steady capacity'}
    for row in records:
        if row['finalization'] is not None:
            crossings[(row['cohort'], row['publication_phase'], classify(row['finalization']))].append(row)
    crossing_result = [{'cohort': cohort, 'publication_phase': pub, 'finalization_phase': final,
                        **summarize_rows(rows)}
                       for (cohort, pub, final), rows in sorted(crossings.items())]
    pending = {}
    for label, boundary in (('persisted_close', closed), ('cleanup_complete', complete)):
        pending[label] = {}
        for cohort in COHORTS:
            before = [r for r in records if r['cohort'] == cohort and interval(r['publication'])[1] < boundary]
            after = [r for r in before if r['finalization'] is not None and interval(r['finalization'])[0] >= boundary]
            missing = [r for r in before if r['finalization'] is None and interval(cutoff)[0] >= boundary]
            pending[label][cohort] = {
                'published_before_finalized_after': summarize_rows(after),
                'published_before_without_finalization_by_cutoff': len(missing)}

    offered = {}
    for phase in (*PHASES, UNCERTAIN):
        indices = [i for i, (offset, forked) in enumerate(publications) if not forked and classify(offset) == phase]
        offered[phase] = {'nonfork_publications': len(indices),
                          'without_any_observed_outcome': sum(i not in earliest for i in indices),
                          'note': 'Offered roots include missing outcomes; their admission epoch is unknown.'}
    last_publication = max(offset for offset, _ in publications)
    post_low = max(0, low + last_publication - complete) / 1_000_000
    post_high = max(0, high + last_publication + rounding - complete) / 1_000_000
    post_count = sum(not forked and classify(offset) == 'after_complete' for offset, forked in publications)
    publication_tail = {
        'last_publication_offset_us': last_publication,
        'last_publication_minus_cleanup_complete_seconds_bounds': [post_low, post_high],
        'definitely_post_complete_nonfork_publications': post_count,
        'assessment': ('no observed publications after cleanup complete' if post_high == 0
                       else 'short publication tail; no full five-second recovery window' if post_high < 5
                       else 'inspect full publication-active windows; duration alone does not prove steady load')}

    windows = {}
    for width in (1, 5):
        width_us = width * 1_000_000
        bins = []
        for start in range(0, int(cutoff // width_us) * width_us, width_us):
            end = start + width_us
            phase = classify_range(low + start, high + end + rounding)
            row = {'start_seconds': start / 1_000_000, 'window_seconds': width, 'phase': phase,
                   'entirely_within_publication_span': end <= last_publication,
                   'nonfork_publications': sum(not forked and start <= offset < end for offset, forked in publications),
                   'cohorts': {}}
            for cohort in COHORTS:
                candidates = [r for r in records if r['cohort'] == cohort]
                published = [r for r in candidates if start <= r['publication'] < end]
                received = [r for r in candidates if r['finalization'] is not None and start <= r['finalization'] < end]
                row['cohorts'][cohort] = {'publication': summarize_rows(published),
                                          'receipt': summarize_rows(received),
                                          'finalizations_per_second': len(received) / width}
            bins.append(row)
        windows[f'{width}s'] = bins
    return {
        'epoch': epoch, 'pr': 0, 'pid': pid, 'phase_boundary_scope': boundary_scope,
        'clock_basis': {'origin': 'first_publication', 'unit': 'microseconds',
                        'unix_us_lower_bound': low, 'unix_us_upper_bound': high,
                        'offset_rounding_error_us': rounding,
                        'classification': 'Entire timestamp interval must fit one phase; overlapping boundaries remain uncertain.',
                        'duration_basis': 'Node marker wall times, clipped to first-publication anchor midpoint and observation cutoff.'},
        'marker_unix_us': selected, 'observation_cutoff_offset_us': cutoff,
        'phase_durations': durations, 'offered_publications': offered,
        'publication_cohorts': cohort_result, 'receipt_phases': receipt_result,
        'publication_to_receipt_phases': crossing_result,
        'work_crossing_boundary': pending, 'post_close_publication_tail': publication_tail,
        'full_windows': windows,
        'notes': ['EPOCH_CLOSED marks AEC close return after ledger persistence/discard, not the exact LMDB commit instruction.',
                  'EPOCH_CLOSE_COMPLETE marks remaining generator/state cleanup completion.',
                  'Latency includes transport and WebSocket delivery; receipt phase is distinct from publication phase.',
                  'Fresh means no outcome for that root was observed in an earlier epoch; carried roots are separate.',
                  'Rates over full observation phases include quiet drain time and cannot establish steady recovery capacity.',
                  'One shared-host wall-clock calibration is assumed stable for the run; clock adjustments are not independently measured.']}


def analyze_network(timeline, markers, nodes_by_pr):
    """Keep local recovery distinct from continued epoch-0 work on other PRs."""
    expected = {str(index) for index in range(6)}
    mapping = {str(pr): int(pid) for pr, pid in nodes_by_pr.items()}
    if set(mapping) != expected or len(set(mapping.values())) != 6:
        return {'status': 'unknown', 'reason': 'Requires explicit unique PID mapping for all six PRs.'}
    low = timeline['first_publication_unix_us_lower_bound']
    high = timeline['first_publication_unix_us_upper_bound']
    observed = {}
    missing = []
    for pr in sorted(expected):
        row = {'pid': mapping[pr], 'markers': {}}
        for kind in MARKERS:
            values = sorted({record['unix_us'] for record in markers
                             if record['kind'] == kind and record['pid'] == mapping[pr]
                             and record['epoch'] == 0})
            row['markers'][kind] = [
                {'unix_us': value, 'seconds_after_first_publication_bounds':
                 [(value - high) / 1_000_000, (value + 1 - low) / 1_000_000]}
                for value in values]
            if len(values) != 1:
                missing.append({'pr': pr, 'marker': kind, 'count': len(values)})
        observed[pr] = row
    if missing:
        return {'status': 'unknown', 'reason': 'Incomplete or ambiguous six-PR marker coverage.',
                'per_pr_markers': observed, 'missing_or_ambiguous': missing}
    times = {kind: [row['markers'][kind][0]['unix_us'] for row in observed.values()]
             for kind in MARKERS}
    aggregate = {MARKERS[0]: min(times[MARKERS[0]]),
                 MARKERS[1]: max(times[MARKERS[1]]),
                 MARKERS[2]: max(times[MARKERS[2]])}
    synthetic = [{'kind': kind, 'pid': mapping['0'], 'epoch': 0, 'unix_us': value}
                 for kind, value in aggregate.items()]
    analysis = analyze_timeline(timeline, synthetic, mapping['0'],
                                boundary_scope='all_six_prs_first_drain_latest_persist_latest_complete')
    return {'status': 'complete', 'per_pr_markers': observed,
            'boundary_basis': 'First drain among six PRs; latest persisted close; latest cleanup completion. Election outcomes still come only from PR0.',
            'analysis': analysis}


def analyze_run(folder):
    result_path = folder / 'results.json'
    result = json.loads(result_path.read_text() if result_path.exists()
                        else gzip.decompress((folder / 'results.json.gz').read_bytes()))
    metadata = json.loads((folder / 'metadata.json').read_text())
    timeline = result.get('EPOCH_PERFORMANCE_RESULT', {}).get('timeline')
    if timeline is None:
        return {'run': folder.name, 'status': 'unavailable',
                'reason': 'Legacy result has no exact timeline; retain approximate RPC bracket analysis.'}
    pid = metadata.get('nodes_by_pr', {}).get('0')
    if pid is None:
        return {'run': folder.name, 'status': 'unavailable', 'reason': 'No explicit PR0 PID mapping; no PID inference performed.'}
    saved_markers = folder / 'phase-markers.json'
    if saved_markers.exists():
        saved = json.loads(saved_markers.read_text())
        markers, malformed = saved['markers'], saved.get('malformed_marker_records', [])
    else:
        markers, malformed = parse_markers((folder / 'nodes.log').read_text())
    report = analyze_timeline(timeline, markers, int(pid))
    return {'run': folder.name, 'status': 'complete', 'profile': metadata.get('profile'),
            'data_deleted': metadata.get('data_deleted'), 'malformed_marker_records': malformed,
            **report, 'network': analyze_network(timeline, markers, metadata.get('nodes_by_pr', {}))}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('name')
    parser.add_argument('--input-dir', type=Path, default=Path(__file__).resolve().parent)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    result = analyze_run(args.input_dir / args.name)
    text = json.dumps(result, indent=2) + '\n'
    if args.output:
        args.output.write_text(text)
        print(f'Wrote {args.output}')
    else:
        print(text, end='')


if __name__ == '__main__':
    main()
