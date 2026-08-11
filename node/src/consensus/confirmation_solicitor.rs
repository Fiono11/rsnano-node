use std::{collections::HashMap, sync::Arc};

use rsnano_messages::{ConfirmReq, Message};
use rsnano_network::{Channel, ChannelId, TrafficType};
use rsnano_types::{BlockHash, Root};

#[cfg(feature = "rai_protocol")]
use rsnano_types::{PublicKey, RaiCommitteeScope, RaiVotePhase};

use super::election::Election;
use crate::{representatives::PeeredRepInfo, transport::MessageFlooder};

/// This struct accepts elections that need further votes before they can be confirmed and bundles them in to confirm_req packets
pub struct ConfirmationSolicitor {
    /// Maximum amount of requests to be sent per election, bypassed if an existing vote is for a different hash
    max_election_requests: usize,
    representatives: Vec<PeeredRepInfo>,
    requests: HashMap<ChannelId, (Arc<Channel>, Vec<(BlockHash, Root)>)>,
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
        #[cfg(feature = "rai_protocol")]
        let requested_hash = election.voting_hash();
        #[cfg(not(feature = "rai_protocol"))]
        let requested_hash = election.winner().hash();
        let mut to_remove = Vec::new();
        for rep in &self.representatives {
            if rep_request_count >= self.max_election_requests {
                break;
            }
            let mut full_queue = false;
            let existing_vote = election.votes().get(&rep.rep_key);
            #[cfg(feature = "rai_protocol")]
            let needs_vote =
                !rai_rep_has_requested_evidence(election, &rep.rep_key, requested_hash);
            #[cfg(not(feature = "rai_protocol"))]
            let needs_vote = existing_vote.is_none()
                || !existing_vote
                    .map(|vote| !election.has_quorum() || vote.is_final_vote())
                    .unwrap_or(false);
            let different_hash = if let Some(existing) = existing_vote {
                existing.hash != requested_hash
            } else {
                false
            };
            if needs_vote || different_hash {
                if let Some(rep_channel) = self.message_flooder.channel(rep.channel_id) {
                    let should_drop = rep_channel.should_drop(TrafficType::ConfirmationRequests);

                    if !should_drop {
                        let (_, request_queue) = self
                            .requests
                            .entry(rep_channel.channel_id())
                            .or_insert_with(|| (rep_channel, Vec::new()));

                        #[cfg(feature = "rai_protocol")]
                        let request_hash = if election.is_rai_close() {
                            BlockHash::ZERO
                        } else {
                            requested_hash
                        };
                        #[cfg(not(feature = "rai_protocol"))]
                        let request_hash = requested_hash;
                        request_queue.push((request_hash, election.qualified_root().root));

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
        for (channel, requests) in self.requests.values() {
            let mut roots_hashes = Vec::new();
            for root_hash in requests {
                roots_hashes.push(*root_hash);
                if roots_hashes.len() == ConfirmReq::HASHES_MAX {
                    let req = Message::ConfirmReq(ConfirmReq::new(roots_hashes));
                    self.message_flooder
                        .try_send(channel, &req, TrafficType::ConfirmationRequests);
                    roots_hashes = Vec::new();
                }
            }
            if !roots_hashes.is_empty() {
                let req = Message::ConfirmReq(ConfirmReq::new(roots_hashes));
                #[cfg(feature = "rai_protocol")]
                if std::env::var_os("RSNANO_RAI_TRACE_PR").is_some() {
                    eprintln!(
                        "RAI_SOLICIT_TRACE send_confirm_req channel={:?} requests={:?}",
                        channel.channel_id(),
                        requests
                    );
                }
                self.message_flooder
                    .try_send(channel, &req, TrafficType::ConfirmationRequests);
            }
        }
        self.prepared = false;
    }
}

/// Whether this representative already contributes the evidence currently
/// requested by the RAI election in every committee where it has weight.
///
/// The legacy `VoteSummary` projection retains only a representative's latest
/// non-timeout vote. In particular, a Final projection does not prove that an
/// earlier First leaf was received: RAI permits Final with empty prior support
/// and accepts a compatible delayed First afterwards. Solicitation therefore
/// has to inspect the phase-specific certificate state instead.
#[cfg(feature = "rai_protocol")]
fn rai_rep_has_requested_evidence(
    election: &Election,
    representative: &PublicKey,
    requested_hash: BlockHash,
) -> bool {
    use crate::consensus::rai::BlockHashOrTimeout;

    let metadata = election.rai_vote_metadata();
    let value = if requested_hash.is_zero() && metadata.phase != RaiVotePhase::Final {
        BlockHashOrTimeout::Timeout
    } else {
        BlockHashOrTimeout::Block(requested_hash)
    };
    let committee_count = election.rai_votes.committees.len();

    election
        .rai_votes
        .committees
        .iter()
        .enumerate()
        .filter(|(index, committee)| {
            let in_scope = match metadata.scope {
                RaiCommitteeScope::All => true,
                RaiCommitteeScope::Older => *index == 0,
                RaiCommitteeScope::Newer => *index + 1 == committee_count,
            };
            in_scope && !committee.weights.weight(representative).is_zero()
        })
        .all(|(_, committee)| match metadata.phase {
            RaiVotePhase::First => committee.votes.first.get(representative) == Some(&value),
            RaiVotePhase::Notar => {
                committee.votes.first.get(representative) == Some(&value)
                    || committee
                        .votes
                        .notar
                        .get(representative)
                        .is_some_and(|values| values.contains(&value))
            }
            RaiVotePhase::Final => match value {
                BlockHashOrTimeout::Block(hash) => {
                    committee.votes.final_votes.get(representative) == Some(&hash)
                }
                BlockHashOrTimeout::Timeout => false,
            },
        })
}

#[cfg(all(test, feature = "rai_protocol"))]
mod tests {
    use super::*;
    use crate::consensus::{
        election::ElectionBehavior,
        rai::{BlockHashOrTimeout, RaiOutcome},
    };
    use rsnano_ledger::RepWeights;
    use rsnano_nullable_clock::Timestamp;
    use rsnano_types::{Amount, PrivateKey, RaiEpoch, SavedBlock};
    use std::{sync::Arc, time::Duration};

    #[test]
    fn final_without_first_is_solicited_until_delayed_first_completes_notar_support() {
        let keys = (1..=6).map(PrivateKey::from).collect::<Vec<_>>();
        let committee = Arc::new(RepWeights::from([
            (keys[0].public_key(), Amount::raw(1)),
            (keys[1].public_key(), Amount::raw(1)),
            (keys[2].public_key(), Amount::raw(1)),
            (keys[3].public_key(), Amount::raw(1)),
            (keys[4].public_key(), Amount::raw(1)),
            (keys[5].public_key(), Amount::raw(1)),
        ]));
        let block = SavedBlock::new_test_instance();
        let hash = block.hash();
        let mut election = Election::new_slot(
            block,
            ElectionBehavior::Priority,
            Duration::from_secs(1),
            Timestamp::new_test_instance(),
            RaiEpoch::ZERO,
        )
        .with_rai_committees(vec![committee]);
        let rep_weights = RepWeights::default();

        // Final does not imply any First/Notar support in RAI. Three other
        // First votes make Notar the current phase, but remain one short of a
        // notarization certificate.
        election
            .rai_votes
            .record_final_vote(keys[0].public_key(), hash, RaiCommitteeScope::All)
            .unwrap();
        for key in keys.iter().skip(1).take(3) {
            election
                .rai_votes
                .record_first_vote(
                    key.public_key(),
                    BlockHashOrTimeout::Block(hash),
                    RaiCommitteeScope::All,
                )
                .unwrap();
        }
        election.update_tallies(&rep_weights, Amount::ZERO);

        assert_eq!(election.rai_vote_metadata().phase, RaiVotePhase::Notar);
        assert!(!rai_rep_has_requested_evidence(
            &election,
            &keys[0].public_key(),
            election.voting_hash(),
        ));

        // The compatible First leaf can arrive after Final. It supplies the
        // fourth notarization weight, after which this signer's existing Final
        // is exactly the evidence required by the next phase.
        election
            .rai_votes
            .record_first_vote(
                keys[0].public_key(),
                BlockHashOrTimeout::Block(hash),
                RaiCommitteeScope::All,
            )
            .unwrap();
        election.update_tallies(&rep_weights, Amount::ZERO);

        assert_eq!(election.rai_votes.outcome, RaiOutcome::Notarized(hash));
        assert_eq!(election.rai_vote_metadata().phase, RaiVotePhase::Final);
        assert!(rai_rep_has_requested_evidence(
            &election,
            &keys[0].public_key(),
            election.voting_hash(),
        ));
    }

    #[test]
    fn representative_outside_all_applicable_committees_is_not_solicited() {
        let member = PrivateKey::from(1);
        let outsider = PrivateKey::from(2);
        let committee = Arc::new(RepWeights::from([(member.public_key(), Amount::raw(1))]));
        let block = SavedBlock::new_test_instance();
        let election = Election::new_slot(
            block,
            ElectionBehavior::Priority,
            Duration::from_secs(1),
            Timestamp::new_test_instance(),
            RaiEpoch::ZERO,
        )
        .with_rai_committees(vec![committee]);

        assert!(rai_rep_has_requested_evidence(
            &election,
            &outsider.public_key(),
            election.voting_hash(),
        ));
    }
}
