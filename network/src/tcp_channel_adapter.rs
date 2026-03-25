use std::{
    fmt::Display,
    sync::{Arc, Weak},
};
use tokio::select;

use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_nullable_tcp::TcpStream;

use crate::{
    Channel, ChannelDirection, ChannelId, TEST_ENDPOINT_1, TEST_ENDPOINT_2,
    bandwidth_limiter::BandwidthLimiter, channel_stats::ChannelStats,
};

/// Connects a Channel with a TcpStream
pub struct TcpChannelAdapter {
    pub channel: Arc<Channel>,
    stream: Weak<TcpStream>,
    clock: Arc<SteadyClock>,
}

impl TcpChannelAdapter {
    fn new(channel: Arc<Channel>, stream: Weak<TcpStream>, clock: Arc<SteadyClock>) -> Self {
        Self {
            channel,
            stream,
            clock,
        }
    }

    pub fn new_null() -> Self {
        Self::new_null_with_id(42)
    }

    pub fn new_null_with_id(id: impl Into<ChannelId>) -> Self {
        let channel_id = id.into();
        Self::new(
            Arc::new(Channel::new(
                channel_id,
                TEST_ENDPOINT_1,
                TEST_ENDPOINT_2,
                ChannelDirection::Outbound,
                u8::MAX,
                Timestamp::new_test_instance(),
                Arc::new(BandwidthLimiter::default()),
                Arc::new(ChannelStats::default()),
            )),
            Arc::downgrade(&Arc::new(TcpStream::new_null())),
            Arc::new(SteadyClock::new_null()),
        )
    }

    pub fn create(
        channel: Arc<Channel>,
        stream: TcpStream,
        clock: Arc<SteadyClock>,
        runtime: &tokio::runtime::Handle,
    ) -> Arc<Self> {
        let stream = Arc::new(stream);
        let channel_adapter = Self::new(channel.clone(), Arc::downgrade(&stream), clock.clone());

        // process write queue:
        runtime.spawn(async move {
            loop {
                let res = select! {
                    _ = channel.cancelled() =>{
                        return;
                    },
                  res = channel.pop() => res
                };

                if let Some(entry) = res {
                    let mut written = 0;
                    let buffer = &entry.buffer;
                    loop {
                        select! {
                            _ = channel.cancelled() =>{
                                return;
                            }
                            res = stream.writable() =>{
                            match res {
                            Ok(()) => match stream.try_write(&buffer[written..]) {
                                Ok(n) => {
                                    written += n;
                                    if written >= buffer.len() {
                                        channel.set_last_activity(clock.now());
                                        break;
                                    }
                                }
                                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                    continue;
                                }
                                Err(_) => {
                                    channel.write_error();
                                    channel.close();
                                    return;
                                }
                            },
                            Err(_) => {
                                channel.write_error();
                                channel.close();
                                return;
                            }
                        }
                            }
                        }
                    }
                } else {
                    break;
                }
            }
            channel.close();
        });

        Arc::new(channel_adapter)
    }

    pub async fn readable(&self) -> anyhow::Result<()> {
        if self.channel.is_closed() {
            return Err(anyhow!("Tried to read from a closed TcpStream"));
        }

        let Some(stream) = self.stream.upgrade() else {
            return Err(anyhow!("TCP stream dropped"));
        };

        let res = select! {
            _  = self.channel.cancelled() =>{
                return Err(anyhow!("cancelled"));
            },
            res = stream.readable() => res
        };

        match res {
            Ok(_) => Ok(()),
            Err(e) => {
                self.channel.read_failed();
                Err(e.into())
            }
        }
    }

    pub fn try_read(&self, buffer: &mut [u8]) -> anyhow::Result<usize> {
        let Some(stream) = self.stream.upgrade() else {
            return Err(anyhow!("TCP stream dropped"));
        };

        match stream.try_read(buffer) {
            Ok(0) => {
                self.channel.read_failed();
                Err(anyhow!("remote side closed the channel"))
            }
            Ok(n) => {
                self.channel.read_succeeded(n, self.clock.now());
                Ok(n)
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(0),
            Err(e) => {
                self.channel.read_failed();
                Err(e.into())
            }
        }
    }
}

impl Display for TcpChannelAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.channel.peer_addr().fmt(f)
    }
}

impl Drop for TcpChannelAdapter {
    fn drop(&mut self) {
        self.channel.close();
    }
}
