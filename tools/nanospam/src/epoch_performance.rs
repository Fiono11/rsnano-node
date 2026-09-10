use rsnano_nullable_clock::Timestamp;
use rsnano_types::QualifiedRoot;
use rsnano_websocket_messages::ElectionOutcome;
use std::{collections::HashMap, time::Duration};

#[derive(Default)]
pub(crate) struct EpochPerformance {
    publications: HashMap<QualifiedRoot, (bool, Option<Timestamp>)>,
    first_vote_durations: HashMap<(u64, QualifiedRoot), [Option<Duration>; 2]>,
    outcomes: HashMap<(u64, QualifiedRoot), [Option<Duration>; 2]>,
}
impl EpochPerformance {
    pub fn register(&mut self, root: QualifiedRoot, forked: bool) {
        self.publications.entry(root).or_insert((forked, None));
    }
    pub fn published(&mut self, root: &QualifiedRoot, now: Timestamp) {
        if let Some((_, start)) = self.publications.get_mut(root) {
            start.get_or_insert(now);
        }
    }
    pub fn observe(&mut self, event: ElectionOutcome, received: Timestamp) {
        let Some((_, Some(start))) = self.publications.get(&event.root) else {
            return;
        };
        let elapsed = start.elapsed(received);
        if let Some(us) = event.first_vote_to_outcome_us {
            let times = self
                .first_vote_durations
                .entry((event.epoch, event.root.clone()))
                .or_default();
            if !self
                .outcomes
                .contains_key(&(event.epoch, event.root.clone()))
            {
                times[0].get_or_insert(Duration::from_micros(us));
            }
            if event.finalized {
                times[1].get_or_insert(Duration::from_micros(us));
            }
        }
        let times = self.outcomes.entry((event.epoch, event.root)).or_default();
        times[0].get_or_insert(elapsed);
        if event.finalized {
            times[1].get_or_insert(elapsed);
        }
    }
    pub fn summarize(&self, closed_epochs: u64) -> serde_json::Value {
        let mut rows = Vec::new();
        for epoch in 0..closed_epochs {
            for forked in [false, true] {
                let mut durations = [Vec::new(), Vec::new()];
                for ((e, root), times) in &self.outcomes {
                    if *e != epoch || self.publications[root].0 != forked {
                        continue;
                    }
                    for i in 0..2 {
                        if let Some(t) = times[i] {
                            durations[i].push(t.as_secs_f64() * 1000.0);
                        }
                    }
                }
                let timing = |values: &mut Vec<f64>| {
                    values.sort_by(f64::total_cmp);
                    serde_json::json!({"count":values.len(),
                        "mean_ms":(!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64),
                        "p95_ms":(!values.is_empty()).then(|| values[(values.len() * 95).div_ceil(100) - 1]),
                        "max_ms":values.last()})
                };
                rows.push(serde_json::json!({"epoch":epoch,"forked":forked,
                    "terminated":timing(&mut durations[0]),"finalized":timing(&mut durations[1])}));
            }
        }
        let mut cohorts = Vec::new();
        for epoch in 0..closed_epochs {
            for forked in [false, true] {
                for carried in [false, true] {
                    let mut publication = [Vec::new(), Vec::new()];
                    let mut first = [Vec::new(), Vec::new()];
                    let mut before_first = [Vec::new(), Vec::new()];
                    for ((e, root), times) in &self.outcomes {
                        if *e != epoch || self.publications[root].0 != forked {
                            continue;
                        }
                        let previous =
                            (0..epoch).any(|old| self.outcomes.contains_key(&(old, root.clone())));
                        if previous != carried {
                            continue;
                        }
                        for i in 0..2 {
                            if let Some(t) = times[i] {
                                publication[i].push(t.as_secs_f64() * 1000.0);
                                if let Some(d) = self
                                    .first_vote_durations
                                    .get(&(*e, root.clone()))
                                    .and_then(|v| v[i])
                                {
                                    first[i].push(d.as_secs_f64() * 1000.0);
                                    before_first[i]
                                        .push(t.saturating_sub(d).as_secs_f64() * 1000.0);
                                }
                            }
                        }
                    }
                    let stats = |v: &mut Vec<f64>| {
                        v.sort_by(f64::total_cmp);
                        serde_json::json!({"count":v.len(), "mean_ms":(!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64), "p95_ms":(!v.is_empty()).then(|| v[(v.len()*95).div_ceil(100)-1]), "max_ms":v.last()})
                    };
                    cohorts.push(serde_json::json!({"epoch":epoch,"forked":forked,"prior_epoch_outcome":carried,
                        "terminated":{"publication":stats(&mut publication[0]),"first_vote":stats(&mut first[0]),"before_first_plus_delivery":stats(&mut before_first[0])},
                        "finalized":{"publication":stats(&mut publication[1]),"first_vote":stats(&mut first[1]),"before_first_plus_delivery":stats(&mut before_first[1])}}));
                }
            }
        }
        let mut windows = Vec::new();
        if let Some(start) = self.publications.values().filter_map(|(_, t)| *t).min() {
            let mut groups: std::collections::BTreeMap<(u64, bool, u64, bool), [Vec<f64>; 4]> =
                Default::default();
            for ((epoch, root), times) in &self.outcomes {
                let (forked, publication) = self.publications[root];
                let bin = start.elapsed(publication.unwrap()).as_secs() / 5;
                let carried = (0..*epoch).any(|e| self.outcomes.contains_key(&(e, root.clone())));
                let values = groups.entry((*epoch, forked, bin, carried)).or_default();
                for i in 0..2 {
                    if let Some(t) = times[i] {
                        values[i].push(t.as_secs_f64() * 1000.0);
                    }
                    if let Some(t) = self
                        .first_vote_durations
                        .get(&(*epoch, root.clone()))
                        .and_then(|x| x[i])
                    {
                        values[2 + i].push(t.as_secs_f64() * 1000.0);
                    }
                }
            }
            for ((epoch, forked, bin, carried), mut values) in groups {
                let stats = |v: &mut Vec<f64>| {
                    v.sort_by(f64::total_cmp);
                    serde_json::json!({"count":v.len(),"mean_ms":(!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64),"p95_ms":(!v.is_empty()).then(|| v[(v.len()*95).div_ceil(100)-1])})
                };
                windows.push(serde_json::json!({"epoch":epoch,"forked":forked,"publication_start_seconds":bin*5,"prior_epoch_outcome":carried,"termination":stats(&mut values[0]),"finalization":stats(&mut values[1]),"first_to_termination":stats(&mut values[2]),"first_to_finalization":stats(&mut values[3])}));
            }
        }
        serde_json::json!({"publication_windows":windows,"cohorts":cohorts,"pr":0,"latency_basis":"first publication to PR0 WebSocket outcome receipt", "rows":rows})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn publication_latency_survives_republish_and_deduplicates_each_epoch() {
        let root = QualifiedRoot::new(1.into(), 2.into());
        let start = Timestamp::new_test_instance();
        let mut p = EpochPerformance::default();
        p.register(root.clone(), true);
        p.published(&root, start);
        p.published(&root, start + Duration::from_millis(5));
        let event = |epoch, finalized| ElectionOutcome {
            root: root.clone(),
            hash: 3.into(),
            epoch,
            finalized,
            timeout: false,
            first_vote_to_outcome_us: Some(2000),
        };
        p.observe(event(0, false), start + Duration::from_millis(10));
        p.observe(event(0, false), start + Duration::from_millis(20));
        p.observe(event(0, true), start + Duration::from_millis(30));
        p.observe(event(1, true), start + Duration::from_millis(50));
        let s = p.summarize(2);
        assert_eq!(s["rows"][1]["terminated"]["count"], 1);
        assert_eq!(s["rows"][1]["terminated"]["mean_ms"], 10.0);
        assert_eq!(s["rows"][1]["finalized"]["mean_ms"], 30.0);
        assert_eq!(s["rows"][3]["terminated"]["mean_ms"], 50.0);
        assert_eq!(s["rows"][0]["terminated"]["count"], 0);
        let carried = s["cohorts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["epoch"] == 1 && c["forked"] == true && c["prior_epoch_outcome"] == true)
            .unwrap();
        assert_eq!(carried["terminated"]["publication"]["mean_ms"], 50.0);
        assert_eq!(carried["terminated"]["first_vote"]["mean_ms"], 2.0);
        assert_eq!(
            carried["terminated"]["before_first_plus_delivery"]["mean_ms"],
            48.0
        );
    }
}
