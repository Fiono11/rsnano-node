# RAI with f = p = 1 on the Rule 2 build

RAI and pause on 205eeb3d3 (the present Rule 2: settled first votes only, threshold pass from the highest position down, keep pass from the lowest up); `rule2.patch` is that change against b852b4945. Nano (`develop*`) copied unchanged from rai-paper-matrix-final-v2-2026-10-19, which used the same Nano binary. 2000 blocks/s, 48k blocks, 8 s epochs, 3 reps per cell, same seeds as before (`tools/rai/run_sweep.py`).

- `--set final`, default committee (f = p = 1, fast path on).
- Every run settled and resolved every fork. The fork10 Byzantine cell has 3 reps (the previous matrix pooled 8).
