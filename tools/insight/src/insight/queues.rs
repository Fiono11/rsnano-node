use std::{fmt::Debug, hash::Hash};

use num_format::{Locale, ToFormattedString};
use strum::IntoEnumIterator;

use rsnano_utils::fair_queue::{FairQueueInfo, QueueInfo};

use crate::insight::gui::PaletteColor;
use rsnano_node::Node;

pub(crate) fn create_queue_groups(node: &Node) -> Vec<QueueGroupViewModel> {
    let aec_info = node.aec.info();
    let confirming_set = node.confirming_set.info();
    vec![
        QueueGroupViewModel {
            heading: "Active Elections".to_string(),
            queues: vec![
                QueueViewModel::new("Priority", aec_info.priority, aec_info.max_elections),
                QueueViewModel::new(
                    "Hinted",
                    aec_info.hinted,
                    node.election_schedulers.hinted.max_elections,
                ),
                QueueViewModel::new(
                    "Optimistic",
                    aec_info.optimistic,
                    node.election_schedulers.optimistic.max_elections(),
                ),
                QueueViewModel::new("Total", aec_info.total, aec_info.max_elections),
            ],
        },
        QueueGroupViewModel::for_fair_queue("Block Processor", &node.block_processor_queue.info()),
        QueueGroupViewModel::for_fair_queue("Vote Processor", &node.vote_processor_queue.info()),
        QueueGroupViewModel {
            heading: "Miscellaneous".to_string(),
            queues: vec![QueueViewModel::new(
                "Confirming",
                confirming_set.size,
                confirming_set.max_size,
            )],
        },
    ]
}

pub(crate) struct QueueGroupViewModel {
    pub heading: String,
    pub queues: Vec<QueueViewModel>,
}

impl QueueGroupViewModel {
    pub fn for_fair_queue<T, H>(heading: H, info: &FairQueueInfo<T>) -> Self
    where
        T: Clone + Hash + Eq + IntoEnumIterator + Debug,
        H: Into<String>,
    {
        let mut queues = Vec::new();

        for source in T::iter() {
            let info = info
                .queues
                .get(&source)
                .cloned()
                .unwrap_or_else(|| QueueInfo {
                    source,
                    size: 0,
                    max_size: 0,
                });
            let label = format!("{:?}", info.source);
            queues.push(QueueViewModel::new(label, info.size, info.max_size));
        }

        queues.push(QueueViewModel::new(
            "Total",
            info.total_size,
            info.total_max_size,
        ));

        QueueGroupViewModel {
            heading: heading.into(),
            queues,
        }
    }
}

pub(crate) struct QueueViewModel {
    pub label: String,
    pub value: String,
    pub max: String,
    pub progress: f32,
    pub color: PaletteColor,
}

impl QueueViewModel {
    pub fn new(label: impl Into<String>, value: usize, max: usize) -> Self {
        let progress = value as f32 / max as f32;
        QueueViewModel {
            label: label.into(),
            value: value.to_formatted_string(&Locale::en),
            max: max.to_formatted_string(&Locale::en),
            progress,
            color: match progress {
                0.75.. => PaletteColor::Red1,
                0.5..0.75 => PaletteColor::Orange1,
                _ => PaletteColor::Blue1,
            },
        }
    }
}
