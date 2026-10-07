# RAI — EuroSys 2027 manuscript sources

Research manuscript for RAI, a protocol for safe pipelined epoch transitions in account-local payment systems, with its separate supplement.

## What changed in the October 2026 revision

The paper is now organized around the transition mechanism. The convergence layer (report reconstruction) and the consensus layer (checkpoint agreement) are abstract services with stated interfaces and properties (Section 3.5: C1–C2 and A1–A4). RAI defines the reports, the eligibility predicate of checkpoint candidates, the deterministic construction, the predecessor gate and overlap exception, and installation. Safety and termination (Section 6) are proved relative to the two services. The services are instantiated only in Section 9: rateless IBLT set reconciliation and a Kudzu-style election. The evaluation table was regenerated from the October 7, 2026 matrix (`doc/benchmarks/rai-paper-2026-10-07`) with `tools/eval_table.py`.

## Files

- `main.tex` and `sections/*.tex`: main manuscript. `sections/eval-table.tex` is generated.
- `tools/eval_table.py <matrix dir> sections/eval-table.tex`: regenerates the evaluation table from `run_gate_b.py` summaries.
- `supplement.tex`: the prototype's Kudzu-style election (Appendix A), the optional predecessor-backed extension (B), and boundary examples (C).
- `preamble.tex`: ACM SIGPLAN formatting and shared notation.
- `references.bib`: bibliography source.
- `acmart.cls` and `ACM-Reference-Format.bst`: ACM template files used for the build, with their original license notices.
- `build.sh`: portable command-line build helper (needs pdflatex).

## Build locally

A reasonably complete TeX Live or MiKTeX installation is required, including the normal packages used by `acmart`, TikZ, `algorithm`, and `algpseudocode`. Run `./build.sh`, or `latexmk -pdf main.tex` and `latexmk -pdf supplement.tex`.

Without TeX Live, `tectonic` builds the manuscript with its bundled class: copy the directory elsewhere, delete `acmart.cls`, and run `tectonic -X compile main.tex`. Fonts then differ from the ACM build and the page count is not authoritative.

## Submission packaging

The main manuscript uses anonymous ACM SIGPLAN two-column formatting, nominal 10-point text on 12-point leading, US Letter, a 7 × 9 inch text block, a 0.34 inch column gap, and page numbers. The supplement is a separate file, not an appendix appended to the main submission. The manuscript has not been submitted or accepted.

## Reading copies

`main-tectonic.pdf` and `supplement-tectonic.pdf` were built with tectonic and its bundled class on October 7, 2026; their fonts and page count differ from the ACM build.
