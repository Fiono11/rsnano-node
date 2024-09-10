use rsnano_node::stats::{CounterEntry, CounterKey, Sample, SamplerEntry, SamplerKey, StatCategory};
use serde::{Deserialize, Serialize};
use crate::RpcCommand;

impl RpcCommand {
    pub fn stats(stats_category: StatCategory) -> Self {
        Self::Stats(StatsArgs::new(stats_category))
    }
}

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]

pub struct StatsArgs {
    #[serde(rename = "type")]
    pub stat_category: StatCategory
}

impl StatsArgs {
    pub fn new(stat_category: StatCategory) -> Self {
        Self { stat_category }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum StatsDto {
    #[serde(rename = "samples")]
    Samples {
        created: u64,
        entries: Vec<(SamplerKey, SamplerEntry)>,
        stat_duration_seconds: u64,
    },
    #[serde(rename = "counters")]
    Counters {
        created: u64,
        entries: Vec<(CounterKey, CounterEntry)>,
        stat_duration_seconds: u64,
    },
}

impl StatsDto {
    pub fn new_samples(created: u64, entries: Vec<(SamplerKey, SamplerEntry)>, stat_duration_seconds: u64) -> Self {
        Self::Samples {
            created,
            entries,
            stat_duration_seconds,
        }
    }

    pub fn new_counters(created: u64, entries: Vec<(CounterKey, CounterEntry)>, stat_duration_seconds: u64) -> Self {
        Self::Counters {
            created,
            entries,
            stat_duration_seconds,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_node::stats::{DetailType, Direction, StatType};
    use serde_json;

    #[test]
    fn test_serialize_deserialize_samples() {
        let sample_entries = vec![
            (
                SamplerKey::new(Sample::ActiveElectionDuration),
                SamplerEntry::new(1, (0, 600000)),
            ),
            (
                SamplerKey::new(Sample::RepResponseTime),
                SamplerEntry::new(3, (0, 60000)),
            ),
        ];

        let stats_dto = StatsDto::new_samples(
            1694371769, // Unix timestamp for "2024.09.10 19:29:29"
            sample_entries,
            1118,
        );

        let serialized = serde_json::to_string(&stats_dto).unwrap();
        let deserialized: StatsDto = serde_json::from_str(&serialized).unwrap();

        //assert_eq!(stats_dto, deserialized);

        if let StatsDto::Samples { created, entries, stat_duration_seconds } = deserialized {
            assert_eq!(created, 1694371769);
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].0.sample, Sample::ActiveElectionDuration);
            assert_eq!(entries[1].0.sample, Sample::RepResponseTime);
            assert_eq!(stat_duration_seconds, 1118);
        } else {
            panic!("Deserialized to wrong variant");
        }
    }

    #[test]
    fn test_serialize_deserialize_counters() {
        let counter_entries = vec![
            (
                CounterKey::new(StatType::TrafficTcp, DetailType::All, Direction::In),
                CounterEntry::new(),
            ),
        ];

        let stats_dto = StatsDto::new_counters(
            1694371798, // Unix timestamp for "2024.09.10 19:29:58"
            counter_entries,
            1147,
        );

        let serialized = serde_json::to_string(&stats_dto).unwrap();
        let deserialized: StatsDto = serde_json::from_str(&serialized).unwrap();

        //assert_eq!(stats_dto, deserialized);

        if let StatsDto::Counters { created, entries, stat_duration_seconds } = deserialized {
            assert_eq!(created, 1694371798);
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].0.stat_type, StatType::TrafficTcp);
            assert_eq!(entries[0].0.detail, DetailType::All);
            assert_eq!(entries[0].0.dir, Direction::In);
            assert_eq!(stat_duration_seconds, 1147);
        } else {
            panic!("Deserialized to wrong variant");
        }
    }
}