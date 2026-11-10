# Notes for the paper

Points to work into the manuscript. Remove an entry once the text carries it.

## Done (Oct 21 2026)

- Protocol changes (advantage over Nano): in the conclusion, as a property of the design.
- Proposals carry the selected reports' signed commitments: a sentence in Section VI.
- Latency claim in Section X: rewritten from the c534df908 matrices (`tools/matrix_figures.py`).
- Supplement: departures nine and ten, defects (23)-(28), reproduction commands for the new matrices.

## Done (Oct 22 2026)

- Rotation with a Byzantine member that keeps its seat: Figure 7 (six scenarios), Section X, supplement defect (29) and behaviour (28) (`run_sweep --set rotation-byz`, doc/benchmarks/rai-rotation-byz-2026-10-22; build A/B in doc/benchmarks/rai-rotbyz-fix-ab-2026-10-22).

## Done (Nov 8 2026)

- Section X, Figures 4-7, abstract, contributions, conclusion and supplement read the merged matrix: gcp-matrix4 (11 publish-gap reruns) over gcp-matrix3 (voter-lock build, equal weights) over gcp-matrix2 (Nano's cells without the change). The four-vote Nano handover text, the laggard limitation and the request-cap build split are gone; the reduced offered rate for pause is stated.
- The committee change has six-region data (Figure 7); only restarts and handoff stay one host.

## Open

- Restart and handoff runs have no six-region data: the restarts paragraph is labelled as one host (doc/benchmarks/rai-ownlane-rotation-byz-2026-11-02, rai-rule2-rotation-2026-10-29).
- No six-region run at f=1, p=0: the comparison with Nano is not at equal quorum (RAI's normal path needs four votes, Nano's five); Section X says so.

## Done (Nov 10 2026)

- Drop-ins of Nov 9 merged into the sources: new title and abstract, invariants and Table I (rule, invariant, counterexample) in Section III, threat-model paragraphs in Section IV, corrected Fig. 3, the R9 run as Fig. 4, shortened Sections VIII and IX (the TLC table moved out, Table I carries the counterexamples), the pause model in Section X, the compact related work with Table III, `rai-additions.bib`, and a bibliography in the supplement. The built `main.pdf` has the same text as the fitted PDF of Nov 9 (11 pages of body; page 12 holds AI Tool Usage and the references).

## Open before submission

- Every `\fillin` box: the tool versions, section list and figure numbers in AI Tool Usage (`main.tex`), and the new member's catch-up time in Section IV.
- Abstract and conclusion say "across six regions ... 6.7 to 38 times"; that range is the seven-region committee change, the six-region cells give 9 to 18.
- Fig. 3: V4 reports A as voted but first-votes its child B.
- References [17] and [18] print no arXiv number; [5] has no year.
- The supplement was not revised for the new numbering of the main paper's tables and figures.
