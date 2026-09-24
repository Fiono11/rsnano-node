use rsnano_types::{Amount, Blake2HashBuilder, BlockHash, ConsensusEpoch, PublicKey};

use super::{BlockIndex, BuildRules, BuildStateError, EpochLedger, SelectedReport, build_state};

/// RAI: one of the reports an epoch value selects, by its reporter and the
/// two roots the reporter signed. The value names the reports; the contents
/// behind the roots are reconstructed separately and checked against them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReportRef {
    pub reporter: PublicKey,
    /// r_i, the certified-state root
    pub certified: BlockHash,
    /// g_i, the residual-vote root
    pub residual: BlockHash,
}

/// RAI: the value an epoch election decides, `X = (h_p, Q_e, d_e)`. It names
/// a parent epoch value, the selected report roots, and only the hash of the
/// state derived from those reports: the state itself is never carried.
///
/// Every validator that accepts a proposal reconstructs the reports of `Q_e`,
/// derives `BuildState(S_{e-1}, Q_e)` and checks that its hash is `d_e`. Two
/// validators that accept the same value therefore hold the same state, and
/// the value stays small however large the state is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpochValue {
    pub epoch: ConsensusEpoch,
    /// The epoch-election slot this placement sits in. Election slots are
    /// separate from account slots, and the placement hash binds this one:
    /// the same reports and state proposed in two slots are two placements,
    /// so a vote in one slot cannot be replayed into the other.
    pub slot: u32,
    /// The epoch value this one descends from, zero for the first of an epoch
    pub parent: BlockHash,
    /// `Q_e`, in a canonical order so that two leaders naming the same
    /// reports name them the same way
    reports: Vec<ReportRef>,
    /// `d_e`
    pub state: BlockHash,
}

impl EpochValue {
    /// The value a proposal carries, as it came off the wire. The reports
    /// are checked for canonical order and distinct reporters when the value
    /// is validated, not here.
    pub fn from_parts(
        epoch: ConsensusEpoch,
        slot: u32,
        parent: BlockHash,
        reports: Vec<ReportRef>,
        state: BlockHash,
    ) -> Self {
        Self {
            epoch,
            slot,
            parent,
            reports,
            state,
        }
    }

    /// The reports an epoch value selects, in canonical order
    pub fn reports(&self) -> &[ReportRef] {
        &self.reports
    }

    /// RAI: `(Q_e, d_e)`, the payload of a placement. "A child of a
    /// non-genesis placement must copy its parent's (Q_e, d_e)": only a
    /// child of election genesis may introduce a selection of its own, so
    /// once a placement is joint-complete every later slot carries the same
    /// state, and the election decides which slot's placement it finalizes
    /// rather than which state.
    pub fn copies(&self, parent: &EpochValue) -> bool {
        self.reports == parent.reports && self.state == parent.state
    }

    /// A child of the notional genesis placement, which starts the election
    /// and is the only placement that may introduce a report selection
    pub fn extends_genesis(&self) -> bool {
        self.parent.is_zero()
    }

    /// The same payload in the next election slot: what a leader proposes
    /// when a joint-complete placement already exists
    pub fn extend(&self, slot: u32) -> Self {
        Self {
            epoch: self.epoch,
            slot,
            parent: self.hash(),
            reports: self.reports.clone(),
            state: self.state,
        }
    }

    /// Builds the value a leader proposes: the state the selected reports
    /// determine, and its hash. The caller has reconstructed every report it
    /// selects, which is what makes them usable.
    pub fn propose(
        epoch: ConsensusEpoch,
        slot: u32,
        parent: BlockHash,
        previous: &EpochLedger,
        selection: &[(ReportRef, SelectedReport)],
        index: &dyn BlockIndex,
        rules: BuildRules,
    ) -> Result<(Self, EpochLedger), BuildStateError> {
        let mut reports: Vec<ReportRef> = selection.iter().map(|(report, _)| *report).collect();
        reports.sort();
        let states: Vec<SelectedReport> = selection.iter().map(|(_, state)| *state).collect();
        let ledger = build_state(previous, &states, index, rules)?;
        let value = Self {
            epoch,
            slot,
            parent,
            reports,
            state: ledger.state_hash(),
        };
        Ok((value, ledger))
    }

