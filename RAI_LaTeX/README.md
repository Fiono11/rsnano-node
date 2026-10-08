# RAI — DSN 2027 manuscript sources

Research manuscript for RAI, a protocol for safe pipelined epoch transitions in account-local payment systems, prepared for the research track of the 57th IEEE/IFIP International Conference on Dependable Systems and Networks (DSN 2027).

## Format

DSN 2027 regular papers: IEEE Computer Society two-column format, US Letter, 10-point text on 12-point leading, at most 11 pages including everything except references, double-blind, abstract of at most 150 words, paper type on the first page, an "AI Tool Usage" section after the acknowledgements that does not count toward the limit. `main.tex` uses the `IEEEtran` class in conference mode; the class is part of every TeX distribution and of tectonic's bundle and is not shipped here.

## What changed in the October 2026 DSN revision

- Reformatted from ACM SIGPLAN (EuroSys 2027 draft) to IEEEtran; abstract cut to 150 words.
- Sections 3 to 6 carry the repairs that followed the TLA+ models in `tla/`: exclusion witnesses distinct from notarization certificates, settled versus early first votes with no fast path on early votes, origin-stamped lock records with matching-origin discharge, overlap certificates, the second committed set `G_i` that keeps fresh votes for `R`-tagged blocks (Fix B), and the corresponding lemmas (signers hold the witness; the overlap verdict does not depend on installation order).
- New Section 7 reports the model checking: three models, the counterexamples to the earlier rules, and the exhaustive runs of the repaired ones.
- The evaluation table was regenerated from a nine-variant matrix on the final prototype commit (`doc/benchmarks/rai-dsn-2026-10-08`) with `tools/eval_table.py`.
- `supplement.tex` holds the full proofs, the model descriptions and results, the scripted regression cases, the prototype departures and the defects found while measuring. It is a separate PDF, since DSN counts appendices toward the page limit.

## Files

- `main.tex`, `preamble.tex`, `sections/*.tex`: the submission. `sections/eval-table.tex` is generated.
- `supplement.tex`: supplementary material (not part of the 11 pages).
- `tla/`: the TLA+ models (`RaiClose`, `RaiOverlap`, `RaiHandoff`), their TLC configurations, `run.sh`, and the TLC outputs including the violating traces. See `tla/README.md`.
- `tools/eval_table.py <matrix dir> sections/eval-table.tex`: regenerates the evaluation table from `run_gate_b.py` summaries.
- `references.bib`: bibliography source.
- `build.sh`: builds `main.pdf` and `supplement.pdf` with pdflatex or tectonic.

## Build

`./build.sh`, or `tectonic -X compile main.tex` and `tectonic -X compile supplement.tex`. Check the page count of `main.pdf`: everything before the references must fit in 11 pages.
