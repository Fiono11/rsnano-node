use rsnano_types::{BlockHash, QualifiedRoot};
use std::collections::{BTreeMap, BTreeSet, HashSet};
pub type Event = (u8, QualifiedRoot, BlockHash, u64, u64);

/// Require termination on every PR and reject conflicting finalized values.
pub fn check(
    roots: &HashSet<QualifiedRoot>,
    prs: &[Vec<Event>],
    cutoff: u64,
    rai: bool,
) -> serde_json::Value {
    let mut failures = Vec::new();
    let mut failed = 0;
    let mut conflicts = 0;
    let mut pending = 0;
    let mut timeout_roots = 0;
    let mut timeout_finalization_conflicts = 0;
    let mut indexed = Vec::new();
    for pr in prs {
        let mut map: BTreeMap<QualifiedRoot, Vec<&Event>> = BTreeMap::new();
        for e in pr {
            if e.4 <= cutoff {
                map.entry(e.1.clone()).or_default().push(e);
            }
        }
        indexed.push(map);
    }
    for root in roots {
        let events: Vec<_> = indexed
            .iter()
            .map(|p| p.get(root).cloned().unwrap_or_default())
            .collect();
        let finals: BTreeSet<_> = events
            .iter()
            .flatten()
            .filter(|e| matches!(e.0, 2 | 4))
            .map(|e| e.2)
            .collect();
        // Prefer a finalized block, then block notarization, then a notarized
        // timeout. Within that outcome class the earliest epoch is canonical.
        let canonical = events
            .iter()
            .flatten()
            .filter(|e| matches!(e.0, 2 | 4))
            .map(|e| e.3)
            .min()
            .or_else(|| {
                events
                    .iter()
                    .flatten()
                    .filter(|e| e.0 == 1)
                    .map(|e| e.3)
                    .min()
            })
            .or_else(|| {
                events
                    .iter()
                    .flatten()
                    .filter(|e| e.0 == 7)
                    .map(|e| e.3)
                    .min()
            });
        if rai && canonical.is_none() {
            // A root without any outcome is incomplete even when all PRs agree
            // that it is pending. Keep it separate from conflicting outcomes.
            pending += 1;
            continue;
        }
        let certificates: BTreeSet<_> = events
            .iter()
            .flatten()
            .filter(|e| e.0 == 1 && Some(e.3) == canonical)
            .map(|e| e.2)
            .collect();
        let canonical_finals: BTreeSet<_> = events
            .iter()
            .flatten()
            .filter(|e| matches!(e.0, 2 | 4) && Some(e.3) == canonical)
            .map(|e| e.2)
            .collect();
        let canonical_timeout = certificates.is_empty()
            && canonical_finals.is_empty()
            && events
                .iter()
                .flatten()
                .any(|e| e.0 == 7 && Some(e.3) == canonical);
        timeout_roots += usize::from(canonical_timeout);
        let timeout_epochs: BTreeSet<_> = events
            .iter()
            .flatten()
            .filter(|e| e.0 == 7)
            .map(|e| e.3)
            .collect();
        let timeout_final_conflict = events
            .iter()
            .flatten()
            .any(|e| matches!(e.0, 2 | 4) && timeout_epochs.contains(&e.3));
        timeout_finalization_conflicts += usize::from(timeout_final_conflict);
        let bad: Vec<_> = events
            .iter()
            .enumerate()
            .filter_map(|(pr, ev)| {
                let ok = if rai {
                    let local_certificates: BTreeSet<_> = ev
                        .iter()
                        .filter(|e| e.0 == 1 && Some(e.3) == canonical)
                        .map(|e| e.2)
                        .collect();
                    let local_finals: BTreeSet<_> = ev
                        .iter()
                        .filter(|e| matches!(e.0, 2 | 4) && Some(e.3) == canonical)
                        .map(|e| e.2)
                        .collect();
                    ev.iter()
                        .any(|e| matches!(e.0, 1 | 2 | 4 | 7) && Some(e.3) == canonical)
                        && if canonical_timeout {
                            ev.iter().any(|e| e.0 == 7 && Some(e.3) == canonical)
                        } else if canonical_finals.is_empty() {
                            local_certificates == certificates
                        } else {
                            local_finals == canonical_finals
                        }
                } else {
                    ev.iter().any(|e| matches!(e.0, 2 | 4))
                };
                (!ok).then_some(pr)
            })
            .collect();
        if finals.len() > 1 {
            conflicts += 1;
        }
        if !bad.is_empty() || finals.len() > 1 || timeout_final_conflict || prs.is_empty() {
            failed += 1;
            if failures.len() < 20 {
                failures.push(serde_json::json!({"root":root,"canonical_epoch":canonical,"missing_or_different_prs":bad,"conflicting_finalizations":finals.len()>1,"conflicting_timeout_and_finalization":timeout_final_conflict}));
            }
        }
    }
    serde_json::json!({"success":failed==0 && pending==0 && !roots.is_empty() && !prs.is_empty(),"workload_roots":roots.len(),"pending_roots":pending,"timeout_roots":timeout_roots,"all_workload_terminated":pending==0,"failed_roots":failed,"conflicting_finalization_roots":conflicts,"conflicting_timeout_finalization_roots":timeout_finalization_conflicts,"examples":failures})
}
#[cfg(test)]
mod tests {
    use super::*;
    fn event(kind: u8, hash: u64, epoch: u64) -> Event {
        (
            kind,
            QualifiedRoot::new_test_instance(),
            BlockHash::from(hash),
            epoch,
            1,
        )
    }
    fn ok(prs: Vec<Vec<Event>>) -> bool {
        check(
            &HashSet::from([QualifiedRoot::new_test_instance()]),
            &prs,
            10,
            true,
        )["success"]
            .as_bool()
            .unwrap()
    }
    #[test]
    fn pending_everywhere_fails_even_when_decided_roots_agree() {
        let decided = event(1, 1, 0);
        let pending = QualifiedRoot::new(BlockHash::from(999).into(), BlockHash::ZERO);
        let roots = HashSet::from([decided.1.clone(), pending]);
        let result = check(&roots, &[vec![decided.clone()], vec![decided]], 10, true);
        assert_eq!(result["success"], false);
        assert_eq!(result["pending_roots"], 1);
        assert_eq!(result["failed_roots"], 0);
        assert_eq!(result["all_workload_terminated"], false);
    }