    /// The hash a proposal binds and the votes name
    pub fn hash(&self) -> BlockHash {
        let mut builder = Blake2HashBuilder::new()
            .update(b"RAI epoch value")
            .update(self.epoch.as_u64().to_le_bytes())
            .update(self.slot.to_le_bytes())
            .update(self.parent.as_bytes());
        for report in &self.reports {
            builder = builder
                .update(report.reporter.as_bytes())
                .update(report.certified.as_bytes())
                .update(report.residual.as_bytes());
        }
        builder.update(self.state.as_bytes()).build()
    }

    /// RAI: what a validator checks before voting for a value. It selects the
    /// same reports, derives the state from them and requires the hash to be
    /// the one the value carries; a value whose reports it has not
    /// reconstructed is not one it can check, and it does not vote for it.
    ///
    /// The derived state is returned so that the caller keeps what it
    /// validated: a value that is later decided decides that state.
    pub fn validate(
        &self,
        previous: &EpochLedger,
        reconstructed: &dyn ReportSource,
        index: &dyn BlockIndex,
        quorum: Amount,
        rules: BuildRules,
    ) -> Result<EpochLedger, EpochValueError> {
        // Distinct reporters: a value that names one reporter twice would
        // count one report as several
        if self
            .reports
            .windows(2)
            .any(|pair| pair[0].reporter == pair[1].reporter)
        {
            return Err(EpochValueError::RepeatedReporter);
        }
        if self.reports.windows(2).any(|pair| pair[0] > pair[1]) {
            return Err(EpochValueError::NotCanonical);
        }
        let mut states = Vec::with_capacity(self.reports.len());
        let mut selected = Amount::ZERO;
        for report in &self.reports {
            let Some(state) = reconstructed.report(report) else {
                return Err(EpochValueError::NotReconstructed {
                    reporter: report.reporter,
                });
            };
            selected = selected
                .number()
                .checked_add(state.weight.number())
                .map(Amount::raw)
                .unwrap_or(Amount::MAX);
            states.push(state);
        }
        // q_report = N − f of the old committee, as weight
        if selected < quorum {
            return Err(EpochValueError::WrongSelectionSize {
                selected,
                required: quorum,
            });
        }
        let ledger =
            build_state(previous, &states, index, rules).map_err(EpochValueError::InvalidState)?;
        if ledger.state_hash() != self.state {
            return Err(EpochValueError::StateMismatch {
                derived: ledger.state_hash(),
                proposed: self.state,
            });
        }
        Ok(ledger)
    }
}

/// The reports a validator has reconstructed and checked against their signed
/// roots, which are the only ones it can derive a state from
pub trait ReportSource {
    fn report(&self, report: &ReportRef) -> Option<SelectedReport<'_>>;
}

