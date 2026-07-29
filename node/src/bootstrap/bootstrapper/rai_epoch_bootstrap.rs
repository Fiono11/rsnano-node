use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

use rsnano_network::ChannelId;
use rsnano_types::{
    BlockHash, RAI_EPOCH_CLOSE_PAGE_MAX_ENTRIES, RaiEpoch, RaiEpochCloseAck, RaiEpochCloseEntry,
    RaiEpochCloseEntryState, RaiEpochClosePage, RaiEpochCloseReq, RaiSlot, SavedBlock,
};

use crate::consensus::{
    CloseRecordEntries, RaiCloseState, RaiClosedSlotState, RaiCommitteeProvider, RaiEpochPhase,
    RaiStatePersistence, VisibleSlots,
};

/// Separate attempt namespace used when the terminal historical committee
/// reissues votes for a joining replica.
pub(crate) const RAI_CLOSE_RECORD_RECOVERY_ATTEMPT: u64 = u64::MAX;

/// Supplies the ordinary ledger blocks referenced by a historical close package.
pub(crate) trait RaiEpochBlockRequester: Send + Sync {
    fn request_confirmed_slots(&self, slots: &[RaiSlot], source: Option<ChannelId>);

    fn confirmed_block_hash(&self, _slot: &RaiSlot) -> Option<BlockHash> {
        None
    }
}

/// Bridges ordinary block bootstrap into RAI's historical close-state install path.
///
/// A joining representative can learn old confirmed blocks through ascending bootstrap after the
/// live RAI votes that originally closed those epochs have already passed by. Those blocks should
/// still be usable as the finalized evidence for an already-installed old close cut, otherwise the
/// node stays pinned to epoch 0 waiting for slot outcomes it will never produce locally.
pub(crate) struct RaiEpochBootstrap {
    close_state: Arc<RwLock<RaiCloseState>>,
    committee_provider: Arc<dyn RaiCommitteeProvider>,
    persistence: Arc<dyn RaiStatePersistence>,
    block_requester: Arc<dyn RaiEpochBlockRequester>,
    confirmed_slots: Mutex<BTreeMap<RaiEpoch, BTreeMap<RaiSlot, BlockHash>>>,
    close_state_pages: Mutex<BTreeMap<RaiEpoch, RaiEpochClosePageAccumulator>>,
    close_state_requests: Mutex<BTreeMap<RaiEpoch, Instant>>,
    close_state_sources: Mutex<BTreeMap<RaiEpoch, ChannelId>>,
    target_epoch: Mutex<Option<RaiEpoch>>,
}

impl RaiEpochBootstrap {
    const CLOSE_STATE_REQUEST_COOLDOWN: Duration = Duration::from_secs(1);

    pub(crate) fn new(
        close_state: Arc<RwLock<RaiCloseState>>,
        committee_provider: Arc<dyn RaiCommitteeProvider>,
        persistence: Arc<dyn RaiStatePersistence>,
        block_requester: Arc<dyn RaiEpochBlockRequester>,
    ) -> Self {
        Self {
            close_state,
            committee_provider,
            persistence,
            block_requester,
            confirmed_slots: Mutex::new(BTreeMap::new()),
            close_state_pages: Mutex::new(BTreeMap::new()),
            close_state_requests: Mutex::new(BTreeMap::new()),
            close_state_sources: Mutex::new(BTreeMap::new()),
            target_epoch: Mutex::new(None),
        }
    }

    pub(crate) fn record_confirmed_blocks(&self, confirmed: &[(SavedBlock, BlockHash)]) {
        if confirmed.is_empty() {
            return;
        }

        let epoch = self.close_state.read().unwrap().current_epoch();
        let slots = confirmed
            .iter()
            .map(|(block, _)| (RaiSlot::new(block.account(), block.height()), block.hash()))
            .collect::<Vec<_>>();

        {
            let mut confirmed_slots = self.confirmed_slots.lock().unwrap();
            let epoch_slots = confirmed_slots.entry(epoch).or_default();
            for (slot, block) in slots {
                epoch_slots.insert(slot, block);
            }
        }

        self.try_install_available_epochs();
    }

