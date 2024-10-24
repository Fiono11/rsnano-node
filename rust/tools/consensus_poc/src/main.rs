use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

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
}

impl Node {
    fn new(
        id: usize,
        channels: Vec<Sender<Message>>,
        receiver: Receiver<Message>,
    ) -> Self {
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
        }
    }

    fn broadcast(&self, msg: Message) {
        println!("Node {} broadcasting message: {:?}", self.id, msg);
        for ch in &self.channels {
            ch.send(msg.clone()).expect("Failed to send message");
        }
    }    

    fn run(&mut self) {
        println!("Node {} starting run loop", self.id);
        while !self.decided {
            match self.round {
                1 => self.round_one(),
                _ => self.round_generic(),
            }
        }
        println!("Node {} has decided", self.id);
    }

    fn round_one(&mut self) {
        println!("Node {} entering round 1", self.id);
        // Proposer role: Node 0
        if self.id == 0 {
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
                        if self.votes.len() > 2 * F + 1 {
                            println!("Node {} received more than 2F votes, proceeding to next round", self.id);
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