/// Why a validator does not vote for an epoch value
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EpochValueError {
    InvalidState(BuildStateError),
    /// A proposal selects reports carrying at least N−f of the old
    /// committee's weight
    WrongSelectionSize {
        selected: Amount,
        required: Amount,
    },
    /// One reporter named twice would count as several reports
    RepeatedReporter,
    /// The reports are not in the canonical order, so two leaders naming the
    /// same reports could propose two different values
    NotCanonical,
    /// This validator has not reconstructed one of the reports, so it can not
    /// derive the state and has nothing to check the value against
    NotReconstructed {
        reporter: PublicKey,
    },
    /// The state the reports determine is not the one the value carries
    StateMismatch {
        derived: BlockHash,
        proposed: BlockHash,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::election::{
        AccountSlot, BlockPlacement, CertifiedBlock, CertifiedState, CertifiedStatus, ResidualKind,
        ResidualVotes,
    };
    use rsnano_types::Account;
    use std::collections::HashMap;

    /// Two validators holding the same reports derive the same state and the
    /// same value: the proposal carries the hash, not the state
    #[test]
    fn the_same_reports_give_the_same_value() {
        let world = World::new(3);
        let (value, ledger) = world.propose();
        assert_eq!(value.state, ledger.state_hash());
        assert_eq!(value.reports().len(), 3);

        let derived = value
            .validate(&EpochLedger::new(), &world, &world.index, QUORUM, rules())
            .expect("the reports determine this state");
        assert_eq!(derived.state_hash(), value.state);
        assert_eq!(derived.finalized_count(), ledger.finalized_count());
    }

    /// The reports are named in a canonical order, so that two leaders which
    /// selected the same reports propose the same value
    #[test]
    fn the_selection_is_canonically_ordered() {
        let world = World::new(3);
        let (value, _) = world.propose();
        let mut sorted = value.reports().to_vec();
        sorted.sort();
        assert_eq!(value.reports(), sorted.as_slice());

        let mut shuffled = value.clone();
        shuffled.reports.reverse();
        assert_eq!(
            shuffled.validate(&EpochLedger::new(), &world, &world.index, QUORUM, rules()),
            Err(EpochValueError::NotCanonical)
        );
        // And the hash follows the order, so a shuffled value is a different one
        assert_ne!(shuffled.hash(), value.hash());
    }

    /// A value whose state is not the one the reports determine is refused
    #[test]
    fn a_value_with_the_wrong_state_is_refused() {
        let world = World::new(3);
        let (mut value, _) = world.propose();
        let proposed = BlockHash::from(999);
        value.state = proposed;
        let error = value
            .validate(&EpochLedger::new(), &world, &world.index, QUORUM, rules())
            .unwrap_err();
        let EpochValueError::StateMismatch { derived, .. } = error else {
            panic!("expected a state mismatch, got {error:?}");
        };
        assert_ne!(derived, proposed);
    }

    /// A validator that has not reconstructed a selected report can not
    /// derive the state, so it does not vote for the value
    #[test]
    fn a_value_naming_an_unreconstructed_report_is_not_checkable() {
        let world = World::new(3);
        let (value, _) = world.propose();
        let missing = world.reporters[1];
        let partial = World {
            reports: world
                .reports
                .iter()
                .filter(|(reporter, _)| **reporter != missing)
                .map(|(reporter, held)| (*reporter, held.clone()))
                .collect(),
            ..World::new(3)
        };
        assert_eq!(
            value.validate(
                &EpochLedger::new(),
                &partial,
                &partial.index,
                QUORUM,
                rules()
            ),
            Err(EpochValueError::NotReconstructed { reporter: missing })
        );
    }

    /// A proposal selects exactly N-f reports, and one reporter named twice
    /// is not a second report
    #[test]
    fn the_selection_is_checked() {
        let world = World::new(3);
        let (value, _) = world.propose();
        assert_eq!(
            value.validate(&EpochLedger::new(), &world, &world.index, TOO_MUCH, rules()),
            Err(EpochValueError::WrongSelectionSize {
                selected: QUORUM,
                required: TOO_MUCH
            })
        );

        let mut repeated = value.clone();
        repeated.reports[1] = repeated.reports[0];
        assert_eq!(
            repeated.validate(&EpochLedger::new(), &world, &world.index, QUORUM, rules()),
            Err(EpochValueError::RepeatedReporter)
        );
    }

    /// Include_Q: a block one selected reporter supported is in the derived
    /// state, so a selection that leaves that reporter out gives another
    /// state and therefore another value
    #[test]
    fn leaving_a_reporter_out_changes_the_value() {
        let world = World::new(3);
        let (all, _) = world.propose();
        let fewer: Vec<(ReportRef, SelectedReport)> =
            world.selection().into_iter().take(2).collect();
        let (some, _) = EpochValue::propose(
            ConsensusEpoch::ZERO,
            0,
            BlockHash::ZERO,
            &EpochLedger::new(),
            &fewer,
            &world.index,
            rules(),
        )
        .unwrap();
        // Below-threshold residuals need not be retained, but Q is still bound.
        assert_eq!(all.state, some.state);
        assert_ne!(all.hash(), some.hash());
    }

    /// RAI: the placement hash binds its election slot, so the same reports
    /// and the same derived state proposed in two slots are two placements.
    /// Without this a vote cast in one slot would carry into the other.
    #[test]
    fn the_election_slot_is_bound_into_the_hash() {
        let world = World::new(3);
        let (first, _) = world.propose();
        let (second, _) = EpochValue::propose(
            ConsensusEpoch::ZERO,
            1,
            BlockHash::ZERO,
            &EpochLedger::new(),
            &world.selection(),
            &world.index,
            rules(),
        )
        .unwrap();

        assert_eq!(first.reports(), second.reports());
        assert_eq!(first.state, second.state);
        assert_ne!(first.slot, second.slot);
        assert_ne!(first.hash(), second.hash());
    }

    /*
     * Test helpers
     */

    /// f + p + 1, against a reporter weight that keeps a single report below
    /// the checkpoint recovery threshold
    const MANY: Amount = Amount::raw(25);
    const REPORTER_WEIGHT: Amount = Amount::raw(10);

    fn rules() -> BuildRules<'static> {
        BuildRules {
            many: MANY,
            backing: &(),
            finalization: super::super::CheckpointFinalization::CertificateOnly,
        }
    }
    /// q_report for three reporters of `REPORTER_WEIGHT` each
    const QUORUM: Amount = Amount::raw(30);
    /// More weight than three reporters carry
    const TOO_MUCH: Amount = Amount::raw(40);

    /// Three reporters: all of them certified the same block, and each one
    /// supported a block of its own that no certificate covers
    struct World {
        reporters: Vec<PublicKey>,
        reports: HashMap<PublicKey, HeldReport>,
        index: StubIndex,
    }

    #[derive(Clone)]
    struct HeldReport {
        certified: CertifiedState,
        residual: ResidualVotes,
        refs: ReportRef,
    }

    impl World {
        fn new(reporters: usize) -> Self {
            let mut index = StubIndex::default();
            let shared = index.add(1, 1, BlockHash::ZERO);
            let mut world = World {
                reporters: Vec::new(),
                reports: HashMap::new(),
                index: StubIndex::default(),
            };
            for i in 0..reporters {
                let reporter = PublicKey::from(i as u64 + 1);
                let own = index.add(2 + i as u64, 1, BlockHash::ZERO);
                let mut certified = CertifiedState::new();
                certified.certify(
                    CertifiedBlock::new(Account::from(1), 1, shared),
                    BlockHash::ZERO,
                    CertifiedStatus::Notarized,
                );
                let mut residual = ResidualVotes::new();
                residual.record(
                    CertifiedBlock::new(Account::from(2 + i as u64), 1, own),
                    BlockHash::ZERO,
                    ResidualKind::First,
                );
                let refs = ReportRef {
                    reporter,
                    certified: certified.root(),
                    residual: residual.root(),
                };
                world.reporters.push(reporter);
                world.reports.insert(
                    reporter,
                    HeldReport {
                        certified,
                        residual,
                        refs,
                    },
                );
            }
            world.index = index;
            world
        }

        fn selection(&self) -> Vec<(ReportRef, SelectedReport<'_>)> {
            let mut reporters = self.reporters.clone();
            reporters.sort();
            reporters
                .iter()
                .map(|reporter| {
                    let held = &self.reports[reporter];
                    (
                        held.refs,
                        SelectedReport {
                            reporter: *reporter,
                            weight: REPORTER_WEIGHT,
                            certified: &held.certified,
                            residual: &held.residual,
                        },
                    )
                })
                .collect()
        }

        fn propose(&self) -> (EpochValue, EpochLedger) {
            EpochValue::propose(
                ConsensusEpoch::ZERO,
                0,
                BlockHash::ZERO,
                &EpochLedger::new(),
                &self.selection(),
                &self.index,
                rules(),
            )
            .unwrap()
        }
    }

    impl ReportSource for World {
        fn report(&self, report: &ReportRef) -> Option<SelectedReport<'_>> {
            let held = self.reports.get(&report.reporter)?;
            (held.refs == *report).then_some(SelectedReport {
                reporter: report.reporter,
                weight: REPORTER_WEIGHT,
                certified: &held.certified,
                residual: &held.residual,
            })
        }
    }

    #[derive(Default)]
    struct StubIndex {
        blocks: HashMap<BlockHash, BlockPlacement>,
    }

    impl StubIndex {
        fn add(&mut self, account: u64, height: u64, previous: BlockHash) -> BlockHash {
            let hash = BlockHash::from(1000 + self.blocks.len() as u64);
            self.blocks.insert(
                hash,
                BlockPlacement {
                    slot: AccountSlot::new(Account::from(account), height),
                    previous,
                },
            );
            hash
        }
    }

    impl BlockIndex for StubIndex {
        fn placement(&self, hash: &BlockHash) -> Option<BlockPlacement> {
            self.blocks.get(hash).copied()
        }
    }
}
