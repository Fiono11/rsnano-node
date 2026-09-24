use std::{collections::BTreeMap, time::Duration};

/// Diagnostic: how busy a single event thread was in one second, by event
/// kind, and what the work done for every event cost on top. One summary
/// per second tells whether the thread was saturated and by what.
#[derive(Default)]
pub(crate) struct FactTimings {
    second: Option<u64>,
    kinds: BTreeMap<&'static str, KindTiming>,
    per_event: BTreeMap<&'static str, Duration>,
}

#[derive(Default)]
struct KindTiming {
    count: usize,
    total: Duration,
    max: Duration,
}

impl FactTimings {
    /// Adds one event of `kind` handled at `now_ms`, which took `handling`
    /// plus the `per_event` work done for every event. Returns the summary
    /// of the previous second once an event of a later second arrives.
    pub fn record(
        &mut self,
        now_ms: u64,
        kind: &'static str,
        handling: Duration,
        per_event: &[(&'static str, Duration)],
    ) -> Option<String> {
        let second = now_ms / 1000;
        let summary = match self.second {
            Some(current) if current != second => {
                let line = self.summary(current);
                *self = Self::default();
                Some(line)
            }
            _ => None,
        };
        self.second = Some(second);
        let timing = self.kinds.entry(kind).or_default();
        timing.count += 1;
        timing.total += handling;
        timing.max = timing.max.max(handling);
        for (name, spent) in per_event {
            *self.per_event.entry(name).or_default() += *spent;
        }
        summary
    }

    fn summary(&self, second: u64) -> String {
        let busy = self.kinds.values().map(|k| k.total).sum::<Duration>()
            + self.per_event.values().sum::<Duration>();
        let mut kinds: Vec<(&&str, &KindTiming)> = self.kinds.iter().collect();
        kinds.sort_by(|a, b| b.1.total.cmp(&a.1.total).then(a.0.cmp(b.0)));
        let kinds: Vec<String> = kinds
            .iter()
            .map(|(name, t)| {
                format!(
                    "{}:{}/{}/{}",
                    name,
                    t.count,
                    t.total.as_millis(),
                    t.max.as_millis()
                )
            })
            .collect();
        let per_event: Vec<String> = self
            .per_event
            .iter()
            .map(|(name, spent)| format!("{}:{}", name, spent.as_millis()))
            .collect();
        format!(
            "AEC_FACTS second={} busy_ms={} events={} per_event=[{}] kinds=[{}]",
            second,
            busy.as_millis(),
            self.kinds.values().map(|k| k.count).sum::<usize>(),
            per_event.join(" "),
            kinds.join(" ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_a_second_by_kind_once_the_next_one_starts() {
        let mut timings = FactTimings::default();

        assert!(
            timings
                .record(5_000, "vote", ms(2), &[("plugins", ms(1))])
                .is_none()
        );
        assert!(
            timings
                .record(5_100, "vote", ms(4), &[("plugins", ms(1))])
                .is_none()
        );
        assert!(
            timings
                .record(5_900, "epoch", ms(300), &[("plugins", ms(9))])
                .is_none()
        );
        let line = timings.record(6_000, "vote", ms(1), &[]).unwrap();

        assert_eq!(
            line,
            "AEC_FACTS second=5 busy_ms=317 events=3 per_event=[plugins:11] \
             kinds=[epoch:1/300/300 vote:2/6/4]"
        );
    }

    #[test]
    fn a_new_second_starts_from_scratch() {
        let mut timings = FactTimings::default();
        timings.record(5_000, "epoch", ms(300), &[]);
        timings.record(6_000, "vote", ms(1), &[]);

        let line = timings.record(7_000, "vote", ms(1), &[]).unwrap();

        assert_eq!(
            line,
            "AEC_FACTS second=6 busy_ms=1 events=1 per_event=[] kinds=[vote:1/1/1]"
        );
    }

    /* Test helpers */

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }
}