    #[test]
    fn all_roots_must_terminate_but_notarization_is_sufficient() {
        let root = QualifiedRoot::new_test_instance();
        let roots = HashSet::from([root]);
        let pending = check(&roots, &[vec![], vec![]], 10, true);
        assert_eq!(pending["success"], false);
        assert_eq!(pending["pending_roots"], 1);
        let complete = check(
            &roots,
            &[vec![event(1, 1, 0)], vec![event(1, 1, 0)]],
            10,
            true,
        );
        assert_eq!(complete["success"], true);
        assert_eq!(complete["all_workload_terminated"], true);
    }

    #[test]
    fn certified_timeout_must_be_recovered_by_every_pr() {
        assert!(ok(vec![vec![event(7, 0, 0)], vec![event(7, 0, 0)]]));
        assert!(!ok(vec![vec![event(7, 0, 0)], vec![]]));
        assert!(!ok(vec![vec![event(7, 0, 0)], vec![event(7, 0, 1)]]));
    }

    #[test]
    fn block_certificate_takes_precedence_over_timeout_and_must_be_recovered() {
        assert!(!ok(vec![
            vec![event(7, 0, 0), event(1, 1, 1)],
            vec![event(7, 0, 0)]
        ]));
        assert!(ok(vec![
            vec![event(7, 0, 0), event(1, 1, 1)],
            vec![event(1, 1, 1)]
        ]));
        assert!(!ok(vec![
            vec![event(7, 0, 0), event(2, 1, 0)],
            vec![event(2, 1, 0)]
        ]));
    }

    #[test]
    fn missing_canonical_epoch_fails() {
        assert!(!ok(vec![
            vec![event(1, 1, 0), event(1, 1, 1)],
            vec![event(1, 1, 1)]
        ]));
    }
    #[test]
    fn epoch_one_when_no_epoch_zero() {
        assert!(ok(vec![vec![event(1, 1, 1)], vec![event(1, 1, 1)]]));
    }
    #[test]
    fn different_canonical_certificate_sets_fail() {
        assert!(!ok(vec![
            vec![event(1, 1, 0), event(1, 2, 0)],
            vec![event(1, 2, 0)]
        ]));
    }
    #[test]
    fn other_epoch_sets_do_not_matter() {
        assert!(ok(vec![
            vec![event(1, 1, 0), event(1, 2, 1)],
            vec![event(1, 1, 0)]
        ]));
    }
    #[test]
    fn implicit_finalization_requires_canonical_epoch_agreement() {
        assert!(!ok(vec![vec![event(1, 1, 0)], vec![event(4, 1, 1)]]));
        assert!(!ok(vec![vec![event(4, 1, 0)], vec![event(4, 1, 1)]]));
        assert!(ok(vec![vec![event(4, 1, 0)], vec![event(4, 1, 0)]]));
    }
    #[test]
    fn conflicting_finalization_fails_even_if_implicit() {
        assert!(!ok(vec![vec![event(4, 1, 0)], vec![event(4, 2, 1)]]));
    }
    #[test]
    fn finalization_supersedes_different_notarization_sets() {
        assert!(ok(vec![
            vec![event(1, 1, 0), event(1, 2, 0), event(2, 1, 0)],
            vec![event(4, 1, 0)],
        ]));
        assert!(!ok(vec![vec![event(2, 1, 0)], vec![event(1, 1, 0)]]));
    }

    #[test]
    fn later_finalization_supersedes_earlier_notarization() {
        assert!(!ok(vec![
            vec![event(1, 1, 0), event(2, 1, 1)],
            vec![event(1, 1, 0)],
        ]));
        assert!(ok(vec![
            vec![event(1, 1, 0), event(2, 1, 1)],
            vec![event(4, 1, 1)],
        ]));
        assert!(!ok(vec![vec![event(2, 1, 0)], vec![event(2, 1, 1)]]));
    }

    #[test]
    fn missing_and_after_cutoff_fail() {
        assert!(!ok(vec![vec![event(1, 1, 0)], vec![]]));
        let mut e = event(1, 1, 0);
        e.4 = 11;
        assert!(!ok(vec![vec![e]]));
    }
}
