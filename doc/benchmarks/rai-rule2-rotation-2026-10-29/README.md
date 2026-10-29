# Committee change on the Rule 2 build

RAI and pause on 205eeb3d3 (the present Rule 2: settled first votes only, threshold pass from the highest position down, keep pass from the lowest up); `rule2.patch` is that change against b852b4945. Nano (`develop*`) copied unchanged from rai-rotation-matrix-2026-10-20, which used the same Nano binary. 2000 blocks/s, 48k blocks, 8 s epochs, 3 reps per cell, same seeds as before (`tools/rai/run_sweep.py`).

- `--set rotation` (six representatives, two standby, four seats, PR2 and PR3 replaced in epoch 0), `RAI_FAST_PATH=0`.
- Every run settled and resolved every fork.
