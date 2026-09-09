import json,pathlib
base=pathlib.Path(__file__).resolve().parent
names=['baseline','timeout-scheduler','batched-notifications','delay20','delay50','repeat100']
rows=[]
for name in names:
 p=base/name
 if not (p/'latency.json').exists(): continue
 s=json.loads((p/'summary.json').read_text());a=json.loads((p/'latency.json').read_text())['prs'][0]
 rows.append(dict(run=name, counts=a['counts'], latencies=a['latencies'],
                  window_agreement=s['PERFORMANCE_WINDOW_TERMINATION_RESULT']['success'],
                  final_agreement=s['TERMINATION_RESULT']['success'],
                  recovery_seconds=s['CANONICAL_AGREEMENT_RESULT']['additional_observation_seconds'],
                  benchmark=s['BENCHMARK_RESULT']))
(base/'comparison.json').write_text(json.dumps(rows,indent=2)+'\n')
lines=['# RAI latency optimization — 100 ms batching retained', '',
'Workload: six PRs, no priority workload, 15,000 accounts and blocks, 1,500 blocks/s, 7,500-block epochs, 5% random forks. Each run starts with fresh node/account data; all data directories and process groups are removed afterward.', '',
'The retained changes batch second-look notifications so representative keys are loaded once per tick, skip certificate/audit processing for rejected replay votes, and give newly eligible timeout votes a separate retry record. Repeated timeout votes remain rate limited. The node batching default remains **100 ms**, as requested. No quorum, signing-lock, finalization, or termination rules were relaxed.', '',
'## Measurements', '',
'PR0 local earliest election insertion → first local finalization for nonforks; insertion → first verified block or timeout notarization for forks. Times span epochs and are cut off at the end of the 60-second performance window. Means exclude missing outcomes; their counts are shown explicitly. Fork finalization is not an optimization target. Per-PR distributions, p50/p95/p99/max, and per-epoch distributions are in each run’s `latency.json`.', '',
'| Run | Nonfork finalized / total | Nonfork mean / p95 / p99 ms | Fork terminated / total | Fork mean / p95 / p99 ms | All-PR agreement at cutoff | Extra recovery s |',
'|---|---:|---:|---:|---:|---|---:|']
for r in rows:
 c=r['counts'];n=r['latencies']['nonfork/finalization'];f=r['latencies']['fork/termination']
 dist=lambda x:'/'.join(f'{x[k]:.1f}' for k in ['mean_ms','p95_ms','p99_ms'])
 lines.append(f"| {r['run']} | {n['samples']} / {c['nonfork/roots']} | {dist(n)} | {f['samples']} / {c['fork/roots']} | {dist(f)} | {'yes' if r['window_agreement'] else 'no'} | {r['recovery_seconds']:.3f} |")
lines += ['', 'The timeout-scheduler run included a three-second CPU sample and is diagnostic, not a clean performance comparison. The 20 ms trial eventually reached agreement but needed additional recovery; it is not the retained configuration. Fork placement and scheduling vary between runs.', '', '## Epoch results at 100 ms', '',
'Rows group completed outcomes by the epoch of their first local outcome. The clock starts at the root’s earliest insertion, including any earlier epoch. These are local timing cohorts; canonical certificate agreement is checked separately across every PR.', '',
'| Run | Epoch | Nonfork finalized samples | Nonfork mean / p95 ms | Fork terminated samples | Fork mean / p95 ms |',
'|---|---:|---:|---:|---:|---:|']
for r in rows:
 if r['run'] not in ['baseline','batched-notifications','repeat100']:continue
 for e in range(2):
  n=r['latencies'].get(f'epoch{e}/nonfork/finalization',{});f=r['latencies'].get(f'epoch{e}/fork/termination',{})
  lines.append(f"| {r['run']} | {e} | {n.get('samples',0)} | {n.get('mean_ms',0):.1f} / {n.get('p95_ms',0):.1f} | {f.get('samples',0)} | {f.get('mean_ms',0):.1f} / {f.get('p95_ms',0):.1f} |")
