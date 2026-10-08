#!/usr/bin/env sh
# Builds main.pdf (the DSN 2027 submission) and supplement.pdf.
# With TeX Live: pdflatex + bibtex.  Without it: tectonic (IEEEtran and the
# Times fonts are in tectonic's bundle, so the page count is authoritative).
set -eu
cd "$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
if command -v tectonic >/dev/null 2>&1 && ! command -v pdflatex >/dev/null 2>&1; then
  for document in main supplement; do
    tectonic -X compile "$document.tex"
  done
  exit 0
fi
if ! command -v pdflatex >/dev/null 2>&1; then
  echo "pdflatex or tectonic is required." >&2
  exit 1
fi
for document in main supplement; do
  pdflatex -interaction=nonstopmode -halt-on-error "$document.tex"
  bibtex "$document"
  pdflatex -interaction=nonstopmode -halt-on-error "$document.tex"
  pdflatex -interaction=nonstopmode -halt-on-error "$document.tex"
done
