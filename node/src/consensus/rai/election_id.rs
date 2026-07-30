use blake2::{
    Blake2bVar,
    digest::{Update, VariableOutput},
};
use rsnano_types::{BlockHash, QualifiedRoot, Root};

use super::RaiEpoch;

const CLOSE_CUT_DOMAIN: &[u8] = b"RAI/CloseCutId";
const CLOSE_RECORD_DOMAIN: &[u8] = b"RAI/CloseRecordId";

pub fn rai_close_cut_root(epoch: RaiEpoch, round: u32) -> QualifiedRoot {
    synthetic_root(CLOSE_CUT_DOMAIN, epoch, round)
}

pub fn rai_close_record_root(epoch: RaiEpoch, round: u32) -> QualifiedRoot {
    synthetic_root(CLOSE_RECORD_DOMAIN, epoch, round)
}

fn synthetic_root(domain: &[u8], epoch: RaiEpoch, round: u32) -> QualifiedRoot {
    let mut hash = [0; 32];
    let mut hasher = Blake2bVar::new(hash.len()).expect("valid Blake2b output length");
    hasher.update(domain);
    hasher.update(&epoch.number().to_be_bytes());
    hasher.update(&round.to_be_bytes());
    hasher
        .finalize_variable(&mut hash)
        .expect("output buffer has the configured length");

    // This identity is only an active-election key. Its zero `previous` half
    // must never be used for block lookup or interpreted as a block root.
    QualifiedRoot::new(Root::from_bytes(hash), BlockHash::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_roots_are_deterministic_and_domain_separated() {
        let epoch = RaiEpoch::new(7);
        assert_eq!(rai_close_cut_root(epoch, 3), rai_close_cut_root(epoch, 3));
        assert_eq!(
            rai_close_record_root(epoch, 3),
            rai_close_record_root(epoch, 3)
        );
        assert_ne!(
            rai_close_cut_root(epoch, 3),
            rai_close_record_root(epoch, 3)
        );
    }

    #[test]
    fn epoch_and_round_are_part_of_the_identity() {
        assert_ne!(
            rai_close_cut_root(RaiEpoch::new(7), 3),
            rai_close_cut_root(RaiEpoch::new(8), 3)
        );
        assert_ne!(
            rai_close_cut_root(RaiEpoch::new(7), 3),
            rai_close_cut_root(RaiEpoch::new(7), 4)
        );
    }
}
