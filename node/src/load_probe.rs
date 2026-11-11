use std::{
    fs,
    sync::{Arc, RwLock, atomic::Ordering},
    time::Instant,
};

use rsnano_network::Network;
use rsnano_network_protocol::InboundMessageQueue;
use rsnano_utils::{
    CancellationToken,
    stats::{DetailType, Direction, StatType, Stats},
    ticker::Tickable,
};

use crate::{
    block_processing::BlockProcessorQueue,
    cementation::ConfirmingSet,
    consensus::{AecService, RequestAggregator, VoteProcessor, VoteProcessorQueue},
};

/// RAI evaluation: one diagnostic line per second with the depth of the queues a
/// message passes through, how long the elections' lock took to get, and how
/// late this tick fired. It shows what a node falling behind is waiting for.
pub(crate) struct LoadProbe {
    pub(crate) inbound: Arc<InboundMessageQueue>,
    pub(crate) votes: Arc<VoteProcessorQueue>,
    pub(crate) processor: Arc<VoteProcessor>,
    pub(crate) requests: Arc<RequestAggregator>,
    pub(crate) blocks: Arc<BlockProcessorQueue>,
    pub(crate) cementing: Arc<ConfirmingSet>,
    pub(crate) aec: Arc<AecService>,
    pub(crate) network: Arc<RwLock<Network>>,
    pub(crate) stats: Arc<Stats>,
    pub(crate) last_tick: Option<Instant>,
}

impl Tickable for LoadProbe {
    fn tick(&mut self, _cancel_token: &CancellationToken) {
        let started = Instant::now();
        let tick_gap_ms = self
            .last_tick
            .map_or(0, |last| started.duration_since(last).as_millis());
        self.last_tick = Some(started);
        let elections = self.aec.len();
        let aec_wait_us = started.elapsed().as_micros();
        let locks = crate::consensus::take_lock_profile(10);
        if !locks.is_empty() {
            // line:r|w:count:hold_ms:max_hold_ms:wait_ms:max_wait_ms of aec_service.rs
            crate::utils::diagnostic!("AEC_LOCK {}", locks.join(" "));
        }
        let (out_max, out_sum) = self.outbound_queues();
        let lanes = self.votes.lane_lengths();
        crate::utils::diagnostic!(
            "NODE_LOAD gap_ms={} aec_wait_us={} elections={} inbound={} votes={} v_close={} v_own={} v_replay={} v_net={} v_done={} cooling={} requests={} blocks={} cementing={} out_max={} out_sum={} cpu_ticks={} asked={} votes_in={} votes_dropped={}",
            tick_gap_ms,
            aec_wait_us,
            elections,
            self.inbound.size(),
            self.votes.len(),
            lanes.close,
            lanes.own,
            lanes.replays,
            lanes.network,
            self.processor.total_processed.load(Ordering::Relaxed),
            self.processor.is_cooling_down() as u8,
            self.requests.len(),
            self.blocks.total_queue_len(),
            self.cementing.len(),
            out_max,
            out_sum,
            process_cpu_ticks().unwrap_or(0),
            self.stats.count(
                StatType::Election,
                DetailType::ConfirmationRequest,
                Direction::In
            ),
            self.stats
                .count(StatType::Message, DetailType::ConfirmAck, Direction::In),
            self.stats
                .count(StatType::VoteProcessor, DetailType::Overfill, Direction::In),
        );
    }
}

impl LoadProbe {
    fn outbound_queues(&self) -> (usize, usize) {
        let network = self.network.read().unwrap();
        let lengths = network.channels().map(|channel| channel.queue_len());
        lengths.fold((0, 0), |(max, sum), length| (max.max(length), sum + length))
    }
}

/// User plus system CPU time of this process in clock ticks, on Linux
fn process_cpu_ticks() -> Option<u64> {
    parse_cpu_ticks(&fs::read_to_string("/proc/self/stat").ok()?)
}

/// The process name may hold spaces, so the fields count from after its `)`:
/// utime and stime are the 12th and 13th of them
fn parse_cpu_ticks(stat: &str) -> Option<u64> {
    let mut fields = stat.rsplit_once(')')?.1.split_whitespace().skip(11);
    let user: u64 = fields.next()?.parse().ok()?;
    let system: u64 = fields.next()?.parse().ok()?;
    Some(user + system)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_ticks_count_from_after_the_process_name() {
        let stat = "1234 (rs nano) S 1 2 3 4 5 6 7 8 9 10 111 222 13 14";
        assert_eq!(parse_cpu_ticks(stat), Some(333));
    }

    #[test]
    fn a_malformed_stat_has_no_cpu_ticks() {
        assert_eq!(parse_cpu_ticks("no parenthesis"), None);
        assert_eq!(parse_cpu_ticks("1 (x) S 1"), None);
    }
}