    pub(crate) fn try_install_available_epochs(&self) {
        // A passively observed cut does not contain the finalized/released
        // outcome of each slot. Only reconstruct from the certified close
        // package supplied by the historical committee; otherwise replicas
        // can manufacture different record hashes from their transient local
        // block views and accidentally start a recovery record election for
        // the wrong value.
        while self.try_install_collected_current_epoch() {}
    }

    pub(crate) fn next_close_state_request(&self) -> Option<RaiEpochCloseReq> {
        let epoch = {
            let close_state = self.close_state.read().unwrap();
            let epoch = close_state.current_epoch();
            if close_state.epoch_phase(epoch) != Some(RaiEpochPhase::Closing) {
                return None;
            }
            epoch
        };

        let start_index = {
            let pages = self.close_state_pages.lock().unwrap();
            match pages.get(&epoch) {
                // Keep polling page zero after reconstruction. Besides serving
                // data, the request asks historical members to re-vote; the
                // requester may have missed an earlier broadcast.
                Some(accumulator) if accumulator.is_complete() => 0,
                Some(accumulator) => accumulator.next_missing_start_index(),
                None => 0,
            }
        };

        let now = Instant::now();
        {
            let mut requests = self.close_state_requests.lock().unwrap();
            if requests.get(&epoch).is_some_and(|sent_at| {
                now.duration_since(*sent_at) < Self::CLOSE_STATE_REQUEST_COOLDOWN
            }) {
                return None;
            }
            requests.insert(epoch, now);
        }

        Some(RaiEpochCloseReq {
            epoch,
            start_index,
            max_entries: RAI_EPOCH_CLOSE_PAGE_MAX_ENTRIES,
        })
    }

    pub(crate) fn process_epoch_close_ack(
        &self,
        ack: RaiEpochCloseAck,
        channel_id: ChannelId,
    ) -> bool {
        let Some(page) = ack.page else {
            return true;
        };

        let epoch = page.epoch;
        let advertised_target = page.server_epoch.saturating_sub(2);
        {
            let mut target = self.target_epoch.lock().unwrap();
            *target = Some(target.unwrap_or_default().max(advertised_target));
        }
        if !self.accept_epoch_close_page(page) {
            return false;
        }
        self.close_state_sources
            .lock()
            .unwrap()
            .insert(epoch, channel_id);

        self.try_install_available_epochs();
        true
    }

    fn accept_epoch_close_page(&self, page: RaiEpochClosePage) -> bool {
        let current_epoch = self.close_state.read().unwrap().current_epoch();
        if page.epoch < current_epoch {
            return true;
        }
        if page.epoch > current_epoch {
            return false;
        }

        let accepted = self
            .close_state_pages
            .lock()
            .unwrap()
            .entry(page.epoch)
            .or_default()
            .insert_page(page);
        if accepted {
            tracing::debug!(
                epoch = current_epoch,
                "RAI bootstrap accepted epoch close state page"
            );
        }
        accepted
    }

