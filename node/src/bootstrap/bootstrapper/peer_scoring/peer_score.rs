#[derive(Default)]
pub(super) struct PeerScore {
    /// Number of requests to a peer that haven't been replied to yet
    pub running_queries: usize,
}

impl PeerScore {
    pub fn request_sent(&mut self) {
        self.running_queries += 1;
    }

    pub fn got_response(&mut self) {
        self.running_queries = self.running_queries.saturating_sub(1);
    }

    pub fn decay(&mut self) {
        self.got_response();
    }
}
