# Notes for the paper

Points to work into the manuscript. Remove an entry once the text carries it.

## Done (Oct 21 2026)

- Protocol changes (advantage over Nano): in the conclusion, as a property of the design.
- Proposals carry the selected reports' signed commitments: a sentence in Section VI.
- Latency claim in Section X: rewritten from the c534df908 matrices (`tools/matrix_figures.py`).
- Supplement: departures nine and ten, defects (23)-(28), reproduction commands for the new matrices.

## Done (Oct 22 2026)

- Rotation with a Byzantine member that keeps its seat: Figure 7 (six scenarios), Section X, supplement defect (29) and behaviour (28) (`run_sweep --set rotation-byz`, doc/benchmarks/rai-rotation-byz-2026-10-22; build A/B in doc/benchmarks/rai-rotbyz-fix-ab-2026-10-22).

## Open

- Behaviour (28): once closes outlast the epoch after the load, contested positions get only the 2 s window after each late install; one of ten 10%-fork Byzantine-rotation runs left 321 positions open.
