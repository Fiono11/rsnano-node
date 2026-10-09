use std::{
    path::Path,
    str::FromStr,
    time::{Duration, Instant},
};

use anyhow::anyhow;
use tokio::{io::WriteHalf, select, sync::mpsc, time::sleep};
use tokio_util::sync::CancellationToken;
use tracing::info;

use rsnano_nullable_tcp::{TcpStream, TcpStreamFactory};
use rsnano_rpc_client::NanoRpcClient;
use rsnano_rpc_messages::FinalStateResponse;

use crate::{
    app::{connect_node, drain_reader},
    cli_args::CliArgs,
    node_lifetime::NodeLifetime,
    setup::node_command,
};

/// A crash and restart of one node during the run: `--restart PR:WHEN`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RestartSpec {
    pub pr: usize,
    pub trigger: RestartTrigger,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RestartTrigger {
    /// This long after the spam started: in the middle of the elections
    At(Duration),
    /// While the node is in round `round` or a later one of the close
    /// election of `epoch`, before that close finalized a value
    Close { epoch: u64, round: u64 },
}

impl FromStr for RestartSpec {
    type Err = anyhow::Error;

    /// `PR:SECS`, `PR:at:SECS`, `PR:close:EPOCH` or `PR:close:EPOCH:ROUND`
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = value.split(':').collect();
        let pr = parts[0].parse()?;
        let trigger = match parts[1..] {
            [secs] | ["at", secs] => RestartTrigger::At(Duration::from_secs_f64(secs.parse()?)),
            ["close", epoch] => RestartTrigger::Close {
                epoch: epoch.parse()?,
                round: 0,
            },
            ["close", epoch, round] => RestartTrigger::Close {
                epoch: epoch.parse()?,
                round: round.parse()?,
            },
            _ => {
                return Err(anyhow!(
                    "restart must be PR:SECS, PR:at:SECS, PR:close:EPOCH or PR:close:EPOCH:ROUND"
                ));
            }
        };
        Ok(Self { pr, trigger })
    }
}

/// Where a node stands with respect to a close trigger
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClosePhase {
    Before,
    /// In the requested round or a later one, not closed yet
    Inside {
        round: u64,
    },
    /// The close finalized before the node was seen inside it
    Missed,
}

pub(crate) fn close_phase(state: &FinalStateResponse, epoch: u64, round: u64) -> ClosePhase {
    let Some(close) = state
        .epochs
        .iter()
        .find(|e| e.epoch.inner() == epoch)
        .and_then(|e| e.close.as_ref())
    else {
        return ClosePhase::Before;
    };
    if close.closed_value.is_some() {
        ClosePhase::Missed
    } else if close.started.inner() && close.round.inner() >= round {
        ClosePhase::Inside {
            round: close.round.inner(),
        }
    } else {
        ClosePhase::Before
    }
}

const CLOSE_POLL: Duration = Duration::from_millis(50);
const RPC_UP_TIMEOUT: Duration = Duration::from_secs(60);
const CLOSE_GRACE_AFTER_SPAM: Duration = Duration::from_secs(120);

pub(crate) struct Restarter<'a> {
    pub args: &'a CliArgs,
    pub data_dir: &'a Path,
    pub rpc_clients: &'a [NanoRpcClient],
    pub node_lifetime: &'a NodeLifetime,
    pub tcp_stream_factory: &'a TcpStreamFactory,
    /// New publishing connections for a restarted node
    pub reconnected: mpsc::UnboundedSender<(usize, Vec<WriteHalf<TcpStream>>)>,
    pub spam_started: Instant,
    pub down: Duration,
}

