use std::{sync::Arc, time::Duration};

use rand::RngCore;

use rsnano_messages::{AscPullReq, Message};
use rsnano_network::TrafficType;
use rsnano_nullable_clock::SteadyClock;
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use rsnano_utils::stats::{DetailType, StatType, Stats};

use crate::{
    bootstrap::bootstrapper::{
        AscPullQuerySpec,
        state::{BootstrapLogic, RunningQuery},
    },
    transport::MessageSender,
};
use tracing::trace;

/// Sends an AscPullReq message
pub(crate) struct QuerySender {
    message_sender: MessageSender,
    clock: SteadyClock,
    request_timeout: Duration,
    stats: Arc<Stats>,
    send_listener: OutputListenerMt<AscPullQuerySpec>,
}

impl QuerySender {
    pub(crate) fn new(message_sender: MessageSender, stats: Arc<Stats>) -> Self {
        Self::new_impl(message_sender, SteadyClock::default(), stats)
    }
    fn new_impl(message_sender: MessageSender, clock: SteadyClock, stats: Arc<Stats>) -> Self {
        Self {
            message_sender,
            clock,
            stats,
            request_timeout: Duration::from_secs(15),
            send_listener: OutputListenerMt::new(),
        }
    }

    pub fn set_request_timeout(&mut self, timeout: Duration) {
        self.request_timeout = timeout;
    }

    pub fn send(&mut self, spec: AscPullQuerySpec, state: &mut BootstrapLogic) -> Option<u64> {
        if self.send_listener.is_tracked() {
            self.send_listener.emit(spec.clone());
        }

        let id = rand::rng().next_u64();
        let now = self.clock.now();
        let query_type = spec.query_type();
        let channel_id = spec.channel.channel_id();
        let mut query = RunningQuery::from_spec(id, &spec, now, self.request_timeout);

        let message = Message::AscPullReq(AscPullReq {
            id,
            req_type: spec.req_type,
        });

        let sent =
            self.message_sender
                .try_send(&spec.channel, &message, TrafficType::BootstrapRequests);

        if sent {
            trace!(query_id = id, ?query_type, ?channel_id, "Pull request sent");
            self.stats.inc(StatType::Bootstrap, DetailType::Request);
            self.stats
                .inc(StatType::BootstrapRequest, query_type.into());

            // After the request has been sent, the peer has a limited time to respond
            query.response_cutoff = now + self.request_timeout;
            state.running_queries.insert(query);

            if spec.cooldown_account {
                state.bootstrap_queue.set_last_request(&spec.account, now);
            }

            Some(id)
        } else {
            trace!(
                query_id = id,
                ?query_type,
                ?channel_id,
                "Pull request failed"
            );

            self.stats
                .inc(StatType::Bootstrap, DetailType::RequestFailed);
            None
        }
    }

    #[allow(dead_code)]
    pub fn track(&self) -> Arc<OutputTrackerMt<AscPullQuerySpec>> {
        self.send_listener.track()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::SendEvent;
    use rsnano_nullable_clock::Timestamp;
    use rsnano_output_tracker::OutputTrackerMt;

    #[test]
    fn send_message() {
        let mut fixture = create_fixture();

        let spec = AscPullQuerySpec::new_test_instance();
        let channel_id = spec.channel.channel_id();
        let mut state = BootstrapLogic::default();

        let id = fixture.query_sender.send(spec, &mut state);
        assert!(id.is_some());

        let output = fixture.send_tracker.output();
        assert_eq!(output.len(), 1, "no message sent!");
        assert_eq!(output[0].channel_id, channel_id);
        assert_eq!(output[0].traffic_type, TrafficType::BootstrapRequests);
        let Message::AscPullReq(_) = &output[0].message else {
            panic!("no asc pull req!")
        };
    }

    #[test]
    fn insert_into_running_queries() {
        let mut fixture = create_fixture();

        let spec = AscPullQuerySpec::new_test_instance();
        let mut state = BootstrapLogic::default();
        state.bootstrap_queue.priority_up(&spec.account);

        let id = fixture.query_sender.send(spec.clone(), &mut state).unwrap();

        assert_eq!(state.running_queries.len(), 1);
        assert!(state.running_queries.contains(id));
        assert_eq!(
            state.running_queries.get(id).unwrap().response_cutoff,
            fixture.now + fixture.query_sender.request_timeout
        );
        assert_eq!(state.bootstrap_queue.last_request(&spec.account), None);
    }

    #[test]
    fn cool_down_account() {
        let mut fixture = create_fixture();
        let mut spec = AscPullQuerySpec::new_test_instance();
        spec.cooldown_account = true;

        let mut state = BootstrapLogic::default();
        state.bootstrap_queue.priority_up(&spec.account);

        fixture.query_sender.send(spec.clone(), &mut state).unwrap();

        assert_eq!(
            state.bootstrap_queue.last_request(&spec.account),
            Some(fixture.now)
        );
    }

    #[test]
    fn when_channel_unavailable_should_not_send() {
        let mut fixture = create_fixture();
        let spec = AscPullQuerySpec::new_test_instance();
        let mut state = BootstrapLogic::default();

        spec.channel.close();
        let id = fixture.query_sender.send(spec.clone(), &mut state);

        assert_eq!(id, None);
        assert_eq!(state.running_queries.len(), 0);
        assert_eq!(state.bootstrap_queue.info().download_queue, 0);
        assert_eq!(state.bootstrap_queue.blocked_count(), 0);
    }

    #[test]
    fn can_track_sends() {
        let mut fixture = create_fixture();
        let spec = AscPullQuerySpec::new_test_instance();
        let mut state = BootstrapLogic::default();

        let tracker = fixture.query_sender.track();
        fixture.query_sender.send(spec.clone(), &mut state);

        assert_eq!(tracker.output(), [spec]);
    }

    fn create_fixture() -> Fixture {
        let message_sender = MessageSender::new_null();
        let send_tracker = message_sender.track();

        let clock = SteadyClock::new_null();
        let now = clock.now();
        let query_sender = QuerySender::new_impl(message_sender, clock, Arc::new(Stats::default()));

        Fixture {
            query_sender,
            send_tracker,
            now,
        }
    }

    struct Fixture {
        query_sender: QuerySender,
        send_tracker: Arc<OutputTrackerMt<SendEvent>>,
        now: Timestamp,
    }
}