    fn try_install_collected_current_epoch(&self) -> bool {
        let (epoch, close_hash, entries, server_epoch) = {
            let close_state = self.close_state.read().unwrap();
            let epoch = close_state.current_epoch();
            if close_state.epoch_phase(epoch) != Some(RaiEpochPhase::Closing) {
                return false;
            }

            let pages = self.close_state_pages.lock().unwrap();
            let Some(accumulator) = pages.get(&epoch) else {
                return false;
            };
            let Some((close_hash, entries, server_epoch)) = accumulator.complete_entries() else {
                return false;
            };
            (epoch, close_hash, entries, server_epoch)
        };

        let target_epoch = server_epoch.saturating_sub(2);
        if epoch > target_epoch {
            return false;
        }

        let close_entries = match close_entries_from_epoch_page_entries(&entries) {
            Ok(entries) => entries,
            Err(()) => {
                tracing::warn!(
                    epoch,
                    "RAI bootstrap rejected epoch close state with duplicate slots"
                );
                self.close_state_pages.lock().unwrap().remove(&epoch);
                return false;
            }
        };

        match self.missing_committed_slots(epoch, &close_entries) {
            Ok(missing_slots) if !missing_slots.is_empty() => {
                self.request_missing_confirmed_slots(epoch, &missing_slots);
                return false;
            }
            Err(slot) => {
                tracing::warn!(
                    epoch,
                    account = %slot.account,
                    height = slot.account_height,
                    "RAI bootstrap rejected epoch close state with conflicting finalized slot"
                );
                self.close_state_pages.lock().unwrap().remove(&epoch);
                return false;
            }
            Ok(_) => {}
        }

        let terminal = epoch == target_epoch;
        let (advanced, snapshot) = {
            let mut close_state = self.close_state.write().unwrap();
            if close_state.current_epoch() != epoch
                || close_state.epoch_phase(epoch) != Some(RaiEpochPhase::Closing)
            {
                return false;
            }

            let proof_cut = close_entries.keys().copied().collect::<VisibleSlots>();
            if close_state
                .cut_set(epoch)
                .is_some_and(|cut| cut != &proof_cut)
            {
                tracing::warn!(epoch, "RAI bootstrap rejected conflicting epoch close cut");
                self.close_state_pages.lock().unwrap().remove(&epoch);
                return false;
            }

            for (slot, state) in &close_entries {
                if close_state
                    .closed_slot_state(epoch, slot)
                    .is_some_and(|existing| existing != state)
                {
                    tracing::warn!(
                        epoch,
                        account = %slot.account,
                        height = slot.account_height,
                        "RAI bootstrap rejected conflicting closed slot state"
                    );
                    self.close_state_pages.lock().unwrap().remove(&epoch);
                    return false;
                }
            }

            let Ok(computed_close_hash) =
                close_state.close_record_hash_from_entries(epoch, &close_entries)
            else {
                return false;
            };
            if computed_close_hash != close_hash {
                tracing::warn!(
                    epoch,
                    expected = %close_hash,
                    computed = %computed_close_hash,
                    "RAI bootstrap rejected epoch close state with mismatched close hash"
                );
                self.close_state_pages.lock().unwrap().remove(&epoch);
                return false;
            }

            if close_state.cut_set(epoch).is_none()
                && close_state.install_cut(epoch, proof_cut).is_err()
            {
                return false;
            }

            if !close_state.cut_drained(epoch)
                && close_state
                    .record_cut_drain(
                        epoch,
                        close_entries.iter().map(|(slot, state)| (*slot, *state)),
                    )
                    .is_err()
            {
                return false;
            }

            let Ok(recorded_close_hash) = close_state.record_current_close_record_value(epoch)
            else {
                return false;
            };
            if recorded_close_hash != close_hash {
                return false;
            }
            let advanced = if terminal {
                close_state
                    .record_close_record_attempt_started(epoch, RAI_CLOSE_RECORD_RECOVERY_ATTEMPT);
                false
            } else {
                let _ = close_state.certify_close_record(epoch, &close_hash);
                let advanced = close_state.advance_epoch(epoch).is_ok();
                if advanced {
                    let _ = close_state.start_closing(epoch + 1);
                }
                advanced
            };
            (advanced, close_state.snapshot())
        };

        self.persistence.save_close_state(&snapshot);
        if advanced {
            let committee = self
                .committee_provider
                .snapshot_closed_epoch_committee(epoch);
            self.persistence.save_committee_snapshot(epoch, &committee);
            if let Some(weights) = self
                .committee_provider
                .closed_epoch_rep_weight_snapshot(epoch)
            {
                self.persistence.save_rep_weight_snapshot(epoch, &weights);
            }
            self.confirmed_slots.lock().unwrap().remove(&epoch);
            self.close_state_pages.lock().unwrap().remove(&epoch);
            self.close_state_requests.lock().unwrap().remove(&epoch);
            tracing::info!(epoch, %close_hash, terminal, "RAI bootstrap installed hash-chained epoch");
        }
        advanced
    }

