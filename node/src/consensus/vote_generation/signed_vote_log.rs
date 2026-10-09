use std::{
    fmt::Write as _,
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
};

use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use rsnano_types::{Root, Vote, VoteKind};

use crate::utils::unix_ms;

/// RAI evaluation: one line per vote this node signs, appended before the
/// vote leaves the process. The file is the auditor's view of what each
/// representative signed across restarts: a vote that reached the network
/// was in the kernel's page cache first, so a killed process loses none.
/// It is independent of the signing records under test.
pub struct SignedVoteLog {
    file: Option<Mutex<File>>,
    listener: OutputListenerMt<String>,
}

impl SignedVoteLog {
    pub const FILE_NAME: &str = "signed-votes.log";

    /// Appends to the file of an earlier run of this node. An `OPENED` line
    /// starts each process lifetime, so the auditor can tell a conflict
    /// within one lifetime from one across a restart.
    pub fn new(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        file.write_all(format!("OPENED pid={} t={}\n", std::process::id(), unix_ms()).as_bytes())?;
        Ok(Self {
            file: Some(Mutex::new(file)),
            listener: OutputListenerMt::new(),
        })
    }

    /// Writes nothing; the lines are still emitted to trackers
    pub fn new_null() -> Self {
        Self {
            file: None,
            listener: OutputListenerMt::new(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.file.is_some() || self.listener.is_tracked()
    }

    /// `roots[i]` is the root `vote.hashes[i]` was voted at
    pub fn record(&self, vote: &Vote, roots: &[Root]) {
        if !self.is_enabled() {
            return;
        }
        let line = signed_vote_line(vote, roots);
        if let Some(file) = &self.file {
            // One write per line: the six nodes' logs are separate files, but
            // a line must never be split by a crash between two writes
            let _ = file.lock().unwrap().write_all(line.as_bytes());
        }
        self.listener.emit(line);
    }

    pub fn track(&self) -> Arc<OutputTrackerMt<String>> {
        self.listener.track()
    }
}

/// `SIGNED voter=<hex> kind=<kind> epoch=<raw u64> ts=<ms> [base=<hex>] <root>:<hash> ...`
/// The raw epoch keeps close rounds apart (top bit, round in the low bits);
/// a first vote names the checkpoint it was cast on
fn signed_vote_line(vote: &Vote, roots: &[Root]) -> String {
    let kind = match vote.kind() {
        VoteKind::First if vote.is_early() => "early",
        kind => kind.as_str(),
    };
    let mut line = String::with_capacity(96 + vote.hashes.len() * 130);
    let _ = write!(
        line,
        "SIGNED voter={} kind={} epoch={} ts={}",
        vote.voter,
        kind,
        vote.epoch.as_u64(),
        vote.timestamp().as_u64()
    );
    if vote.kind() == VoteKind::First {
        let _ = write!(line, " base={}", vote.base);
    }
    for (root, hash) in roots.iter().zip(&vote.hashes) {
        let _ = write!(line, " {root}:{hash}");
    }
    line.push('\n');
    line
}

#[cfg(test)]
mod tests {
    use rsnano_types::{BlockHash, ConsensusEpoch, PrivateKey};

    use super::*;

    #[test]
    fn records_one_line_per_signed_vote() {
        let log = SignedVoteLog::new_null();
        let tracker = log.track();
        let key = PrivateKey::from(1);
        let vote = Vote::new_in_epoch_as(
            &key,
            VoteKind::Final,
            false,
            ConsensusEpoch::new(3),
            BlockHash::ZERO,
            vec![BlockHash::from(7), BlockHash::from(8)],
        );

        log.record(&vote, &[Root::from(1), Root::from(2)]);

        let lines = tracker.output();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with(&format!(
            "SIGNED voter={} kind=final epoch=3 ts=",
            key.public_key()
        )));
        assert!(lines[0].ends_with(&format!(
            " {}:{} {}:{}\n",
            Root::from(1),
            BlockHash::from(7),
            Root::from(2),
            BlockHash::from(8)
        )));
    }

    /// The early bit is decoded under the RAI protocol only
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn early_first_votes_are_told_apart() {
        let log = SignedVoteLog::new_null();
        let tracker = log.track();
        let vote = Vote::new_in_epoch_as(
            &PrivateKey::from(1),
            VoteKind::First,
            true,
            ConsensusEpoch::new(1),
            BlockHash::from(5),
            vec![BlockHash::from(7)],
        );

        log.record(&vote, &[Root::from(1)]);

        assert!(tracker.output()[0].contains(" kind=early "));
        assert!(tracker.output()[0].contains(&format!(" base={} ", BlockHash::from(5))));
    }

    #[test]
    fn appends_to_the_file_across_reopens() {
        let dir =
            std::env::temp_dir().join(format!("rsnano-signed-vote-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SignedVoteLog::FILE_NAME);
        let _ = std::fs::remove_file(&path);
        let vote = Vote::new_in_epoch_as(
            &PrivateKey::from(1),
            VoteKind::First,
            false,
            ConsensusEpoch::new(0),
            BlockHash::ZERO,
            vec![BlockHash::from(7)],
        );

        SignedVoteLog::new(&path)
            .unwrap()
            .record(&vote, &[Root::from(1)]);
        SignedVoteLog::new(&path)
            .unwrap()
            .record(&vote, &[Root::from(1)]);

        let contents = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let kinds: Vec<&str> = contents
            .lines()
            .map(|line| line.split(' ').next().unwrap())
            .collect();
        assert_eq!(kinds, ["OPENED", "SIGNED", "OPENED", "SIGNED"]);
    }
}
