# RAI — EuroSys 2027 manuscript sources

This package contains the rewritten research manuscript, its separate supplement, and author-facing revision notes.

## Read first

Read `REVISION_NOTES.md` before treating the rewrite as an equivalent restatement of the original protocol. It documents proposed wire-format and checkpoint-input changes, a stricter overlap rule, and the decision to keep predecessor-backed fresh work outside the main theorem scope. The proofs are not machine checked. No implementation or experimental result is claimed.

## Files

- `main.tex` and `sections/*.tex`: main manuscript.
- `supplement.tex`: Kudzu-style checkpoint election, optional extension, and boundary examples.
- `preamble.tex`: ACM SIGPLAN formatting and shared notation.
- `references.bib`: bibliography source; generated `.bbl` files are also included.
- `acmart.cls` and `ACM-Reference-Format.bst`: ACM template files used for the build, with their original license notices.
- `main.pdf` and `supplement.pdf`: compiled reading copies.
- `REVISION_NOTES.md`: editorial changes, substantive protocol amendments, research scope, and author checks.
- `build.sh`: portable command-line build helper.
- `QUALITY_CHECKS.json`: build and PDF checks, not protocol verification.

## Build locally

A reasonably complete TeX Live or MiKTeX installation is required, including the normal packages used by `acmart`, TikZ, `algorithm`, and `algpseudocode`. No font files are distributed in this package; the template uses fonts supplied by the user's TeX distribution.

Run:

```sh
./build.sh
```

Or, with a working BibTeX installation:

```sh
latexmk -pdf main.tex
latexmk -pdf supplement.tex
```

The helper also recognizes `bibtex.original` or `bibtex8` on systems whose `bibtex` launcher is unavailable. A direct PDFLaTeX build can use the included `.bbl` files when bibliography contents are unchanged.

## Edit in Overleaf

Upload the ZIP as a project. Select `main.tex` as the main document for the paper, or `supplement.tex` for the separate supplement. The source does not require shell escape or external image-generation tools; the figures are editable LaTeX/TikZ.

## Submission packaging

The main manuscript uses anonymous ACM SIGPLAN two-column formatting, nominal 10-point text on 12-point leading, US Letter, a 7 × 9 inch text block, a 0.34 inch column gap, and page numbers. Its technical material occupies pages 1–12, followed by references. The supplement is a separate file, not an appendix appended to the main submission.

The official requirements were checked on September 24, 2026 at https://2027.eurosys.org/cfp.html. Verify the live CFP and your submission eligibility before uploading. The manuscript has not been submitted or accepted, and the sources do not invent ACM publication metadata.


## Minimal redesign
This bundle adds Rule 3 checkpoint promotion: a decided checkpoint may finalize the maximal uniquely retained prefix whose blocks each have a verified closing-epoch NC. Recovery-only sole survivors remain unresolved. See `REVISION_NOTES.md`.