    fn missing_committed_slots(
        &self,
        epoch: RaiEpoch,
        entries: &CloseRecordEntries,
    ) -> Result<Vec<RaiSlot>, RaiSlot> {
        let confirmed_slots = self.confirmed_slots.lock().unwrap();
        let epoch_slots = confirmed_slots.get(&epoch);
        let mut missing = Vec::new();
        for (slot, state) in entries {
            match state {
                RaiClosedSlotState::Finalized(expected) | RaiClosedSlotState::Carry(expected) => {
                    if let Some(local) = epoch_slots.and_then(|slots| slots.get(slot)) {
                        if local == expected {
                            continue;
                        }
                        return Err(*slot);
                    }

                    match self.block_requester.confirmed_block_hash(slot) {
                        Some(local) if local == *expected => {}
                        Some(_) => return Err(*slot),
                        None => missing.push(*slot),
                    }
                }
                RaiClosedSlotState::Released => {
                    if epoch_slots.and_then(|slots| slots.get(slot)).is_some()
                        || self.block_requester.confirmed_block_hash(slot).is_some()
                    {
                        return Err(*slot);
                    }
                }
            }
        }
        Ok(missing)
    }

    fn request_missing_confirmed_slots(&self, epoch: RaiEpoch, slots: &[RaiSlot]) {
        let source = self
            .close_state_sources
            .lock()
            .unwrap()
            .get(&epoch)
            .copied();
        self.block_requester.request_confirmed_slots(slots, source);
        tracing::debug!(
            epoch,
            missing_slots = slots.len(),
            "RAI bootstrap requested missing confirmed cut slots"
        );
    }
}

#[derive(Default)]
struct RaiEpochClosePageAccumulator {
    total_entries: Option<u32>,
    close_hash: Option<BlockHash>,
    entries: BTreeMap<u32, RaiEpochCloseEntry>,
    server_epoch: Option<RaiEpoch>,
}

impl RaiEpochClosePageAccumulator {
    fn insert_page(&mut self, page: RaiEpochClosePage) -> bool {
        if self
            .total_entries
            .is_some_and(|total| total != page.total_entries)
            || self
                .close_hash
                .is_some_and(|close_hash| close_hash != page.close_hash)
        {
            return false;
        }

        if !page
            .entries
            .windows(2)
            .all(|window| window[0].slot < window[1].slot)
        {
            return false;
        }
        if page.entries.is_empty() && page.start_index < page.total_entries {
            return false;
        }

        let end_index = page.start_index.saturating_add(page.entries.len() as u32);
        if end_index > page.total_entries {
            return false;
        }

        for entry in &page.entries {
            if self
                .entries
                .values()
                .any(|existing| existing.slot == entry.slot && *existing != *entry)
            {
                return false;
            }
        }

        self.total_entries = Some(page.total_entries);
        self.close_hash = Some(page.close_hash);
        self.server_epoch = Some(self.server_epoch.unwrap_or_default().max(page.server_epoch));

        for (offset, entry) in page.entries.into_iter().enumerate() {
            let index = page.start_index + offset as u32;
            self.entries.entry(index).or_insert(entry);
        }

        true
    }

    fn next_missing_start_index(&self) -> u32 {
        let Some(total_entries) = self.total_entries else {
            return 0;
        };

        for index in 0..total_entries {
            if !self.entries.contains_key(&index) {
                return index;
            }
        }
        total_entries
    }

    fn is_complete(&self) -> bool {
        self.total_entries
            .is_some_and(|total| self.entries.len() == total as usize)
    }

    fn complete_entries(&self) -> Option<(BlockHash, Vec<RaiEpochCloseEntry>, RaiEpoch)> {
        if !self.is_complete() {
            return None;
        }

        Some((
            self.close_hash?,
            self.entries.values().copied().collect::<Vec<_>>(),
            self.server_epoch?,
        ))
    }
}

fn close_entries_from_epoch_page_entries(
    entries: &[RaiEpochCloseEntry],
) -> Result<CloseRecordEntries, ()> {
    let mut close_entries = CloseRecordEntries::new();
    for entry in entries {
        if close_entries
            .insert(entry.slot, closed_slot_state_from_wire(entry.state))
            .is_some()
        {
            return Err(());
        }
    }
    Ok(close_entries)
}

