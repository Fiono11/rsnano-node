pub(super) struct PeerScore {
    /// Number of requests to a peer that haven't been replied to yet
    pub running_queries: usize,
    pub priority: f32,
    channel_limit: usize,
    min_priority: f32,
    pub timeouts: usize,
    pub requests: usize,
    pub responses: usize,
    pub channel_full: usize,
    pub blocks_received: usize,
    pub out_of_date: usize,
}

impl PeerScore {
    pub fn new(channel_limit: usize) -> Self {
        Self {
            running_queries: 0,
            priority: 0.0,
            channel_limit,
            min_priority: (channel_limit as f32) * -1.0,
            timeouts: 0,
            requests: 0,
            responses: 0,
            channel_full: 0,
            blocks_received: 0,
            out_of_date: 0,
        }
    }

    pub fn request_sent(&mut self) {
        self.running_queries += 1;
        self.requests += 1;
    }

    pub fn got_response(&mut self) {
        self.remove_query();
        self.responses += 1;
    }

    pub fn remove_query(&mut self) {
        self.running_queries = self.running_queries.saturating_sub(1);
    }

    pub fn decay(&mut self) {
        self.priority *= 0.99;
    }

    pub fn priority_up(&mut self, value: f32) {
        self.priority = (self.priority + value).min(128.0);
    }

    pub fn priority_down(&mut self, value: f32) {
        self.priority = (self.priority - value).max(-128.0);
    }

    pub fn is_good(&self) -> bool {
        self.running_queries < self.channel_limit && self.priority >= self.min_priority
    }
}
