#!/usr/bin/env python3
"""Generate sections/eval-table.tex from a matrix directory of run_gate_b.py summaries."""
import json, sys
from pathlib import Path

VARIANTS = [
    ('nofork', 'No forks'), ('nofork-offline1', 'No forks, 1 offline'), ('nofork-byz1', 'No forks, 1 Byzantine'),
    ('fork5', '5\\% forks'), ('fork5-offline1', '5\\% forks, 1 offline'), ('fork5-byz1', '5\\% forks, 1 Byzantine'),
    ('fork10', '10\\% forks'), ('fork10-offline1', '10\\% forks, 1 offline'), ('fork10-byz1', '10\\% forks, 1 Byzantine'),
]
# RsNano develop baseline, earlier pass (unchanged code); see the paper text.
BASELINE = {
    'nofork': (1901, 189, 410, 622), 'nofork-offline1': (1907, 207, 238, 285), 'nofork-byz1': (1921, 205, 234, 289),
    'fork5': '95.4', 'fork5-offline1': '90.4', 'fork5-byz1': '93.0',
    'fork10': '87.3', 'fork10-offline1': '69.2', 'fork10-byz1': '86.4',
}

def main(matrix, out):
    rows = []
    for key, label in VARIANTS:
        d = json.load(open(Path(matrix) / f'{key}.summary.json'))
        c = d['client']
        cp = len(d['settlement']['decided_everywhere'])
        assert d['settlement']['settled'], key
        base = BASELINE[key]
        if isinstance(base, tuple):
            left = ' & '.join(str(x) for x in base)
        else:
            left = '\\multicolumn{4}{c}{did not finish: %s\\%% confirmed}' % base
        n = lambda x: f'{int(round(x)):,}'.replace(',', '{,}')
        rows.append(f"{label} & {left} & {n(c['nonfork_goodput_cps'])} & {c['nonfork_p50_ms']} & {c['nonfork_p95_ms']} & {c['nonfork_p99_ms']} & {cp}\\\\")
        if key in ('nofork-byz1', 'fork5-byz1'):
            rows.append('\\midrule')
    body = '\n'.join(rows)
    Path(out).write_text(r'''\begin{table*}[t]
\caption{Non-fork goodput (blocks/s) and non-fork confirmation latency (ms) at an offered 2{,}000 blocks/s, one run per variant. With forks the non-fork share of the offered load is 95\% or 90\%. CP is the number of checkpoints every running validator decided identically. A baseline run that did not finish is given by the share of the 45{,}000 blocks confirmed when its publishing deadline expired.}
\label{tab:eval}
\centering
\begin{tabular}{@{}lrrrrrrrrc@{}}
\toprule
& \multicolumn{4}{c}{RsNano \texttt{develop} (no checkpoints)} & \multicolumn{5}{c}{RAI prototype (8\,s epochs)}\\
\cmidrule(lr){2-5}\cmidrule(l){6-10}
Variant & Blocks/s & p50 & p95 & p99 & Blocks/s & p50 & p95 & p99 & CP\\
\midrule
''' + body + r'''
\bottomrule
\end{tabular}
\end{table*}
''')
    print(body)

if __name__ == '__main__':
    main(sys.argv[1], sys.argv[2])
