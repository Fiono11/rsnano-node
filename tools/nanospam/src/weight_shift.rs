use std::time::Duration;

use tokio::{select, time::interval};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use rsnano_rpc_client::NanoRpcClient;
use rsnano_types::{Amount, Block, BlockHash, JsonBlock, PrivateKey, RawKey, StateBlockArgs};

use crate::{setup::pr_key, wallets_factory::wait_until_confirmed};

/// RAI: a schedule that moves voting weight between the principal
/// representatives while a run goes on, so that the committees the epochs
/// derive differ. Shift k moves a share of PR(k mod n)'s balance to an account
/// delegating to PR(k+1 mod n): a bump of weight travels around the
/// representatives, and every member's weight moves over a run.
pub(crate) struct WeightShifts {
    prs: usize,
    percent: u32,
    next: u64,
}

/// One move of weight from one principal representative to another
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WeightShift {
    pub index: u64,
    pub from: usize,
    pub to: usize,
}

impl WeightShifts {
    pub(crate) fn new(prs: usize, percent: u32) -> Self {
        Self {
            prs,
            percent,
            next: 0,
        }
    }

    pub(crate) fn next_shift(&mut self) -> WeightShift {
        let index = self.next;
        self.next += 1;
        let from = (index % self.prs as u64) as usize;
        WeightShift {
            index,
            from,
            to: (from + 1) % self.prs,
        }
    }

    /// The part of a representative's balance one shift moves
    pub(crate) fn amount(&self, balance: Amount) -> Amount {
        // Quotient first: the representatives hold close to the whole supply
        Amount::raw(balance.number() / 100 * u128::from(self.percent))
    }
}

/// The account a shift moves weight into: one per shift, held by no wallet,
/// so no node receives into it on its own
pub(crate) fn holder_key(shift: &WeightShift) -> PrivateKey {
    RawKey::from(2_000_000_000 + shift.index).into()
}

/// Applies a shift every `period`, the first at once, until cancelled
pub(crate) async fn run_weight_shifts(
    rpc_client: &NanoRpcClient,
    mut shifts: WeightShifts,
    period: Duration,
    cancel: CancellationToken,
) {
    let mut ticks = interval(period);
    loop {
        select! {
            _ = cancel.cancelled() => return,
            _ = ticks.tick() => {}
        }
        let shift = shifts.next_shift();
        select! {
            _ = cancel.cancelled() => return,
            result = apply(rpc_client, &shifts, &shift) => {
                if let Err(error) = result {
                    warn!(?shift, "Weight shift failed: {error:?}");
                }
            }
        }
    }
}

/// The source representative sends the share to the shift's holder account,
/// which opens delegating to the target representative once the send is
/// confirmed
async fn apply(
    rpc_client: &NanoRpcClient,
    shifts: &WeightShifts,
    shift: &WeightShift,
) -> anyhow::Result<()> {
    let from = pr_key(shift.from);
    let to = pr_key(shift.to);
    let holder = holder_key(shift);
    let info = rpc_client.account_info(from.account()).await?;
    let amount = shifts.amount(info.balance);
    let send: Block = StateBlockArgs {
        key: &from,
        previous: info.frontier,
        // A representative votes with its own weight
        representative: from.public_key(),
        balance: info.balance - amount,
        link: holder.account().into(),
        work: 0.into(),
    }
    .into();
    let send_hash = send.hash();
    rpc_client.process(JsonBlock::from(send)).await?;
    wait_until_confirmed(rpc_client, send_hash).await;

    let open: Block = StateBlockArgs {
        key: &holder,
        previous: BlockHash::ZERO,
        representative: to.public_key(),
        balance: amount,
        link: send_hash.into(),
        work: 0.into(),
    }
    .into();
    rpc_client.process(JsonBlock::from(open)).await?;
    info!(
        "WEIGHT_SHIFT index={} from=PR{} to=PR{} amount=Ӿ{}",
        shift.index,
        shift.from,
        shift.to,
        amount.format_balance(0)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifts_travel_around_the_representatives() {
        let mut shifts = WeightShifts::new(3, 5);
        let pairs: Vec<_> = (0..4)
            .map(|_| {
                let shift = shifts.next_shift();
                (shift.index, shift.from, shift.to)
            })
            .collect();
        assert_eq!(pairs, vec![(0, 0, 1), (1, 1, 2), (2, 2, 0), (3, 0, 1)]);
    }

    #[test]
    fn a_shift_moves_its_percentage_without_overflow() {
        let shifts = WeightShifts::new(6, 5);
        assert_eq!(shifts.amount(Amount::raw(1000)), Amount::raw(50));
        let large = shifts.amount(Amount::MAX);
        assert!(large < Amount::MAX && large > Amount::ZERO);
    }

    #[test]
    fn every_shift_has_its_own_holder_outside_the_representatives() {
        let mut shifts = WeightShifts::new(6, 5);
        let a = holder_key(&shifts.next_shift());
        let b = holder_key(&shifts.next_shift());
        assert_ne!(a.account(), b.account());
        assert!((0..6).all(|i| pr_key(i).account() != a.account()));
    }
}
