use rsnano_messages::Message;

#[cfg(feature = "rai_protocol")]
use rsnano_types::{
    RaiElectionId, RaiElectionValue, RaiPendingReport, RaiSlot, RaiVote, RaiVoteKind, RaiVoteScope,
};

pub(crate) fn log_received_message(message: &Message) {
    #[cfg(feature = "rai_protocol")]
    log_rai_message(message);

    #[cfg(not(feature = "rai_protocol"))]
    let _ = message;
}

pub(crate) fn closed_epoch(message: &Message) -> Option<u64> {
    #[cfg(feature = "rai_protocol")]
    {
        if let Message::RaiVote(vote) = message
            && vote.kind == RaiVoteKind::Final
            && let RaiElectionId::CloseRecord { epoch, .. } = vote.election_id
        {
            return Some(epoch);
        }

        // A close normally takes the fast path, whose certificate consists of
        // first votes and therefore has no standalone "close completed"
        // message. Observing any message in epoch e proves that e - 1 was
        // installed and closed locally.
        let observed_epoch = match message {
            Message::RaiVote(vote) => match &vote.election_id {
                RaiElectionId::Slot { epoch, .. }
                | RaiElectionId::CloseCut { epoch, .. }
                | RaiElectionId::CloseRecord { epoch, .. } => Some(*epoch),
            },
            Message::RaiPendingReport(report) => Some(report.epoch),
            _ => None,
        };
        observed_epoch.and_then(|epoch| epoch.checked_sub(1))
    }

    #[cfg(not(feature = "rai_protocol"))]
    {
        let _ = message;
        None
    }
}

#[cfg(feature = "rai_protocol")]
fn log_rai_message(message: &Message) {
    match message {
        Message::RaiVote(vote) => log_vote(vote),
        Message::RaiPendingReport(report) => log_pending_report(report),
        _ => {}
    }
}

#[cfg(feature = "rai_protocol")]
fn log_vote(vote: &RaiVote) {
    let election = describe_election(&vote.election_id);
    let certificate = describe_certificate(vote.kind, &vote.election_id);

    tracing::info!(
        target: "nanospam::rai",
        rai_event = "vote",
        phase = election.phase,
        election = election.name,
        certificate,
        vote_kind = vote.kind.as_str(),
        scope = %describe_scope(&vote.scope),
        epoch = election.epoch,
        attempt = %election.attempt,
        slot = %election.slot,
        value = %describe_value(&vote.value),
        voter = %vote.voter,
        vote_hash = %vote.hash(),
        "RAI certificate vote"
    );
}

#[cfg(feature = "rai_protocol")]
fn log_pending_report(report: &RaiPendingReport) {
    tracing::info!(
        target: "nanospam::rai",
        rai_event = "pending_report",
        phase = "closing",
        epoch = report.epoch,
        slots = report.slots.len(),
        slot_preview = %describe_slots(&report.slots),
        reporter = %report.reporter,
        report_hash = %report.hash(),
        "RAI pending report"
    );
}

#[cfg(feature = "rai_protocol")]
struct ElectionDescription {
    name: &'static str,
    phase: &'static str,
    epoch: u64,
    attempt: String,
    slot: String,
}

#[cfg(feature = "rai_protocol")]
fn describe_election(election_id: &RaiElectionId) -> ElectionDescription {
    match election_id {
        RaiElectionId::Slot { slot, epoch } => ElectionDescription {
            name: "slot",
            phase: "open",
            epoch: *epoch,
            attempt: "-".to_owned(),
            slot: describe_slot(slot),
        },
        RaiElectionId::CloseCut { epoch, attempt } => ElectionDescription {
            name: "close_cut",
            phase: "closing",
            epoch: *epoch,
            attempt: attempt.to_string(),
            slot: "-".to_owned(),
        },
        RaiElectionId::CloseRecord { epoch, attempt } => ElectionDescription {
            name: "close_record",
            phase: "close",
            epoch: *epoch,
            attempt: attempt.to_string(),
            slot: "-".to_owned(),
        },
    }
}

#[cfg(feature = "rai_protocol")]
fn describe_certificate(kind: RaiVoteKind, election_id: &RaiElectionId) -> &'static str {
    match election_id {
        RaiElectionId::Slot { .. } => match kind {
            RaiVoteKind::First => "slot_first",
            RaiVoteKind::Notarization => "slot_notarization",
            RaiVoteKind::Final => "slot_final",
        },
        RaiElectionId::CloseCut { .. } => match kind {
            RaiVoteKind::First => "closing_first",
            RaiVoteKind::Notarization => "closing_notarization",
            RaiVoteKind::Final => "closing_final",
        },
        RaiElectionId::CloseRecord { .. } => match kind {
            RaiVoteKind::First => "close_first",
            RaiVoteKind::Notarization => "close_notarization",
            RaiVoteKind::Final => "close_final",
        },
    }
}

#[cfg(feature = "rai_protocol")]
fn describe_scope(scope: &RaiVoteScope) -> String {
    match scope {
        RaiVoteScope::All => "all".to_owned(),
        RaiVoteScope::Committee(index) => format!("committee:{index}"),
    }
}

#[cfg(feature = "rai_protocol")]
fn describe_value(value: &RaiElectionValue) -> String {
    match value {
        RaiElectionValue::Block(hash) => format!("block:{hash}"),
        RaiElectionValue::CloseCutHash(hash) => format!("close_cut_hash:{hash}"),
        RaiElectionValue::CloseRecordHash(hash) => format!("close_record_hash:{hash}"),
        RaiElectionValue::Timeout => "timeout".to_owned(),
    }
}

#[cfg(feature = "rai_protocol")]
fn describe_slots(slots: &[RaiSlot]) -> String {
    const LIMIT: usize = 8;

    if slots.is_empty() {
        return "[]".to_owned();
    }

    let mut descriptions = slots
        .iter()
        .take(LIMIT)
        .map(describe_slot)
        .collect::<Vec<_>>();

    if slots.len() > LIMIT {
        descriptions.push(format!("+{} more", slots.len() - LIMIT));
    }

    format!("[{}]", descriptions.join(", "))
}

#[cfg(feature = "rai_protocol")]
fn describe_slot(slot: &RaiSlot) -> String {
    format!("{}:{}", slot.account.encode_account(), slot.account_height)
}

#[cfg(all(test, feature = "rai_protocol"))]
mod tests {
    use super::*;
    use rsnano_types::{Account, BlockHash};

    #[test]
    fn describes_close_cut_vote() {
        let election = RaiElectionId::CloseCut {
            epoch: 7,
            attempt: 2,
        };

        let description = describe_election(&election);

        assert_eq!(description.name, "close_cut");
        assert_eq!(description.phase, "closing");
        assert_eq!(description.epoch, 7);
        assert_eq!(description.attempt, "2");
        assert_eq!(
            describe_certificate(RaiVoteKind::Final, &election),
            "closing_final"
        );
        assert_eq!(
            describe_value(&RaiElectionValue::CloseCutHash(BlockHash::from(9))),
            "close_cut_hash:0000000000000000000000000000000000000000000000000000000000000009"
        );
    }

    #[test]
    fn describes_slot_preview_with_limit() {
        let slots = (0..10)
            .map(|height| RaiSlot::new(Account::from(height), height as u64))
            .collect::<Vec<_>>();

        let description = describe_slots(&slots);

        assert!(description.starts_with("[nano_"));
        assert!(description.ends_with("+2 more]"));
    }
}
