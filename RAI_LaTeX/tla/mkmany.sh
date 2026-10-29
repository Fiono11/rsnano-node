#!/bin/sh
# mkmany.sh: writes the Many<B>_<name>.cfg files of RaiRecoverMany into the
# current directory (the scripted Many2_paper_hole*.cfg are written by hand).
# Prefix assignments to a function call persist in sh: every row after the
# first prefixed one names all of IMP SET REN NOE OWN ORD it relies on.
# The Many3_{patch,none,hash,perm}_* configs were generated but not run.
# row: name pick attach byz ready view constraint invariants...
mk() {
  b=$1 name=$2 pick=$3 att=$4 byzm=$5 ready=$6 view=$7 cons=$8; shift 8
  f=Many${b}_$name.cfg
  {
    echo "SPECIFICATION Spec"
    echo "CONSTANTS"
    echo "  BLOCKS = $b"
    echo "  E = 4"
    echo "  IMPLIED = \"${IMP:-all}\""
    echo "  SETTLED = ${SET:-TRUE}"
    echo "  PICKRULE = \"$pick\""
    echo "  ATTACH = \"$att\""
    echo "  RENOTARIZE = ${REN:-FALSE}"
    echo "  NOEARLY = ${NOE:-FALSE}"
    echo "  OWNER = \"${OWN:-policy}\""
    echo "  R2ORDER = \"${ORD:-up}\""
    echo "  BYZ = \"$byzm\""
    echo "  READY = $ready"
    [ "$view" = v ] && echo "VIEW View"
    [ "$cons" != - ] && echo "ACTION_CONSTRAINT $cons"
    echo "INVARIANTS $*"
  } > "$f"
}
for b in 2 3; do
  mk $b none_live          none strict     release '{3, 4}' v StopRecovered RecoveredMany Progress
  mk $b none_probe_nothing none strict     all     '{}'     v - ProbeNothing
  mk $b patch_safe         prev strict     all     '{}'     v - NoConflict ClosureThm1 OneKept KeptOrFinal FlipBound
  mk $b patch_flip         prev strict     all     '{}'     v StopFlip FlipBound KeptOrFinal
  mk $b patch_live         prev strict     release '{3, 4}' v StopRecovered RecoveredMany Progress KeptOrFinal
  mk $b patch_two23        prev strict     release '{2, 3}' v StopRecovered RecoveredEarly Progress
  mk $b patch_probe_early  prev strict     all     '{}'     v - ProbeEarlyFinalV
  mk $b patch_probe_witness prev strict    all     '{}'     v - ProbeWitnessKeep
  mk $b patch_probe_regain prev strict     all     '{}'     v - ProbeRegain
  mk $b patch_early        prev strict     all     '{}'     n EarlyOverlap NoConflict ClosureThm1 FlipBound KeptOrFinal YieldNext
  mk $b patch_early_probe  prev strict     all     '{}'     n EarlyOverlap ProbeEarlyFinalV
  mk $b patch_early_s1     prev strict     all     '{}'     n EarlyOverlap ProbeS1Rival
  mk $b hash_flip          hash strict     all     '{}'     v StopFlip FlipBound
  mk $b hash_kof           hash strict     all     '{}'     v - KeptOrFinal
  mk $b perm_flip          prev permissive all     '{}'     v StopFlip FlipBound
  mk $b perm_safe          prev permissive all     '{}'     v - NoConflict ClosureThm1 KeptOrFinal
  mk $b perm_live          prev permissive release '{3, 4}' v StopRecovered RecoveredMany Progress
  mk $b perm_probe_regain  prev permissive all     '{}'     v - ProbeRegain
done
# Route A (re-notarization, no ancestor support, settled votes, previous
# block first) and the paper's current rules (all first votes, hash).
fam() {
  b=$1 fname=$2 pick=$3
  mk $b ${fname}_safe        $pick strict all     '{}'     v - NoConflict ClosureThm1 OneKept
  mk $b ${fname}_flip        $pick strict all     '{}'     v StopFlip FlipBound
  mk $b ${fname}_kof         $pick strict all     '{}'     v - KeptOrFinal
  mk $b ${fname}_live        $pick strict release '{3, 4}' v StopRecovered RecoveredMany Progress
  mk $b ${fname}_two23       $pick strict release '{2, 3}' v StopRecovered RecoveredEarly Progress
  mk $b ${fname}_gap24       $pick strict release '{2, 4}' v StopRecovered RecoveredMany Progress
  mk $b ${fname}_probe_flip  $pick strict all     '{}'     v - ProbeFlip
  mk $b ${fname}_probe_early $pick strict all     '{}'     v - ProbeEarlyFinalV
}
for b in 2 3; do
  IMP=off SET=TRUE  REN=TRUE NOE=TRUE fam $b routeA prev
  IMP=off SET=FALSE REN=TRUE NOE=TRUE fam $b paper  hash
done
# Item d under route A: early finality under the overlap rule, scripted.
for b in 2 3; do
  IMP=off SET=TRUE REN=TRUE NOE=TRUE mk $b routeA_early    prev strict all '{}' n EarlyOverlap NoConflict ClosureThm1 FlipBound KeptOrFinal YieldNext
  IMP=off SET=TRUE REN=TRUE NOE=TRUE mk $b routeA_early_s1 prev strict all '{}' n EarlyOverlap ProbeS1Rival
done
# Rule 2 order and a forking owner that signs every child at once.
for b in 2 3; do
  for o in up down; do
    IMP=off SET=FALSE REN=TRUE NOE=TRUE OWN=any ORD=$o mk $b paper_any_${o}_safe  hash strict all '{}' v - NoConflict ClosureThm1 OneKept
    IMP=off SET=TRUE  REN=TRUE NOE=TRUE OWN=any ORD=$o mk $b routeA_any_${o}_safe prev strict all '{}' v - NoConflict ClosureThm1 OneKept
  done
  IMP=off SET=FALSE REN=TRUE NOE=TRUE OWN=policy ORD=up mk $b paper_nc hash strict all '{}' v - NoConflict
  IMP=off SET=TRUE REN=TRUE NOE=TRUE OWN=policy ORD=down mk $b routeA_down_live  prev strict release '{3, 4}' v StopRecovered RecoveredMany Progress
  IMP=off SET=TRUE REN=TRUE NOE=TRUE OWN=policy ORD=down mk $b routeA_down_two23 prev strict release '{2, 3}' v StopRecovered RecoveredEarly Progress
  IMP=off SET=TRUE REN=TRUE NOE=TRUE OWN=policy ORD=down mk $b routeA_down_flip  prev strict all '{}' v StopFlip FlipBound
  IMP=off SET=TRUE REN=TRUE NOE=TRUE OWN=policy ORD=down mk $b routeA_down_kof   prev strict all '{}' v - KeptOrFinal
done
