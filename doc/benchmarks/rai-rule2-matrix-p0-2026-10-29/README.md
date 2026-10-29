# RAI at Nano's quorum on the Rule 2 build: n = 6, f = 1, p = 0, fast path off

RAI and pause on 205eeb3d3 (the present Rule 2: settled first votes only, threshold pass from the highest position down, keep pass from the lowest up); `rule2.patch` is that change against b852b4945. Nano (`develop*`) copied unchanged from rai-paper-matrix-p0-2026-10-20, which used the same Nano binary. 2000 blocks/s, 48k blocks, 8 s epochs, 3 reps per cell, same seeds as before (`tools/rai/run_sweep.py`).

- `--set final --committee-p 0 --committee-seats 6`, `RAI_FAST_PATH=0` for every node.
- Every run settled and resolved every fork. Against the previous matrix, RAI finalizes the forks left after the load more slowly (10% forks: up to 17.0 s, was 9.3 s): see `../rai-rule2-allvotes-ab-2026-10-29`.
