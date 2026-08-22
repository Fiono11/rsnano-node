use std::{collections::HashMap, sync::Arc};

use rsnano_messages::{ConfirmReq, Message};
use rsnano_network::{Channel, ChannelId, TrafficType};
use rsnano_types::{BlockHash, Root};

use super::election::Election;
use crate::{representatives::PeeredRepInfo, transport::MessageFlooder};

#[cfg(not(feature = "rai_protocol"))]
type RequestKey = ChannelId;
#[cfg(feature = "rai_protocol")]
type RequestKey = (ChannelId, u64);

/// This struct accepts elections that need further votes before they can be confirmed and bundles them in to confirm_req packets
pub struct ConfirmationSolicitor {
    /// Maximum amount of requests to be sent per election, bypassed if an existing vote is for a different hash
    max_election_requests: usize,
    representatives: Vec<PeeredRepInfo>,
    requests: HashMap<RequestKey, RequestQueue>,
    prepared: bool,
    message_flooder: MessageFlooder,
}

impl ConfirmationSolicitor {
    pub fn new(message_flooder: MessageFlooder) -> Self {
        Self {
            max_election_requests: 50,
            prepared: false,
            representatives: Vec::new(),
            requests: HashMap::new(),
            message_flooder,
        }
    }

    /// Prepare object for batching election confirmation requests
    pub fn prepare(&mut self, representatives: &[PeeredRepInfo]) {
        debug_assert!(!self.prepared);
        self.requests.clear();
        self.representatives = representatives.to_vec();
        self.prepared = true;
    }

    /// Add an election that needs to be confirmed. Returns true if successfully added
    pub fn add(&mut self, election: &Election) -> bool {
        debug_assert!(self.prepared);
        let mut added = false;
        let mut rep_request_count = 0;
        let winner = election.winner();
        let mut to_remove = Vec::new();
        for rep in &self.representatives {
            if rep_request_count >= self.max_election_requests {
                break;
            }
            let mut full_queue = false;
            let existing_vote = election.votes().get(&rep.rep_key);
            let is_final = if let Some(vote) = existing_vote {
                !election.has_quorum() || vote.is_final_vote()
            } else {
                false
            };
            let different_hash = if let Some(existing) = existing_vote {
                existing.hash != winner.hash()
            } else {
                false
            };
            if existing_vote.is_none() || !is_final || different_hash {
                if let Some(rep_channel) = self.message_flooder.channel(rep.channel_id) {
                    let should_drop = rep_channel.should_drop(TrafficType::ConfirmationRequests);

                    if !should_drop {
                        #[cfg(not(feature = "rai_protocol"))]
                        let request_key = rep_channel.channel_id();
                        #[cfg(feature = "rai_protocol")]
                        let request_key =
                            (rep_channel.channel_id(), election.qualified_root().epoch);
                        let queue =
                            self.requests
                                .entry(request_key)
                                .or_insert_with(|| RequestQueue {
                                    channel: rep_channel,
                                    requests: Vec::new(),
                                    #[cfg(feature = "rai_protocol")]
                                    epoch: election.qualified_root().epoch,
                                });

                        queue.requests.push((winner.hash(), winner.root()));

                        if !different_hash {
                            rep_request_count += 1;
                        }
                        added = true;
                    } else {
                        full_queue = true;
                    }
                }
            }
            if full_queue {
                to_remove.push(rep.rep_key);
            }
        }

        if !to_remove.is_empty() {
            self.representatives
                .retain(|i| !to_remove.contains(&i.rep_key));
        }

        added
    }

    /// Dispatch bundled requests to each channel
    pub fn flush(&mut self) {
        debug_assert!(self.prepared);
        for queue in self.requests.values() {
            let mut roots_hashes = Vec::new();
            for root_hash in &queue.requests {
                roots_hashes.push(*root_hash);
                if roots_hashes.len() == ConfirmReq::HASHES_MAX {
                    let request = ConfirmReq::new(roots_hashes);
                    #[cfg(feature = "rai_protocol")]
                    let request = request.with_epoch(queue.epoch);
                    let req = Message::ConfirmReq(request);
                    self.message_flooder.try_send(
                        &queue.channel,
                        &req,
                        TrafficType::ConfirmationRequests,
                    );
                    roots_hashes = Vec::new();
                }
            }
            if !roots_hashes.is_empty() {
                let request = ConfirmReq::new(roots_hashes);
                #[cfg(feature = "rai_protocol")]
                let request = request.with_epoch(queue.epoch);
                let req = Message::ConfirmReq(request);
                self.message_flooder.try_send(
                    &queue.channel,
                    &req,
                    TrafficType::ConfirmationRequests,
                );
            }
        }
        self.prepared = false;
    }
}

struct RequestQueue {
    channel: Arc<Channel>,
    requests: Vec<(BlockHash, Root)>,
    #[cfg(feature = "rai_protocol")]
    epoch: u64,
}
