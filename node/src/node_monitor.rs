use std::{
    sync::{Arc, RwLock},
    time::Instant,
};

use num_format::{Locale, ToFormattedString};
use tracing::info;

use rsnano_ledger::Ledger;
use rsnano_network::Network;
use rsnano_utils::{CancellationToken, ticker::Tickable};

use crate::{
    block_rate_calculator::CurrentBlockRates, consensus::AecService,
    representatives::RepresentativeTracker,
};

/// Periodically prints info about BPS, CPS, elections, peers,...
pub struct NodeMonitor {
    ledger: Arc<Ledger>,
    network: Arc<RwLock<Network>>,
    rep_tracker: Arc<RepresentativeTracker>,
    active_elections: Arc<AecService>,
    block_rates: Arc<CurrentBlockRates>,
    last_time: Option<Instant>,
}

impl NodeMonitor {
    pub fn new(
        ledger: Arc<Ledger>,
        network: Arc<RwLock<Network>>,
        rep_tracker: Arc<RepresentativeTracker>,
        active_elections: Arc<AecService>,
        block_rates: Arc<CurrentBlockRates>,
    ) -> Self {
        Self {
            ledger,
            network,
            rep_tracker,
            active_elections,
            block_rates,
            last_time: None,
        }
    }

    fn log(&self) {
        let blocks_confirmed = self.ledger.confirmed_count();
        let blocks_total = self.ledger.block_count();
        let channels = self.network.read().unwrap().channels_info();
        log_channels(channels);
        log_quorum(self.rep_tracker.quorum_snapshot());
        log_elections(self.active_elections.info());
        log_blocks(blocks_confirmed, blocks_total);
        log_block_rate(self.block_rates.bps(), self.block_rates.cps());
    }
}

fn log_block_rate(blocks_checked_rate: i64, blocks_confirmed_rate: i64) {
    info!(
        "Blocks rate: {} bps | {} cps)",
        blocks_checked_rate, blocks_confirmed_rate,
    );
}

fn log_blocks(blocks_confirmed: u64, blocks_total: u64) {
    info!(
        "Blocks confirmed: {} | total: {} (backlog: {})",
        blocks_confirmed.to_formatted_string(&Locale::en),
        blocks_total.to_formatted_string(&Locale::en),
        (blocks_total - blocks_confirmed).to_formatted_string(&Locale::en)
    );
}

fn log_quorum(quorum: crate::representatives::QuorumSnapshot) {
    info!(
        "Quorum: {} (stake peered: {} | online stake: {})",
        quorum.quorum_delta.format_balance(0),
        quorum.peered_weight.format_balance(0),
        quorum.online_weight.format_balance(0)
    );
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
        if self.last_time.is_some() {
            self.log();
        } else {
            // Wait for node to warm up before logging
        }
        self.last_time = Some(Instant::now());
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
