# Committee change with a Byzantine member on the Rule 2 build

RAI and pause on 205eeb3d3 (the present Rule 2: settled first votes only, threshold pass from the highest position down, keep pass from the lowest up); `rule2.patch` is that change against b852b4945. Nano (`develop*`) copied unchanged from rai-rotation-byz-2026-10-22, which used the same Nano binary. 2000 blocks/s, 48k blocks, 8 s epochs, 3 reps per cell, same seeds as before (`tools/rai/run_sweep.py`).

- `--set rotation-byz` (the Byzantine PR3 keeps its seat, PR1 and PR2 replaced), `RAI_FAST_PATH=0`.
- Every run settled. `fork10-grace-rot-byznode/r2000/rai-rep3` left 508 positions open, identically everywhere (`fork_grace.outcome = stalled`): behaviour (28) of the supplement, closes after the load outlast the epoch and the open positions withhold first votes until the late checkpoint is installed.
