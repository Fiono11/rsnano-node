use rsnano_nullable_clock::Timestamp;
use rsnano_types::QualifiedRoot;
use rsnano_websocket_messages::ElectionOutcome;
use std::{collections::HashMap, time::Duration};

#[derive(Default)]
pub(crate) struct EpochPerformance {
    publications: HashMap<QualifiedRoot, (bool, Option<Timestamp>)>,
    first_vote_durations: HashMap<(u64, QualifiedRoot), [Option<Duration>; 2]>,
    outcomes: HashMap<(u64, QualifiedRoot), [Option<Duration>; 2]>,
    clock_anchor: Option<ClockAnchor>,
}

struct ClockAnchor {
    before: Timestamp,
    unix_us: u64,
    after: Timestamp,
}

impl EpochPerformance {
    /// The wall-clock sample was taken between these two monotonic samples.
    /// Capture this once before publication, never on the per-block hot path.
    pub fn set_clock_anchor(&mut self, before: Timestamp, unix_us: u64, after: Timestamp) {
        assert!(before <= after);
        self.clock_anchor = Some(ClockAnchor {
            before,
            unix_us,
            after,
        });
    }

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
    /// Epochs with at least one observed outcome; used to report a run whose
    /// epoch closes never converged.
    pub fn observed_epochs(&self) -> u64 {
        self.outcomes
            .keys()
            .map(|(epoch, _)| epoch + 1)
            .max()
            .unwrap_or(0)
    }
    pub fn summarize(&self, closed_epochs: u64, cutoff: Timestamp) -> serde_json::Value {
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
        let mut outcome_windows = Vec::new();
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

            // Publication cohorts measure latency, but their counts cannot show
            // finalization throughput. Bin each outcome by its receipt time;
            // termination and finalization of one root may occupy different bins.
            let mut groups: std::collections::BTreeMap<(u64, bool, u64, bool), [Vec<f64>; 4]> =
                Default::default();
            for ((epoch, root), times) in &self.outcomes {
                let (forked, publication) = self.publications[root];
                let carried = (0..*epoch).any(|e| self.outcomes.contains_key(&(e, root.clone())));
                for i in 0..2 {
                    let Some(elapsed) = times[i] else {
                        continue;
                    };
                    let received = publication.unwrap() + elapsed;
                    let bin = start.elapsed(received).as_secs() / 5;
                    let values = groups.entry((*epoch, forked, bin, carried)).or_default();
                    values[i].push(elapsed.as_secs_f64() * 1000.0);
                    if let Some(first) = self
                        .first_vote_durations
                        .get(&(*epoch, root.clone()))
                        .and_then(|x| x[i])
                    {
                        values[2 + i].push(first.as_secs_f64() * 1000.0);
                    }
                }
            }
            for ((epoch, forked, bin, carried), mut values) in groups {
                let stats = |v: &mut Vec<f64>| {
                    v.sort_by(f64::total_cmp);
                    serde_json::json!({"count":v.len(),"mean_ms":(!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64),"p95_ms":(!v.is_empty()).then(|| v[(v.len()*95).div_ceil(100)-1])})
                };
                let mut termination = stats(&mut values[0]);
                termination["per_second"] = serde_json::json!(values[0].len() as f64 / 5.0);
                let mut finalization = stats(&mut values[1]);
                finalization["per_second"] = serde_json::json!(values[1].len() as f64 / 5.0);
                outcome_windows.push(serde_json::json!({"epoch":epoch,"forked":forked,"outcome_start_seconds":bin*5,"window_seconds":5,"prior_epoch_outcome":carried,"termination":termination,"finalization":finalization,"first_to_termination":stats(&mut values[2]),"first_to_finalization":stats(&mut values[3])}));
            }
        }
        serde_json::json!({"publication_windows":windows,"outcome_windows":outcome_windows,"cohorts":cohorts,"pr":0,"latency_basis":"first publication to PR0 WebSocket outcome receipt", "rows":rows,"timeline":self.timeline(cutoff)})
    }

    fn timeline(&self, cutoff: Timestamp) -> serde_json::Value {
        let mut published: Vec<_> = self
            .publications
            .iter()
            .filter_map(|(root, (forked, time))| time.map(|time| (time, root, *forked)))
            .collect();
        published.sort_unstable_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        let first = published.first().map(|p| p.0);
        let indices: HashMap<_, _> = published
            .iter()
            .enumerate()
            .map(|(index, (_, root, _))| (*root, index))
            .collect();
        let publications: Vec<_> = published
            .iter()
            .map(|(time, _, forked)| (first.unwrap().elapsed(*time).as_micros() as u64, *forked))
            .collect();
        let mut outcomes = Vec::with_capacity(self.outcomes.len());
        for ((epoch, root), times) in &self.outcomes {
            let index = indices[root];
            let publication = published[index].0;
            let receipt_offset = |elapsed: Option<Duration>| {
                elapsed
                    .map(|elapsed| first.unwrap().elapsed(publication + elapsed).as_micros() as u64)
            };
            let first_times = self.first_vote_durations.get(&(*epoch, root.clone()));
            let first_duration = |i| {
                first_times
                    .and_then(|times| times[i])
                    .map(|elapsed: Duration| elapsed.as_micros() as u64)
            };
            outcomes.push((
                index,
                *epoch,
                receipt_offset(times[0]),
                receipt_offset(times[1]),
                first_duration(0),
                first_duration(1),
            ));
        }
        outcomes.sort_unstable_by_key(|row| (row.0, row.1));
        let unix_bounds = first.and_then(|first| {
            self.clock_anchor
                .as_ref()
                .map(|anchor| anchor.unix_bounds(first))
        });
        serde_json::json!({
            "schema_version": 1,
            "origin": "first_publication",
            "unit": "microseconds",
            "offset_rounding": "floor",
            "offset_rounding_error_us": 1,
            "publication_columns": ["publication_offset_us", "forked"],
            "outcome_columns": ["publication_index", "epoch", "termination_offset_us", "finalization_offset_us", "first_to_termination_us", "first_to_finalization_us"],
            "first_publication_unix_us_lower_bound": unix_bounds.map(|bounds| bounds.0),
            "first_publication_unix_us_upper_bound": unix_bounds.map(|bounds| bounds.1),
            "observation_cutoff_offset_us": first.map(|first| first.elapsed(cutoff).as_micros() as u64),
            "publications": publications,
            "outcomes": outcomes,
        })
    }
}

