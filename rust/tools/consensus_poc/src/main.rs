use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use rand::{thread_rng, Rng};
use rand::seq::SliceRandom;
use crossbeam_channel::{bounded, Receiver, Sender};

// Define the number of nodes and faulty nodes (f)
const NUM_NODES: usize = 4; // n
const F: usize = 1; // f (Byzantine faults)

// Message types
#[derive(Debug, Clone)]
enum Message {
    Vote {
        from: usize,
        tx: char,
        round: usize,
    },
    Commit {
        from: usize,
        tx: char,
        round: usize,
        proof_round: usize,
    },
}

// Node state
struct Node {
    id: usize,
    tx_set: Vec<char>,
    current_tx: Option<char>,
    round: usize,
    proof_round: usize,
    votes: HashMap<usize, Message>,
    commits: HashMap<usize, Message>,
    channels: Vec<Sender<Message>>,
    receiver: Receiver<Message>,
    decided: bool,
    byzantine: bool,
}

impl Node {
    fn new(
        id: usize,
        channels: Vec<Sender<Message>>,
        receiver: Receiver<Message>,
    ) -> Self {
        let byzantine = thread_rng().gen_bool(0.25); // 25% chance of being byzantine
        Node {
            id,
            tx_set: vec!['A', 'B'],
            current_tx: None,
            round: 1,
            proof_round: 0,
            votes: HashMap::new(),
            commits: HashMap::new(),
            channels,
            receiver,
            decided: false,
            byzantine,
        }
    }

    fn broadcast(&self, msg: Message) {
        if self.byzantine {
            // Byzantine nodes may send random messages or not send at all
            let mut rng = thread_rng();
            if rng.gen_bool(0.5) { // 50% chance to not send any messages
                println!("Byzantine Node {} decided not to broadcast any messages", self.id);
                return;
            }
            // Randomly modify the message or send a different message
            let random_tx = *self.tx_set.choose(&mut rng).unwrap();
            let modified_msg = match msg {
                Message::Vote { from, .. } => Message::Vote {
                    from,
                    tx: random_tx,
                    round: self.round,
                },
                Message::Commit { from, .. } => Message::Commit {
                    from,
                    tx: random_tx,
                    round: self.round,
                    proof_round: self.round - 1,
                },
            };
            println!("Byzantine Node {} broadcasting modified message: {:?}", self.id, modified_msg);
            for ch in &self.channels {
                ch.send(modified_msg.clone()).expect("Failed to send message");
            }
        } else {
            println!("Node {} broadcasting message: {:?}", self.id, msg);
            for ch in &self.channels {
                ch.send(msg.clone()).expect("Failed to send message");
            }
        }
    }    

    fn run(&mut self) -> Option<char> {
        println!("Node {} starting run loop (Byzantine: {})", self.id, self.byzantine);
        while !self.decided {
            match self.round {
                1 => self.round_one(),
                _ => self.round_generic(),
            }
        }
        println!("Node {} has decided", self.id);
        self.current_tx
    }

    fn round_one(&mut self) {
        println!("Node {} entering round 1", self.id);
        // Proposer role: Node 0
        if self.id == 0 && !self.byzantine {
            let tx = self.tx_set[0]; // Proposer selects 'A'
            self.current_tx = Some(tx);
            let msg = Message::Vote {
                from: self.id,
                tx,
                round: self.round,
            };
            self.broadcast(msg);
        }

        // Start timer for round 1
        let start = Instant::now();
        let timeout = Duration::from_secs(5);

        while start.elapsed() < timeout {
            // Receive messages
            if let Ok(msg) = self.receiver.try_recv() {
                println!("Node {} received message in round 1: {:?}", self.id, msg);
                match msg {
                    Message::Vote { from, tx, round } if round == 1 => {
                        self.votes.insert(from, msg.clone());
                        if self.current_tx.is_none() {
                            self.current_tx = Some(tx);
                            // Broadcast vote
                            let vote_msg = Message::Vote {
                                from: self.id,
                                tx,
                                round: self.round,
                            };
                            self.broadcast(vote_msg);
                        }
                        // Check if received > f votes
                        if self.votes.len() > F {
                            println!("Node {} received more than F votes, proceeding to next round", self.id);
                            self.round += 1;
                            break;
                        }
                    }
                    _ => (),
                }
            }
        }

        // Proceed to next round if timeout
        if self.round == 1 {
            println!("Node {} timed out in round 1, proceeding to next round", self.id);
            self.round += 1;
        }
    }

