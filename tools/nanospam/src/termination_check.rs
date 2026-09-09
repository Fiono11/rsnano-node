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
        let bad: Vec<_> = events
            .iter()
            .enumerate()
            .filter_map(|(pr, ev)| {
                let ok = if rai {
                    ev.iter().any(|e| matches!(e.0, 1 | 2 | 4))
                } else {
                    ev.iter().any(|e| matches!(e.0, 2 | 4))
                };
                (!ok).then_some(pr)
            })
            .collect();
        if finals.len() > 1 {
            conflicts += 1;
        }
        if !bad.is_empty() || finals.len() > 1 || prs.is_empty() {
            failed += 1;
            if failures.len() < 20 {
                failures.push(serde_json::json!({"root":root,"missing_termination_prs":bad,"conflicting_finalizations":finals.len()>1}));
            }
        }
    }
    serde_json::json!({"success":failed==0 && !roots.is_empty() && !prs.is_empty(),"workload_roots":roots.len(),"failed_roots":failed,"conflicting_finalization_roots":conflicts,"examples":failures})
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
    fn different_epochs_are_allowed() {
        assert!(ok(vec![
            vec![event(1, 1, 0), event(1, 1, 1)],
            vec![event(1, 1, 1)]
        ]));
    }
    #[test]
    fn epoch_one_when_no_epoch_zero() {
        assert!(ok(vec![vec![event(1, 1, 1)], vec![event(1, 1, 1)]]));
    }
    #[test]
    fn different_certificate_sets_are_allowed() {
        assert!(ok(vec![
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
    fn implicit_finalization_needs_no_certificate() {
        assert!(ok(vec![vec![event(1, 1, 0)], vec![event(4, 1, 1)]]));
        assert!(ok(vec![vec![event(4, 1, 0)], vec![event(4, 1, 1)]]));
    }
    #[test]
    fn conflicting_finalization_fails_even_if_implicit() {
        assert!(!ok(vec![vec![event(4, 1, 0)], vec![event(4, 2, 1)]]));
    }
    #[test]
    fn missing_and_after_cutoff_fail() {
        assert!(!ok(vec![vec![event(1, 1, 0)], vec![]]));
        let mut e = event(1, 1, 0);
        e.4 = 11;
        assert!(!ok(vec![vec![e]]));
    }
}