impl ClockAnchor {
    fn unix_bounds(&self, timestamp: Timestamp) -> (i128, i128) {
        let delta_ns = |from: Timestamp| {
            if timestamp >= from {
                from.elapsed(timestamp).as_nanos() as i128
            } else {
                -(timestamp.elapsed(from).as_nanos() as i128)
            }
        };
        // SystemTime was floored to microseconds. Round the resulting lower
        // bound down and upper bound up, retaining both sampling uncertainty
        // and sub-microsecond conversion uncertainty.
        let lower_ns = i128::from(self.unix_us) * 1000 + delta_ns(self.after);
        let upper_ns = (i128::from(self.unix_us) + 1) * 1000 + delta_ns(self.before);
        (lower_ns.div_euclid(1000), (upper_ns + 999).div_euclid(1000))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(root: &QualifiedRoot, epoch: u64, finalized: bool) -> ElectionOutcome {
        ElectionOutcome {
            root: root.clone(),
            hash: 3.into(),
            epoch,
            finalized,
            timeout: false,
            first_vote_to_outcome_us: Some(2000),
        }
    }

    #[test]
    fn throughput_uses_receipt_windows_and_deduplicates_each_outcome() {
        let early = QualifiedRoot::new(1.into(), 2.into());
        let delayed = QualifiedRoot::new(3.into(), 4.into());
        let start = Timestamp::new_test_instance();
        let mut p = EpochPerformance::default();
        p.register(early.clone(), false);
        p.register(delayed.clone(), false);
        p.published(&early, start);
        p.published(&delayed, start + Duration::from_secs(4));
        p.observe(event(&early, 0, true), start + Duration::from_millis(4999));
        p.observe(event(&early, 0, true), start + Duration::from_secs(15));
        p.observe(event(&delayed, 0, false), start + Duration::from_secs(5));
        p.observe(event(&delayed, 0, false), start + Duration::from_secs(6));
        p.observe(event(&delayed, 0, true), start + Duration::from_secs(10));
        p.observe(event(&delayed, 0, true), start + Duration::from_secs(20));

        let s = p.summarize(1, start + Duration::from_secs(20));
        let publications = s["publication_windows"].as_array().unwrap();
        assert_eq!(publications.len(), 1);
        assert_eq!(publications[0]["finalization"]["count"], 2);
        let windows = s["outcome_windows"].as_array().unwrap();
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0]["outcome_start_seconds"], 0);
        assert_eq!(windows[0]["window_seconds"], 5);
        assert_eq!(windows[0]["finalization"]["count"], 1);
        assert_eq!(windows[0]["finalization"]["per_second"], 0.2);
        assert_eq!(windows[1]["outcome_start_seconds"], 5);
        assert_eq!(windows[1]["termination"]["count"], 1);
        assert_eq!(windows[1]["termination"]["mean_ms"], 1000.0);
        assert_eq!(windows[1]["finalization"]["count"], 0);
        assert_eq!(windows[1]["finalization"]["per_second"], 0.0);
        assert_eq!(windows[2]["outcome_start_seconds"], 10);
        assert_eq!(windows[2]["termination"]["count"], 0);
        assert_eq!(windows[2]["finalization"]["count"], 1);
        assert_eq!(windows[2]["finalization"]["mean_ms"], 6000.0);
        assert_eq!(windows[2]["first_to_finalization"]["mean_ms"], 2.0);
        // Receipt offsets remain exact across a closing boundary, independently
        // of the publication cohort and repeated WebSocket deliveries.
        assert_eq!(
            s["timeline"]["publications"],
            serde_json::json!([[0, false], [4_000_000, false]])
        );
        assert_eq!(
            s["timeline"]["outcomes"],
            serde_json::json!([
                [0, 0, 4_999_000, 4_999_000, 2000, 2000],
                [1, 0, 5_000_000, 10_000_000, 2000, 2000]
            ])
        );
    }

    #[test]
    fn receipt_windows_separate_new_and_carried_elections_and_forks() {
        let carried = QualifiedRoot::new(1.into(), 2.into());
        let fresh = QualifiedRoot::new(3.into(), 4.into());
        let forked = QualifiedRoot::new(5.into(), 6.into());
        let start = Timestamp::new_test_instance();
        let mut p = EpochPerformance::default();
        for (root, fork) in [(&carried, false), (&fresh, false), (&forked, true)] {
            p.register(root.clone(), fork);
            p.published(root, start);
        }
        p.observe(event(&carried, 0, false), start + Duration::from_secs(1));
        for root in [&carried, &fresh, &forked] {
            p.observe(event(root, 1, true), start + Duration::from_secs(6));
        }

        let s = p.summarize(2, start + Duration::from_secs(6));
        let windows = s["outcome_windows"].as_array().unwrap();
        assert_eq!(windows.len(), 4);
        assert_eq!(windows[0]["epoch"], 0);
        assert_eq!(windows[0]["finalization"]["count"], 0);
        for (i, fork, prior) in [(1, false, false), (2, false, true), (3, true, false)] {
            assert_eq!(windows[i]["epoch"], 1);
            assert_eq!(windows[i]["forked"], fork);
            assert_eq!(windows[i]["prior_epoch_outcome"], prior);
            assert_eq!(windows[i]["outcome_start_seconds"], 5);
            assert_eq!(windows[i]["finalization"]["count"], 1);
            assert_eq!(windows[i]["finalization"]["per_second"], 0.2);
        }
    }

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
        assert_eq!(p.observed_epochs(), 2);
        let s = p.summarize(2, start + Duration::from_millis(50));
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
        assert_eq!(
            s["timeline"]["publications"],
            serde_json::json!([[0, true]])
        );
        assert_eq!(
            s["timeline"]["outcomes"],
            serde_json::json!([
                [0, 0, 10_000, 30_000, 2000, 2000],
                [0, 1, 50_000, 50_000, 2000, 2000]
            ])
        );
    }

    #[test]
    fn timeline_aligns_clock_bounds_and_preserves_missing_outcome_publications() {
        let earlier_tie = QualifiedRoot::new(1.into(), 1.into());
        let later_tie = QualifiedRoot::new(2.into(), 2.into());
        let delayed = QualifiedRoot::new(3.into(), 3.into());
        let unpublished = QualifiedRoot::new(4.into(), 4.into());
        let mut p = EpochPerformance::default();
        p.set_clock_anchor(Timestamp::new(10_000), 1_000_000, Timestamp::new(12_500));
        // Deliberately register/publish in reverse order. Equal publication times
        // use the root as a deterministic tie-breaker without exporting its hash.
        for (root, forked) in [
            (&unpublished, false),
            (&delayed, true),
            (&later_tie, true),
            (&earlier_tie, false),
        ] {
            p.register(root.clone(), forked);
        }
        p.published(&delayed, Timestamp::new(30_999));
        p.published(&later_tie, Timestamp::new(20_200));
        p.published(&earlier_tie, Timestamp::new(20_200));
        let mut terminated = event(&earlier_tie, 0, false);
        terminated.first_vote_to_outcome_us = None;
        p.observe(terminated, Timestamp::new(23_000));
        let mut outcome = event(&delayed, 1, true);
        outcome.first_vote_to_outcome_us = None;
        p.observe(outcome, Timestamp::new(35_500));
        let s = p.summarize(0, Timestamp::new(40_900));
        let timeline = &s["timeline"];
        assert_eq!(timeline["schema_version"], 1);
        assert_eq!(timeline["origin"], "first_publication");
        assert_eq!(timeline["unit"], "microseconds");
        assert_eq!(timeline["offset_rounding"], "floor");
        assert_eq!(timeline["offset_rounding_error_us"], 1);
        assert_eq!(
            timeline["publication_columns"],
            serde_json::json!(["publication_offset_us", "forked"])
        );
        assert_eq!(
            timeline["outcome_columns"],
            serde_json::json!([
                "publication_index",
                "epoch",
                "termination_offset_us",
                "finalization_offset_us",
                "first_to_termination_us",
                "first_to_finalization_us"
            ])
        );
        assert_eq!(timeline["first_publication_unix_us_lower_bound"], 1_000_007);
        assert_eq!(timeline["first_publication_unix_us_upper_bound"], 1_000_012);
        assert_eq!(timeline["observation_cutoff_offset_us"], 20);
        assert_eq!(
            timeline["publications"],
            serde_json::json!([[0, false], [0, true], [10, true]])
        );
        // Floor only after adding the publication offset to the receipt delay:
        // floor(10.799 + 4.501) = 15, whereas flooring each separately loses 1 us.
        assert_eq!(
            timeline["outcomes"],
            serde_json::json!([[0, 0, 2, null, null, null], [2, 1, 15, 15, null, null]])
        );
        assert_eq!(
            p.summarize(2, Timestamp::new(40_900))["timeline"],
            *timeline,
            "timeline must include observed epochs independently of close status"
        );
    }

    #[test]
    fn timeline_without_publications_has_no_invented_origin_or_cutoff() {
        let mut p = EpochPerformance::default();
        p.set_clock_anchor(Timestamp::new(1000), 1_000_000, Timestamp::new(2000));
        p.register(QualifiedRoot::new_test_instance(), false);
        let s = p.summarize(0, Timestamp::new(3000));
        let timeline = &s["timeline"];
        assert_eq!(timeline["publications"], serde_json::json!([]));
        assert_eq!(timeline["outcomes"], serde_json::json!([]));
        assert!(timeline["first_publication_unix_us_lower_bound"].is_null());
        assert!(timeline["first_publication_unix_us_upper_bound"].is_null());
        assert!(timeline["observation_cutoff_offset_us"].is_null());
    }
}
