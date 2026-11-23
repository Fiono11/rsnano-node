#!/usr/bin/env bash
# Builds the anonymized extended version for double-blind review:
#   paper/     submitted paper and extended version (proofs, models, notes)
#   tla/       TLA+ models, TLC configurations and outputs
#   source/    prototype and harness (tracked files, no git history)
#   records/   the run records the paper and the extended version cite
# Usage: RAI_LaTeX/tools/make_anon_bundle.sh <out dir>   (run from the repo root)
# Build main.pdf and supplement.pdf first. The script stops if an identity term
# from IDENTITY_TERMS (one per line, not committed) is found in the bundle.
set -euo pipefail
out=${1:?usage: make_anon_bundle.sh <out dir>}
root=$(git rev-parse --show-toplevel)
cd "$root"
[ -e "$out" ] && { echo "$out exists; remove it first" >&2; exit 1; }
mkdir -p "$out"/{paper,records}

records=(
  gcp-matrix2-45k-2026-11-05
  gcp-matrix3-45k-2026-11-06
  gcp-matrix4-45k-2026-11-06
  gcp-matrix-n12-45k-2026-11-16
  rai-close-scale-onehost-2026-11-11
  rai-ownlane-rotation-byz-2026-11-02
  rai-rule2-rotation-2026-10-29
  gcp-restart-wan-fix-2026-11-23
)

cp RAI_LaTeX/main.pdf "$out/paper/RAI.pdf"
cp RAI_LaTeX/supplement.pdf "$out/paper/RAI-extended.pdf"

# Tracked files only: no build output, caches, local notes or history.
git ls-files -z RAI_LaTeX/tla | (cd "$root" && xargs -0 tar -cf -) | tar -xf - -C "$out"
mv "$out/RAI_LaTeX/tla" "$out/tla" && rmdir "$out/RAI_LaTeX"

git ls-files -z | grep -zvE '^(doc/benchmarks/|doc/RAI-|doc/phase1-|RAI_LaTeX/|\.claude/|\.github/)' \
  | grep -zvE '(^|/)__pycache__/|\.pyc$' \
  | xargs -0 tar -cf - | (mkdir -p "$out/source" && tar -xf - -C "$out/source")

for r in "${records[@]}"; do
  git ls-files -z "doc/benchmarks/$r" | xargs -0 tar -cf - | tar -xf - -C "$out"
  mv "$out/doc/benchmarks/$r" "$out/records/$r"
done
rm -rf "$out/doc"

# Redact the operators' user names and local paths in the copied records:
# controller and node accounts, ps output, local job directories.
grep -rIliE 'fiono|ruimorais' "$out" | while IFS= read -r f; do
  perl -pi -e 's/fiono[0-9]*\+?/anon/gi; s/ruimorais/anon/gi' "$f"
done

cat > "$out/README.md" <<'EOF'
# RAI: extended version (anonymized for review)

- `paper/RAI.pdf`: the submitted paper.
- `paper/RAI-extended.pdf`: full proofs, the weighted-committee argument, the
  model-checking runs (Tables II and III), implementation notes and departures,
  and every evaluation appendix, including twelve validators across six regions.
- `tla/`: TLA+ modules, TLC configurations, `run.sh`, and the TLC outputs with
  the violating traces. See `tla/README.md`.
- `source/`: the prototype (Rust, a fork of the public RsNano node) and the
  benchmark harness (`source/tools/rai`, `source/tools/nanospam`). Build with
  `cargo build --release -p rsnano_cli -p nanospam --features rsnano_cli/rai_protocol`.
- `records/`: per-run results (`result.json`, summaries, hosts) of the
  evaluation matrices cited in the paper and the extended version.
EOF

# Identity scan: local paths always; names, accounts and project ids from a
# private list so they never enter the repository.
status=0
if grep -rIlE '/Users/[^a[]' "$out" >/dev/null 2>&1; then
  echo "local paths found:" >&2; grep -rIlE '/Users/[^a[]' "$out" | head -20 >&2; status=1
fi
if [ -n "${IDENTITY_TERMS:-}" ] && [ -f "$IDENTITY_TERMS" ]; then
  while IFS= read -r term; do
    [ -z "$term" ] && continue
    hits=$(grep -rail -- "$term" "$out" || true)
    [ -n "$hits" ] && { echo "term '$term' found in:" >&2; echo "$hits" | head -20 >&2; status=1; }
  done < "$IDENTITY_TERMS"
else
  echo "warning: IDENTITY_TERMS not set; only local paths were checked" >&2
fi
du -sh "$out"
exit $status
