use rsnano_network::ChannelId;

pub(super) struct PeerScore {
    pub channel_id: ChannelId,
    /// Number of requests to a peer that haven't been replied to yet
    pub running_queries: usize,
    pub request_count_total: usize,
    pub response_count_total: usize,
}

impl PeerScore {
    pub fn new(channel_id: ChannelId) -> Self {
        Self {
            channel_id,
            running_queries: 0,
            request_count_total: 0,
            response_count_total: 0,
        }
    }

    pub fn add_query(&mut self) {
        self.running_queries += 1;
        self.request_count_total += 1;
    }

    pub fn got_response(&mut self) {
        if self.running_queries > 0 {
            self.running_queries -= 1;
            self.response_count_total += 1;
        }
    }

    pub fn decay(&mut self) {
        self.running_queries = self.running_queries.saturating_sub(1);
    }
}
