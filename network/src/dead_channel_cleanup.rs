use crate::{ChannelId, Network};
use rsnano_nullable_clock::SteadyClock;
use std::{
    ops::Deref,
    sync::{Arc, RwLock},
};
use tracing::debug;

pub trait DeadChannelCleanupStep: Send {
    fn clean_up_dead_channels(&self, dead_channel_ids: &[ChannelId]);
}

/// Removes dead channels and all their related queue entries
pub struct DeadChannelCleanup {
    clock: Arc<SteadyClock>,
    network: Arc<RwLock<Network>>,
    cleanup_steps: Vec<Box<dyn DeadChannelCleanupStep>>,
}

impl DeadChannelCleanup {
    pub fn new(clock: Arc<SteadyClock>, network: Arc<RwLock<Network>>) -> Self {
        Self {
            clock,
            network,
            cleanup_steps: Vec::new(),
        }
    }

    pub fn add_step(&mut self, step: impl DeadChannelCleanupStep + 'static) {
        self.cleanup_steps.push(Box::new(step));
    }

    pub fn clean_up(&self) {
        let removed_channels = self.network.write().unwrap().purge(self.clock.now());
        for channel in &removed_channels {
            debug!(
                remote_addr = ?channel.peer_addr(),
                channel_id = %channel.channel_id(),
                mode = ?channel.mode(),
                version = channel.protocol_version(),
                "Idle/dead channel closed");
        }

        let channel_ids: Vec<_> = removed_channels.iter().map(|c| c.channel_id()).collect();

        for step in &self.cleanup_steps {
            step.clean_up_dead_channels(&channel_ids);
        }
    }
}

impl<T> DeadChannelCleanupStep for Arc<T>
where
    T: DeadChannelCleanupStep + Sync,
{
    fn clean_up_dead_channels(&self, dead_channel_ids: &[ChannelId]) {
        self.deref().clean_up_dead_channels(dead_channel_ids)
    }
}
