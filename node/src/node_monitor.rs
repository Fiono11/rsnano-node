use std::{
    sync::{Arc, RwLock},
    time::Duration,
};

use num_format::{Locale, ToFormattedString};
use tracing::{info, warn};

use rsnano_ledger::Ledger;
use rsnano_network::Network;
use rsnano_utils::{CancellationToken, ticker::Tickable};

use crate::{
    block_rate_calculator::CurrentBlockRates,
    consensus::AecService,
    representatives::{QuorumSnapshot, RepresentativeTracker},
};
use rsnano_nullable_clock::{SteadyClock, Timestamp};

/// Periodically prints info about BPS, CPS, elections, peers,...
pub struct NodeMonitor {
    clock: SteadyClock,
    ledger: Arc<Ledger>,
    network: Arc<RwLock<Network>>,
    rep_tracker: Arc<RepresentativeTracker>,
    aec: Arc<AecService>,
    block_rates: Arc<CurrentBlockRates>,
    first_log: Option<Timestamp>,
}

impl NodeMonitor {
    pub fn new(
        ledger: Arc<Ledger>,
        network: Arc<RwLock<Network>>,
        rep_tracker: Arc<RepresentativeTracker>,
        aec: Arc<AecService>,
        block_rates: Arc<CurrentBlockRates>,
    ) -> Self {
        Self {
            clock: SteadyClock::default(),
            ledger,
            network,
            rep_tracker,
            aec,
            block_rates,
            first_log: None,
        }
    }

    fn log(&mut self) {
        if self.first_log.is_none() {
            self.first_log = Some(self.clock.now());
        }
        let blocks_confirmed = self.ledger.confirmed_count();
        let blocks_total = self.ledger.block_count();
        let channels = self.network.read().unwrap().channels_info();
        let quorum = self.rep_tracker.quorum_snapshot();

        log_channels(channels);
        log_quorum(quorum, self.warmed_up());
        log_elections(self.aec.info());
        log_blocks(blocks_confirmed, blocks_total);
        log_block_rate(self.block_rates.bps(), self.block_rates.cps());
    }

    fn warmed_up(&self) -> bool {
        let Some(start) = self.first_log else {
            return false;
        };
        start.elapsed(self.clock.now()) >= Duration::from_mins(5)
    }
}

fn log_block_rate(bps: i64, cps: i64) {
    info!("Blocks rate: {} bps | {} cps)", bps, cps);
}

fn log_blocks(blocks_confirmed: u64, blocks_total: u64) {
    info!(
        "Blocks confirmed: {} | total: {} (backlog: {})",
        blocks_confirmed.to_formatted_string(&Locale::en),
        blocks_total.to_formatted_string(&Locale::en),
        (blocks_total - blocks_confirmed).to_formatted_string(&Locale::en)
    );
}

fn log_quorum(quorum: QuorumSnapshot, warmed_up: bool) {
    info!(
        "Quorum: {} (stake peered: {} | online stake: {})",
        quorum.quorum_delta.format_balance(0),
        quorum.peered_weight.format_balance(0),
        quorum.online_weight.format_balance(0)
    );

    if warmed_up && quorum.peered_weight < quorum.quorum_delta {
        warn!(
            "Peered stake ({}) is below quorum threshold ({}). \
            The node may not be able to confirm transactions. \
            This is usually caused by NAT, firewall rules, or internet connectivity issues.",
            quorum.peered_weight.format_balance(0),
            quorum.quorum_delta.format_balance(0)
        )
    }
}

fn log_channels(channels: rsnano_network::ChannelsInfo) {
    info!(
        "Peers: {} (established: {} | inbound: {} | outbound: {})",
        channels.total, channels.established, channels.inbound, channels.outbound
    );
}

fn log_elections(elections: crate::consensus::ActiveElectionsInfo) {
    info!(
        "Elections active: {} (priority: {} | hinted: {} | optimistic: {}) of which stale: {}",
        elections.total,
        elections.priority,
        elections.hinted,
        elections.optimistic,
        elections.stale
    );
}

impl Tickable for NodeMonitor {
    fn tick(&mut self, _cancel_token: &CancellationToken) {
        self.log();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::ActiveElectionsInfo;
    use tracing_test::traced_test;

    #[test]
    #[traced_test]
    fn test_log_elections() {
        let info = ActiveElectionsInfo {
            total: 10,
            stale: 3,
            priority: 5,
            hinted: 4,
            optimistic: 1,
            ..Default::default()
        };

        log_elections(info);
        assert!(logs_contain(
            "Elections active: 10 (priority: 5 | hinted: 4 | optimistic: 1) of which stale: 3"
        ));
    }
}