impl Restarter<'_> {
    /// Waits for the trigger, kills the node, keeps it down for `down` and
    /// starts it again on the same data directory. Once killed the node is
    /// always started again, even when the spam is over, so that the checks
    /// after the run see every node.
    pub(crate) async fn run(&self, spec: RestartSpec, cancel: CancellationToken) {
        let pr = spec.pr;
        // A close may come after the last block: a close trigger outlives
        // the spam by a grace period
        let grace = match spec.trigger {
            RestartTrigger::At(_) => Duration::ZERO,
            RestartTrigger::Close { .. } => CLOSE_GRACE_AFTER_SPAM,
        };
        let reached = select! {
            _ = async {
                cancel.cancelled().await;
                sleep(grace).await;
            } => None,
            reached = self.wait_for(spec) => Some(reached),
        };
        let Some(Ok(trigger_state)) = reached else {
            info!("RAI_RESTART_SKIPPED pr={pr} trigger={:?}", spec.trigger);
            return;
        };
        let killed = match self.node_lifetime.kill(pr) {
            Ok(pid) => pid,
            Err(e) => {
                info!("RAI_RESTART_FAILED pr={pr} kill: {e}");
                return;
            }
        };
        info!(
            "RAI_RESTART_KILLED pr={pr} pid={killed} trigger={:?} state={trigger_state} t={}",
            spec.trigger,
            unix_ms()
        );
        sleep(self.down).await;
        let mut command = node_command(self.args, self.data_dir, pr);
        let pid = match self.node_lifetime.respawn(pr, &mut command, self.data_dir) {
            Ok(pid) => pid,
            Err(e) => {
                info!("RAI_RESTART_FAILED pr={pr} respawn: {e}");
                return;
            }
        };
        let respawned = Instant::now();
        if tokio::time::timeout(RPC_UP_TIMEOUT, async {
            while self.rpc_clients[pr].version().await.is_err() {
                sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .is_err()
        {
            info!("RAI_RESTART_FAILED pr={pr} pid={pid}: no RPC after {RPC_UP_TIMEOUT:?}");
            return;
        }
        info!(
            "RAI_RESTART_UP pr={pr} pid={pid} after_ms={} t={}",
            respawned.elapsed().as_millis(),
            unix_ms()
        );
        if cancel.is_cancelled() {
            return;
        }
        match connect_node(self.tcp_stream_factory, pr).await {
            Ok((readers, writers)) => {
                for reader in readers {
                    tokio::spawn(drain_reader(reader));
                }
                let _ = self.reconnected.send((pr, writers));
                info!("RAI_RESTART_RECONNECTED pr={pr} t={}", unix_ms());
            }
            Err(e) => info!("RAI_RESTART_FAILED pr={pr} reconnect: {e}"),
        }
    }

    /// Resolves once the trigger is reached, with a description of the
    /// node's state at that moment
    async fn wait_for(&self, spec: RestartSpec) -> anyhow::Result<String> {
        match spec.trigger {
            RestartTrigger::At(after) => {
                sleep(after.saturating_sub(self.spam_started.elapsed())).await;
                Ok(format!("at_ms={}", self.spam_started.elapsed().as_millis()))
            }
            RestartTrigger::Close { epoch, round } => loop {
                // A node busy closing may answer slowly: a failed poll is
                // no reason to give up
                if let Ok(state) = self.rpc_clients[spec.pr].final_state().await {
                    match close_phase(&state, epoch, round) {
                        ClosePhase::Before => {}
                        ClosePhase::Inside { round } => {
                            return Ok(format!("close_epoch={epoch} close_round={round}"));
                        }
                        ClosePhase::Missed => {
                            info!("RAI_RESTART_MISSED pr={} close_epoch={epoch}", spec.pr);
                            return Err(anyhow!("the close of epoch {epoch} finalized first"));
                        }
                    }
                }
                sleep(CLOSE_POLL).await;
            },
        }
    }
}

/// Milliseconds since the epoch, to line nanospam's lines up with the nodes'
fn unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use rsnano_rpc_messages::{EpochCloseState, EpochFinalState};
    use rsnano_types::BlockHash;

    use super::*;

    #[test]
    fn parse_restart_specs() {
        assert_eq!(
            "1:18".parse::<RestartSpec>().unwrap(),
            RestartSpec {
                pr: 1,
                trigger: RestartTrigger::At(Duration::from_secs(18))
            }
        );
        assert_eq!(
            "2:at:1.5".parse::<RestartSpec>().unwrap().trigger,
            RestartTrigger::At(Duration::from_millis(1500))
        );
        assert_eq!(
            "3:close:1".parse::<RestartSpec>().unwrap().trigger,
            RestartTrigger::Close { epoch: 1, round: 0 }
        );
        assert_eq!(
            "3:close:2:1".parse::<RestartSpec>().unwrap().trigger,
            RestartTrigger::Close { epoch: 2, round: 1 }
        );
        assert!("1".parse::<RestartSpec>().is_err());
        assert!("1:close".parse::<RestartSpec>().is_err());
        assert!("x:5".parse::<RestartSpec>().is_err());
    }

    #[test]
    fn close_phase_follows_the_close_of_the_epoch() {
        assert_eq!(close_phase(&state(None), 1, 0), ClosePhase::Before);
        assert_eq!(
            close_phase(&state(Some(close(false, 0, false))), 1, 0),
            ClosePhase::Before
        );
        assert_eq!(
            close_phase(&state(Some(close(true, 0, false))), 1, 0),
            ClosePhase::Inside { round: 0 }
        );
        assert_eq!(
            close_phase(&state(Some(close(true, 0, false))), 1, 1),
            ClosePhase::Before
        );
        assert_eq!(
            close_phase(&state(Some(close(true, 2, false))), 1, 1),
            ClosePhase::Inside { round: 2 }
        );
        assert_eq!(
            close_phase(&state(Some(close(true, 0, true))), 1, 0),
            ClosePhase::Missed
        );
        assert_eq!(
            close_phase(&state(Some(close(true, 0, false))), 2, 0),
            ClosePhase::Before
        );
    }

    /*
     * Test helpers
     */

    fn close(started: bool, round: u64, closed: bool) -> EpochCloseState {
        EpochCloseState {
            ready: true.into(),
            value: None,
            started: started.into(),
            round: round.into(),
            closed_value: closed.then_some(BlockHash::from(1)),
            closed_round: closed.then_some(round.into()),
        }
    }

    fn state(close: Option<EpochCloseState>) -> FinalStateResponse {
        FinalStateResponse {
            hash: BlockHash::ZERO,
            all_terminated: false.into(),
            all_settled: false.into(),
            accounts: 0.into(),
            single_notarized: 0.into(),
            pending: 0.into(),
            empty: 0.into(),
            conflicting: Vec::new(),
            current_epoch: 1.into(),
            epochs: vec![EpochFinalState {
                epoch: 1.into(),
                hash: BlockHash::ZERO,
                finalized: 0.into(),
                single_notarized: 0.into(),
                pending: 0.into(),
                cemented_undecided: 0.into(),
                empty: 0.into(),
                conflicting: 0.into(),
                close,
            }],
            committees: Vec::new(),
            entries: None,
        }
    }
}
