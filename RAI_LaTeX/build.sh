#!/usr/bin/env sh
set -eu
cd "$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
if ! command -v pdflatex >/dev/null 2>&1; then
  echo "PDFLaTeX is required. Install TeX Live or MiKTeX." >&2
  exit 1
fi
BIB=""
for candidate in bibtex bibtex.original bibtex8; do
  if command -v "$candidate" >/dev/null 2>&1; then
    BIB="$candidate"
    break
  fi
done
for document in main supplement; do
  pdflatex -interaction=nonstopmode -halt-on-error "$document.tex"
  if [ -n "$BIB" ]; then
    "$BIB" "$document"
  elif [ ! -f "$document.bbl" ]; then
    echo "BibTeX is required because $document.bbl is missing." >&2
    exit 1
  else
    echo "BibTeX unavailable: retaining the supplied $document.bbl." >&2
  fi
  pdflatex -interaction=nonstopmode -halt-on-error "$document.tex"
  pdflatex -interaction=nonstopmode -halt-on-error "$document.tex"
done
