use std::{
    str::FromStr,
    time::{Duration, Instant},
};

use anyhow::anyhow;
use tokio::time::sleep;
use tracing::{info, warn};

use rsnano_rpc_client::NanoRpcClient;
use rsnano_types::{PrivateKey, RawKey};

use crate::weight_shift::move_weight;

/// RAI: committee rotations. At the start of an epoch each listed member
/// moves its whole balance to an account delegating to another
/// representative, so the committee derived from that epoch's state - the
/// one two epochs on counts in - has the other in its place. With standby
/// representatives (running, outside the committee) a rotation replaces
/// members; the Byzantine representatives keep their weight and stay in
/// every committee.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RotationSchedule(pub Vec<Rotation>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Rotation {
    pub epoch: u64,
    /// (from, to) representative indices
    pub moves: Vec<(usize, usize)>,
}

impl FromStr for RotationSchedule {
    type Err = anyhow::Error;

    /// `EPOCH:FROM>TO,FROM>TO;EPOCH:FROM>TO`, e.g. `1:3>5,4>6`
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut rotations = Vec::new();
        for part in value.split(';').filter(|part| !part.is_empty()) {
            let (epoch, moves) = part
                .split_once(':')
                .ok_or_else(|| anyhow!("a rotation is EPOCH:FROM>TO,..., not {part}"))?;
            let moves = moves
                .split(',')
                .map(|pair| {
                    let (from, to) = pair
                        .split_once('>')
                        .ok_or_else(|| anyhow!("a move is FROM>TO, not {pair}"))?;
                    Ok((from.parse()?, to.parse()?))
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            rotations.push(Rotation {
                epoch: epoch.parse()?,
                moves,
            });
        }
        rotations.sort_by_key(|rotation| rotation.epoch);
        Ok(Self(rotations))
    }
}

impl RotationSchedule {
    /// Every representative a rotation names
    pub(crate) fn representatives(&self) -> impl Iterator<Item = usize> + '_ {
        self.0
            .iter()
            .flat_map(|rotation| rotation.moves.iter().flat_map(|(from, to)| [*from, *to]))
    }
}

/// The account a move puts its weight in: one per move, held by no wallet
fn holder_key(index: u64) -> PrivateKey {
    RawKey::from(3_000_000_000 + index).into()
}

/// Into an epoch by this much, so a move falls inside it
const INTO_EPOCH: Duration = Duration::from_millis(500);

/// Applies the rotations, each at the start of its epoch counted from the
/// start of the epochs. They run past the end of the spam: the epochs go on.
pub(crate) async fn run_rotations(
    rpc_client: &NanoRpcClient,
    schedule: RotationSchedule,
    epochs_started: Instant,
    epoch_duration: Duration,
) {
    let mut index = 0;
    for rotation in schedule.0 {
        let at = epoch_duration * rotation.epoch as u32 + INTO_EPOCH;
        sleep(at.saturating_sub(epochs_started.elapsed())).await;
        for (from, to) in rotation.moves {
            let holder = holder_key(index);
            index += 1;
            match move_weight(rpc_client, from, to, &holder, |balance| balance).await {
                Ok(amount) => info!(
                    "ROTATION epoch={} from=PR{} to=PR{} amount=Ӿ{}",
                    rotation.epoch,
                    from,
                    to,
                    amount.format_balance(0)
                ),
                Err(error) => warn!(
                    "ROTATION_FAILED epoch={} from=PR{} to=PR{}: {error:?}",
                    rotation.epoch, from, to
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_a_schedule() {
        let schedule: RotationSchedule = "3:5>3;1:3>5,4>6".parse().unwrap();
        assert_eq!(
            schedule,
            RotationSchedule(vec![
                Rotation {
                    epoch: 1,
                    moves: vec![(3, 5), (4, 6)]
                },
                Rotation {
                    epoch: 3,
                    moves: vec![(5, 3)]
                },
            ])
        );
        assert_eq!(
            schedule.representatives().collect::<Vec<_>>(),
            vec![3, 5, 4, 6, 5, 3]
        );
        assert!("1".parse::<RotationSchedule>().is_err());
        assert!("1:3-5".parse::<RotationSchedule>().is_err());
    }

    #[test]
    fn every_move_has_its_own_holder() {
        assert_ne!(holder_key(0).account(), holder_key(1).account());
    }
}