fn closed_slot_state_from_wire(state: RaiEpochCloseEntryState) -> RaiClosedSlotState {
    match state {
        RaiEpochCloseEntryState::Finalized(block) => RaiClosedSlotState::Finalized(block),
        RaiEpochCloseEntryState::Carry(block) => RaiClosedSlotState::Carry(block),
        RaiEpochCloseEntryState::Released => RaiClosedSlotState::Released,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::RaiCloseStateSnapshot;
    use rsnano_types::{
        RaiEpochCloseAck, RaiEpochCloseEntry, RaiEpochCloseEntryState, RaiEpochClosePage,
        SavedBlock,
    };
    use std::sync::Mutex;

    #[test]
    fn does_not_reconstruct_close_record_from_local_blocks() {
        let block = SavedBlock::new_test_instance();
        let slot = RaiSlot::new(block.account(), block.height());
        let close_state = Arc::new(RwLock::new(RaiCloseState::new()));
        {
            let mut close_state = close_state.write().unwrap();
            close_state.start_closing(0).unwrap();
            close_state
                .install_cut(0, [slot].into_iter().collect())
                .unwrap();
        }
        let persistence = Arc::new(RecordingPersistence::default());
        let installer = RaiEpochBootstrap::new(
            close_state.clone(),
            Arc::new(EmptyCommitteeProvider),
            persistence.clone(),
            Arc::new(RecordingBlockRequester::default()),
        );

        installer.record_confirmed_blocks(&[(block.clone(), block.hash())]);

        let close_state = close_state.read().unwrap();
        assert_eq!(close_state.current_epoch(), 0);
        assert_eq!(close_state.epoch_phase(0), Some(RaiEpochPhase::Closing));
        assert_eq!(close_state.closed_slot_state(0, &slot), None);
        assert!(persistence.close_states.lock().unwrap().is_empty());
    }

    #[test]
    fn waits_for_all_cut_slots_before_installing_epoch() {
        let first = SavedBlock::new_test_instance_with_key(1);
        let second = SavedBlock::new_test_instance_with_key(2);
        let first_slot = RaiSlot::new(first.account(), first.height());
        let second_slot = RaiSlot::new(second.account(), second.height());
        let close_state = Arc::new(RwLock::new(RaiCloseState::new()));
        {
            let mut close_state = close_state.write().unwrap();
            close_state.start_closing(0).unwrap();
            close_state
                .install_cut(0, [first_slot, second_slot].into_iter().collect())
                .unwrap();
        }
        let installer = RaiEpochBootstrap::new(
            close_state.clone(),
            Arc::new(EmptyCommitteeProvider),
            Arc::new(RecordingPersistence::default()),
            Arc::new(RecordingBlockRequester::default()),
        );

        installer.record_confirmed_blocks(&[(first.clone(), first.hash())]);

        assert_eq!(close_state.read().unwrap().current_epoch(), 0);
    }

    #[test]
    fn does_not_request_blocks_from_an_uncertified_cut() {
        let first = SavedBlock::new_test_instance_with_key(1);
        let second = SavedBlock::new_test_instance_with_key(2);
        let first_slot = RaiSlot::new(first.account(), first.height());
        let second_slot = RaiSlot::new(second.account(), second.height());
        let close_state = Arc::new(RwLock::new(RaiCloseState::new()));
        {
            let mut close_state = close_state.write().unwrap();
            close_state.start_closing(0).unwrap();
            close_state
                .install_cut(0, [first_slot, second_slot].into_iter().collect())
                .unwrap();
        }
        let requester = Arc::new(RecordingBlockRequester::default());
        let installer = RaiEpochBootstrap::new(
            close_state,
            Arc::new(EmptyCommitteeProvider),
            Arc::new(RecordingPersistence::default()),
            requester.clone(),
        );

        installer.record_confirmed_blocks(&[(first.clone(), first.hash())]);

        assert!(requester.requested_slots().is_empty());
    }

    #[test]
    fn does_not_reconstruct_when_uncertified_cut_arrives() {
        let block = SavedBlock::new_test_instance();
        let slot = RaiSlot::new(block.account(), block.height());
        let close_state = Arc::new(RwLock::new(RaiCloseState::new()));
        let installer = RaiEpochBootstrap::new(
            close_state.clone(),
            Arc::new(EmptyCommitteeProvider),
            Arc::new(RecordingPersistence::default()),
            Arc::new(RecordingBlockRequester::default()),
        );

        installer.record_confirmed_blocks(&[(block.clone(), block.hash())]);
        assert_eq!(close_state.read().unwrap().current_epoch(), 0);

        {
            let mut close_state = close_state.write().unwrap();
            close_state.start_closing(0).unwrap();
            close_state
                .install_cut(0, [slot].into_iter().collect())
                .unwrap();
        }
        installer.try_install_available_epochs();

        let close_state = close_state.read().unwrap();
        assert_eq!(close_state.current_epoch(), 0);
    }

    #[test]
    fn reconstructs_epoch_from_bootstrapped_close_state_page() {
        let block = SavedBlock::new_test_instance();
        let slot = RaiSlot::new(block.account(), block.height());
        let close_state = Arc::new(RwLock::new(RaiCloseState::new()));
        close_state.write().unwrap().start_closing(0).unwrap();
        let requester = Arc::new(RecordingBlockRequester::with_confirmed([(
            slot,
            block.hash(),
        )]));
        let installer = RaiEpochBootstrap::new(
            close_state.clone(),
            Arc::new(EmptyCommitteeProvider),
            Arc::new(RecordingPersistence::default()),
            requester,
        );

        assert!(installer.process_epoch_close_ack(
            close_ack(
                0,
                vec![RaiEpochCloseEntry {
                    slot,
                    state: RaiEpochCloseEntryState::Finalized(block.hash()),
                }],
            ),
            ChannelId::from(77),
        ));

        let close_state = close_state.read().unwrap();
        assert_eq!(close_state.current_epoch(), 1);
        assert_eq!(
            close_state.closed_slot_state(0, &slot),
            Some(&RaiClosedSlotState::Finalized(block.hash()))
        );
    }

    #[test]
    fn requests_missing_finalized_blocks_before_installing_close_state_page() {
        let block = SavedBlock::new_test_instance();
        let slot = RaiSlot::new(block.account(), block.height());
        let close_state = Arc::new(RwLock::new(RaiCloseState::new()));
        close_state.write().unwrap().start_closing(0).unwrap();
        let requester = Arc::new(RecordingBlockRequester::default());
        let installer = RaiEpochBootstrap::new(
            close_state.clone(),
            Arc::new(EmptyCommitteeProvider),
            Arc::new(RecordingPersistence::default()),
            requester.clone(),
        );

        let source = ChannelId::from(77);
        assert!(installer.process_epoch_close_ack(
            close_ack(
                0,
                vec![RaiEpochCloseEntry {
                    slot,
                    state: RaiEpochCloseEntryState::Finalized(block.hash()),
                }],
            ),
            source,
        ));

        assert_eq!(close_state.read().unwrap().current_epoch(), 0);
        assert_eq!(requester.requested_slots(), vec![slot]);
        assert_eq!(requester.requested_sources(), vec![Some(source)]);
    }

    #[test]
    fn requests_first_close_state_page_for_closing_epoch() {
        let close_state = Arc::new(RwLock::new(RaiCloseState::new()));
        close_state.write().unwrap().start_closing(0).unwrap();
        let installer = RaiEpochBootstrap::new(
            close_state,
            Arc::new(EmptyCommitteeProvider),
            Arc::new(RecordingPersistence::default()),
            Arc::new(RecordingBlockRequester::default()),
        );

        let request = installer.next_close_state_request().unwrap();

        assert_eq!(request.epoch, 0);
        assert_eq!(request.start_index, 0);
    }

    #[test]
    fn terminal_epoch_waits_for_reissued_close_record_votes() {
        let block = SavedBlock::new_test_instance();
        let slot = RaiSlot::new(block.account(), block.height());
        let entry = RaiEpochCloseEntry {
            slot,
            state: RaiEpochCloseEntryState::Finalized(block.hash()),
        };
        let close_entries = close_entries_from_epoch_page_entries(&[entry]).unwrap();
        let close_hash = RaiCloseState::close_record_from_entries(
            0,
            BlockHash::ZERO,
            &Default::default(),
            &close_entries,
        )
        .hash();
        let close_state = Arc::new(RwLock::new(RaiCloseState::new()));
        close_state.write().unwrap().start_closing(0).unwrap();
        let installer = RaiEpochBootstrap::new(
            close_state.clone(),
            Arc::new(EmptyCommitteeProvider),
            Arc::new(RecordingPersistence::default()),
            Arc::new(RecordingBlockRequester::with_confirmed([(
                slot,
                block.hash(),
            )])),
        );
        let page = RaiEpochClosePage::new(0, 1, 0, close_hash, vec![entry]).with_server_epoch(2);

        assert!(installer.process_epoch_close_ack(RaiEpochCloseAck::new(page), ChannelId::from(1)));

        let state = close_state.read().unwrap();
        assert_eq!(state.current_epoch(), 0);
        assert!(state.has_close_record_value(0, &close_hash));
        assert!(state.close_record_attempt_started(0, RAI_CLOSE_RECORD_RECOVERY_ATTEMPT));
    }

    fn close_ack(epoch: RaiEpoch, entries: Vec<RaiEpochCloseEntry>) -> RaiEpochCloseAck {
        let close_entries =
            close_entries_from_epoch_page_entries(&entries).expect("test entries should be unique");
        let close_hash = RaiCloseState::close_record_from_entries(
            epoch,
            BlockHash::ZERO,
            &Default::default(),
            &close_entries,
        )
        .hash();
        RaiEpochCloseAck::new(
            RaiEpochClosePage::new(epoch, entries.len() as u32, 0, close_hash, entries)
                .with_server_epoch(3),
        )
    }

    #[derive(Default)]
    struct RecordingPersistence {
        close_states: Mutex<Vec<RaiCloseStateSnapshot>>,
    }

    impl RaiStatePersistence for RecordingPersistence {
        fn save_close_state(&self, snapshot: &RaiCloseStateSnapshot) {
            self.close_states.lock().unwrap().push(snapshot.clone());
        }
    }

    #[derive(Default)]
    struct RecordingBlockRequester {
        requested: Mutex<Vec<RaiSlot>>,
        sources: Mutex<Vec<Option<ChannelId>>>,
        confirmed: Mutex<BTreeMap<RaiSlot, BlockHash>>,
    }

    impl RecordingBlockRequester {
        fn with_confirmed(confirmed: impl IntoIterator<Item = (RaiSlot, BlockHash)>) -> Self {
            Self {
                requested: Mutex::new(Vec::new()),
                sources: Mutex::new(Vec::new()),
                confirmed: Mutex::new(confirmed.into_iter().collect()),
            }
        }

        fn requested_slots(&self) -> Vec<RaiSlot> {
            self.requested.lock().unwrap().clone()
        }

        fn requested_sources(&self) -> Vec<Option<ChannelId>> {
            self.sources.lock().unwrap().clone()
        }
    }

    impl RaiEpochBlockRequester for RecordingBlockRequester {
        fn request_confirmed_slots(&self, slots: &[RaiSlot], source: Option<ChannelId>) {
            self.requested.lock().unwrap().extend_from_slice(slots);
            self.sources.lock().unwrap().push(source);
        }

        fn confirmed_block_hash(&self, slot: &RaiSlot) -> Option<BlockHash> {
            self.confirmed.lock().unwrap().get(slot).copied()
        }
    }

    struct EmptyCommitteeProvider;

    impl RaiCommitteeProvider for EmptyCommitteeProvider {
        fn genesis_committee(&self) -> crate::consensus::RaiCommittee {
            crate::consensus::RaiCommittee::from_snapshot(crate::consensus::RaiCommitteeSnapshot {
                members: Vec::new(),
                thresholds: crate::consensus::RaiCommitteeThresholds::for_size(0),
            })
        }

        fn committee_for_closed_epoch(
            &self,
            _epoch: RaiEpoch,
        ) -> Option<crate::consensus::RaiCommittee> {
            Some(self.genesis_committee())
        }
    }
}
