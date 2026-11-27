#!/bin/bash
# As fiono8199 on the controller, detached: the two arms of the backpressure A/B.
#   ~/bin-bpbase/rai = rai_dsn_anon     5d5709827 (paper branch: matrix node + catch-up fix + keep-pass fallback)
#   ~/bin-bpfix/rai  = rai_backpressure 15c7f4693 (= base + solicitation backpressure + inbound close lane)
# Each with rsnano, rsnano-byzantine and the matrix's nanospam. Log ~/runs/build_bp.log,
# status ~/runs/build_bp.status on every exit path.
mkdir -p ~/runs
exec >>"$HOME/runs/build_bp.log" 2>&1
set -x
status=~/runs/build_bp.status
rm -f $status
trap 'echo "${result:-failed:aborted}" > $status' EXIT
. ~/.cargo/env
rustc --version
cd ~/rsnano-node || { result=failed:no-repo; exit 1; }
build() {  # name branch commit
  local name=$1 br=$2 sha=$3 tree=$HOME/bp-$1
  cd ~/rsnano-node || return 1
  # An explicit tracking ref: FETCH_HEAD is per worktree
  git fetch -q origin "+$br:refs/remotes/origin/$br" || return 2
  if [ -d "$tree" ]; then
    (cd "$tree" && git checkout -q --detach "$sha") || return 3
  else
    git worktree add -q --detach "$tree" "$sha" || return 3
  fi
  cd "$tree" && git log --oneline -1
  cargo build -q --release -p rsnano_cli --features rsnano_cli/rai_protocol || return 4
  cargo build -q --release -p rsnano_cli --features rsnano_cli/rai_byzantine --target-dir target/byz || return 5
  local b=$HOME/bin-$name/rai
  rm -rf "$b" && mkdir -p "$b"
  cp target/release/rsnano "$b/" && cp target/byz/release/rsnano "$b/rsnano-byzantine" && cp ~/bin-m12b/rai/nanospam "$b/" || return 6
  for f in rsnano rsnano-byzantine; do
    [ "$(strings "$b/$f" | grep -c EPOCH_START)" -gt 0 ] || return 7
  done
  (cd "$b" && sha256sum *)
}
build bpbase rai_dsn_anon 5d5709827 || { result=failed:base-$?; exit 1; }
build bpfix rai_backpressure 15c7f4693 || { result=failed:fix-$?; exit 1; }
result=done
