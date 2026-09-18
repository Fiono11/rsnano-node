use std::{collections::HashMap, sync::Arc};

use rsnano_messages::{ConfirmReq, Message};
use rsnano_network::{Channel, ChannelId, TrafficType};
use rsnano_types::{BlockHash, ConsensusEpoch, Root};

use super::election::Election;
use crate::{representatives::PeeredRepInfo, transport::MessageFlooder};

/// This struct accepts elections that need further votes before they can be confirmed and bundles them in to confirm_req packets
pub struct ConfirmationSolicitor {
    /// Maximum amount of requests to be sent per election, bypassed if an existing vote is for a different hash
    max_election_requests: usize,
    representatives: Vec<PeeredRepInfo>,
    /// RAI: a request is for the elections of one epoch, so the requests are
    /// bundled per channel and epoch
    requests: HashMap<(ChannelId, ConsensusEpoch), (Arc<Channel>, Vec<(BlockHash, Root)>)>,
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
        // Kudzu: a terminated election asks every representative whose statements
        // it does not hold in full; each answers with its own small re-signed votes
        let terminated = cfg!(feature = "rai_protocol") && election.state().is_terminated();
        for rep in &self.representatives {
            if rep_request_count >= self.max_election_requests {
                break;
            }
            let mut full_queue = false;
            let existing_vote = election.votes().get(&rep.rep_key);
            let is_final = if let Some(vote) = existing_vote {
                // Kudzu: a representative's first vote is not enough, its second look
                // and final vote may be missing here while its election is already terminated
                if cfg!(feature = "rai_protocol") {
                    vote.is_final_vote()
                } else {
                    !election.has_quorum() || vote.is_final_vote()
                }
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
                        let (_, request_queue) = self
                            .requests
                            .entry((rep_channel.channel_id(), election.epoch()))
                            .or_insert_with(|| (rep_channel, Vec::new()));

                        request_queue.push((winner.hash(), winner.root()));

                        if !different_hash || terminated {
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
        for ((_, epoch), (channel, requests)) in &self.requests {
            let mut roots_hashes = Vec::new();
            for root_hash in requests {
                roots_hashes.push(*root_hash);
                if roots_hashes.len() == ConfirmReq::HASHES_MAX {
                    let req = Message::ConfirmReq(ConfirmReq::new_in_epoch(roots_hashes, *epoch));
                    self.message_flooder
                        .try_send(channel, &req, TrafficType::ConfirmationRequests);
                    roots_hashes = Vec::new();
                }
            }
            if !roots_hashes.is_empty() {
                let req = Message::ConfirmReq(ConfirmReq::new_in_epoch(roots_hashes, *epoch));
                self.message_flooder
                    .try_send(channel, &req, TrafficType::ConfirmationRequests);
            }
        }
        self.prepared = false;
    }
}