    fn round_generic(&mut self) {
        println!("Node {} entering round {}", self.id, self.round);
        // Start timer for the current round
        let start = Instant::now();
        let timeout = Duration::from_secs(5);

        let mut tx_count: HashMap<char, usize> = HashMap::new();
        for vote in self.votes.values() {
            if let Message::Vote { tx, round, .. } = vote {
                if *round == self.round - 1 {
                    *tx_count.entry(*tx).or_insert(0) += 1;
                }
            }
        }

        // Send commit if received at least 2f + 1 votes on the same tx in the previous round
        if let Some((&tx, &count)) = tx_count.iter().max_by_key(|&(_, count)| count) {
            if count >= 2 * F + 1 {
                let commit_msg = Message::Commit {
                    from: self.id,
                    tx,
                    round: self.round,
                    proof_round: self.round - 1,
                };
                self.broadcast(commit_msg);
                self.current_tx = Some(tx);
                self.proof_round = self.round - 1;
            } else {
                // Otherwise, broadcast the same vote as the previous round
                if let Some(tx) = self.current_tx {
                    let vote_msg = Message::Vote {
                        from: self.id,
                        tx,
                        round: self.round,
                    };
                    self.broadcast(vote_msg);
                }
            }
        }

        while start.elapsed() < timeout {
            // Receive messages
            if let Ok(msg) = self.receiver.try_recv() {
                println!("Node {} received message in round {}: {:?}", self.id, self.round, msg);
                match msg {
                    Message::Vote { from, tx, round } if round == self.round => {
                        self.votes.insert(from, msg.clone());
                    }
                    Message::Commit {
                        from,
                        tx,
                        round,
                        proof_round,
                    } if round >= self.round => {
                        self.commits.insert(from, msg.clone());

                        // Check if received more than f commits with a more recent proof_round
                        let newer_proof_count = self
                            .commits
                            .values()
                            .filter(|m| match m {
                                Message::Commit { proof_round: pr, .. } => *pr > self.proof_round,
                                _ => false,
                            })
                            .count();

                        if newer_proof_count > F {
                            println!("Node {} updating commit due to receiving more than F newer proof commits", self.id);
                            self.current_tx = Some(tx);
                            self.proof_round = proof_round;
                            let commit_msg = Message::Commit {
                                from: self.id,
                                tx,
                                round: self.round,
                                proof_round: self.proof_round,
                            };
                            self.broadcast(commit_msg);
                        }

                        // Check for decision
                        let tx_commits = self
                            .commits
                            .values()
                            .filter(|m| match m {
                                Message::Commit { tx: t, .. } => *t == tx,
                                _ => false,
                            })
                            .count();

                        println!("Node {} has received {} commits for tx {} in round {}", self.id, tx_commits, tx, round);

                        if tx_commits >= 2 * F + 1 {
                            // Decide on tx
                            println!("Node {} DECIDES on tx {}", self.id, tx);
                            self.decided = true;
                            self.current_tx = Some(tx);
                            return;
                        }
                    }
                    _ => (),
                }
            }
        }

        // Timeout handling
        println!("Node {} timed out in round {}, broadcasting last own vote", self.id, self.round);
        if let Some(tx) = self.current_tx {
            let vote_msg = Message::Vote {
                from: self.id,
                tx,
                round: self.round,
            };
            self.broadcast(vote_msg);
        }
        self.round += 1;
    }
}

fn main() {
    // Create channels for each node
    let mut senders = Vec::new();
    let mut receivers = Vec::new();

    for _ in 0..NUM_NODES {
        let (s, r) = bounded::<Message>(100);
        senders.push(s);
        receivers.push(r);
    }

    // Shared senders for nodes
    let shared_senders = Arc::new(senders);

    let mut handles = Vec::new();

    for i in 0..NUM_NODES {
        let channels = shared_senders.clone();
        let receiver = receivers[i].clone();

        let mut node = Node::new(i, channels.to_vec(), receiver);

        let handle = thread::spawn(move || {
            node.run();
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_consensus_among_honest_nodes() {
        // Set up channels for nodes
        let mut senders = Vec::new();
        let mut receivers = Vec::new();

        for _ in 0..NUM_NODES {
            let (s, r) = bounded::<Message>(100);
            senders.push(s);
            receivers.push(r);
        }

        // Shared senders for nodes
        let shared_senders = Arc::new(senders);
        let mut handles = Vec::new();
        let mut results = Vec::new();

        // Create and run nodes
        for i in 0..NUM_NODES {
            let channels = shared_senders.clone();
            let receiver = receivers[i].clone();
            let mut node = Node::new(i, channels.to_vec(), receiver);

            let handle = thread::spawn(move || node.run());
            handles.push(handle);
        }

        // Collect decisions
        for handle in handles {
            if let Ok(result) = handle.join() {
                results.push(result);
            }
        }

        // Assert that all honest nodes agree on the decision
        let honest_results: Vec<_> = results.into_iter().filter_map(|d| d).collect();
        assert!(honest_results.windows(2).all(|w| w[0] == w[1]), "Honest nodes did not agree on the same value");
    }
}