lines += ['', '## Bottleneck evidence', '',
'The sampled voter thread spent time fetching/decrypting wallet keys for individual second-look notifications, even when the root did not need a second-look vote. Notification draining is bounded at 64 roots per tick; previously that meant up to 64 key loads per tick. The batched implementation performs one load for the entire nonempty batch and releases the signing-state lock before enqueueing selected candidates.', '',
'The sample also shows substantial shared-lock waiting in vote-processing and vote-generation threads. RAI replay votes do not change tallies, but previously still ran certificate/audit and admission processing under the AEC write lock. They now return their per-block replay result and continue to the next hash without that work. Signature verification and new-vote processing remain intact.', '',
'On PR0, the instrumented baseline recorded 1,003 vote-processor overfills, 26,723 outgoing vote-message drops, and 41,984 outgoing block-message drops. The first optimized 100 ms run had zero vote-processor overfills, 13,141 outgoing vote-message drops, and 29,833 outgoing block-message drops. Queue pressure remains, particularly around epoch transition; this is not a claim that all bottlenecks have been eliminated.', '',
'## Reporting overhead and limits', '',
'The termination audit records timestamps and deduplicates events under the election lock, so instrumentation has a cost. The baseline and unprofiled optimized runs use the same event instrumentation, periodic RPC telemetry, and progress logging. Full audit download happens after the performance cutoff. The CPU sample was enabled only for the explicitly marked diagnostic run. Absolute no-audit latency has not been measured; the results are for this instrumented six-node local workload, not a production-network latency guarantee.', '',
'Every root must terminate with canonical all-PR agreement for the runner to pass. This does not mean every nonfork root finalized: nonfork roots with only notarization at cutoff remain visible in the finalized/total column. Timeout certificates are never counted as nonfork finalization.', '',
'## Validation and reproduction', '',
'632 RAI node unit tests, 597 legacy node unit tests, 3 RAI recovery integration tests, and 33 nanospam tests passed (one nanospam test ignored). Formatting and whitespace checks passed. Each run retains its executable hashes, command, outcome summary, compressed audit, telemetry, and cleanup record.', '',
'```sh', 'python3 workload-results/rai-latency/run.py 15000 1500 my-run 100', 'python3 workload-results/rai-latency/analyze.py workload-results/rai-latency/my-run', '```', '',
'The runner requires permission to open local TCP/WebSocket channels. Its optional fourth argument sets `--vote-generator-delay-ms`; omitting it uses the unchanged 100 ms node default. The script exits nonzero if the underlying run fails or any workload root remains pending or inconsistent at the final check.', '']
selected = [r for r in rows if r['run'] in ['batched-notifications','repeat100']]
if len(selected) == 2:
    original = next(r for r in rows if r['run'] == 'baseline')
    summary_lines = ['', 'Across the two unprofiled optimized 100 ms runs (sample-weighted PR0 means):', '']
    for label, key in [('Nonfork finalization','nonfork/finalization'),('Fork termination','fork/termination')]:
        values = [r['latencies'][key] for r in selected]
        mean = sum(v['samples'] * v['mean_ms'] for v in values) / sum(v['samples'] for v in values)
        before = original['latencies'][key]['mean_ms']
        summary_lines.append(f'- {label}: {before:.1f} → {mean:.1f} ms ({100*(1-mean/before):.1f}% lower).')
    summary_lines += ['', 'Both selected runs reached canonical agreement for all 15,000 roots within the performance window, with no conflicting finalizations. Nonfork roots lacking finalization at cutoff numbered 33 and 91, versus 124 in the baseline. These missing finalizations are not included in the latency means.', '']
    lines[5:5] = summary_lines
(base/'report.md').write_text('\n'.join(lines))
print('\n'.join(lines[8:18]))
