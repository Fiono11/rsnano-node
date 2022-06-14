use std::{
    ffi::c_void,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use crate::{
    logger_mt::Logger,
    seconds_since_epoch,
    stats::{DetailType, Direction, Stat, StatType},
    ThreadPool,
};

#[derive(Clone, Copy, Debug)]
pub struct ErrorCode {
    pub val: i32,
    pub category: u8,
}

impl ErrorCode {
    pub fn is_err(&self) -> bool {
        self.val != 0
    }

    pub fn not_supported() -> Self {
        ErrorCode {
            val: 95,     //not supported
            category: 0, // generic,
        }
    }

    pub fn no_buffer_space() -> Self {
        ErrorCode {
            val: 105,    // no buffer space
            category: 0, // generic
        }
    }
}

pub trait BufferWrapper {
    fn len(&self) -> usize;
    fn handle(&self) -> *mut c_void;
}

pub trait SharedConstBuffer {
    fn handle(&self) -> *mut c_void;
}

pub trait TcpSocketFacade {
    fn async_connect(&self, endpoint: SocketAddr, callback: Box<dyn Fn(ErrorCode)>);
    fn async_read(
        &self,
        buffer: &Arc<dyn BufferWrapper>,
        len: usize,
        callback: Box<dyn Fn(ErrorCode, usize)>,
    );
    fn async_write(&self, buffer: &dyn SharedConstBuffer, callback: Box<dyn Fn(ErrorCode, usize)>);
    fn remote_endpoint(&self) -> Result<SocketAddr, ErrorCode>;
    fn post(&self, f: Box<dyn FnOnce()>);
    fn dispatch(&self, f: Box<dyn FnOnce()>);
    fn close(&self) -> Result<(), ErrorCode>;
}

#[derive(PartialEq, Eq, Clone, Copy, FromPrimitive)]
pub enum EndpointType {
    Server,
    Client,
}

pub struct SocketImpl {
    /// The other end of the connection
    pub remote: Mutex<Option<SocketAddr>>,

    /// the timestamp (in seconds since epoch) of the last time there was successful activity on the socket
    /// activity is any successful connect, send or receive event
    pub last_completion_time_or_init: AtomicU64,

    /// the timestamp (in seconds since epoch) of the last time there was successful receive on the socket
    /// successful receive includes graceful closing of the socket by the peer (the read succeeds but returns 0 bytes)
    pub last_receive_time_or_init: AtomicU64,

    pub default_timeout: AtomicU64,

    /// Duration in seconds of inactivity that causes a socket timeout
    /// activity is any successful connect, send or receive event
    pub timeout_seconds: AtomicU64,

    tcp_socket: Arc<dyn TcpSocketFacade>,
    stats: Arc<Stat>,
    pub thread_pool: Arc<dyn ThreadPool>,
    endpoint_type: EndpointType,
    /// used in real time server sockets, number of seconds of no receive traffic that will cause the socket to timeout
    pub silent_connection_tolerance_time: AtomicU64,
    network_timeout_logging: bool,
    logger: Arc<dyn Logger>,

    /// Flag that is set when cleanup decides to close the socket due to timeout.
    /// NOTE: Currently used by bootstrap_server::timeout() but I suspect that this and bootstrap_server::timeout() are not needed.
    pub timed_out: AtomicBool,

    /// Set by close() - completion handlers must check this. This is more reliable than checking
    /// error codes as the OS may have already completed the async operation.
    closed: AtomicBool,

    /// Tracks number of blocks queued for delivery to the local socket send buffers.
    ///  Under normal circumstances, this should be zero.
    ///  Note that this is not the number of buffers queued to the peer, it is the number of buffers
    ///  queued up to enter the local TCP send buffer
    ///  socket buffer queue -> TCP send queue -> (network) -> TCP receive queue of peer
    pub queue_size: AtomicUsize,
}

impl SocketImpl {
    pub fn new(
        endpoint_type: EndpointType,
        tcp_socket: Arc<dyn TcpSocketFacade>,
        stats: Arc<Stat>,
        thread_pool: Arc<dyn ThreadPool>,
        default_timeout: Duration,
        silent_connection_tolerance_time: Duration,
        network_timeout_logging: bool,
        logger: Arc<dyn Logger>,
    ) -> Self {
        Self {
            remote: Mutex::new(None),
            last_completion_time_or_init: AtomicU64::new(seconds_since_epoch()),
            last_receive_time_or_init: AtomicU64::new(seconds_since_epoch()),
            tcp_socket,
            default_timeout: AtomicU64::new(default_timeout.as_secs()),
            timeout_seconds: AtomicU64::new(u64::MAX),
            stats,
            thread_pool,
            endpoint_type,
            silent_connection_tolerance_time: AtomicU64::new(
                silent_connection_tolerance_time.as_secs(),
            ),
            network_timeout_logging,
            logger,
            timed_out: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            queue_size: AtomicUsize::new(0),
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub fn set_last_completion(&self) {
        self.last_completion_time_or_init
            .store(seconds_since_epoch(), std::sync::atomic::Ordering::SeqCst);
    }

    pub fn set_last_receive_time(&self) {
        self.last_receive_time_or_init
            .store(seconds_since_epoch(), std::sync::atomic::Ordering::SeqCst);
    }

    /// Set the current timeout of the socket.
    ///  timeout occurs when the last socket completion is more than timeout seconds in the past
    ///  timeout always applies, the socket always has a timeout
    ///  to set infinite timeout, use Duration::MAX
    ///  the function checkup() checks for timeout on a regular interval
    pub fn set_timeout(&self, timeout: Duration) {
        self.timeout_seconds
            .store(timeout.as_secs(), Ordering::SeqCst);
    }

    pub fn set_default_timeout(&self) {
        self.timeout_seconds.store(
            self.default_timeout.load(Ordering::SeqCst),
            Ordering::SeqCst,
        );
    }

    pub fn close_internal(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.default_timeout.store(0, Ordering::SeqCst);

            if let Err(ec) = self.tcp_socket.close() {
                self.logger
                    .try_log(&format!("Failed to close socket gracefully: {:?}", ec));
                let _ = self.stats.inc(
                    StatType::Bootstrap,
                    DetailType::ErrorSocketClose,
                    Direction::In,
                );
            }
        }
    }
}

impl Drop for SocketImpl {
    fn drop(&mut self) {
        self.close_internal();
    }
}

pub trait Socket {
    fn async_connect(&self, endpoint: SocketAddr, callback: Box<dyn Fn(ErrorCode)>);
    fn async_read(
        &self,
        buffer: Arc<dyn BufferWrapper>,
        size: usize,
        callback: Box<dyn Fn(ErrorCode, usize)>,
    );
    fn async_write(
        &self,
        buffer: Arc<dyn SharedConstBuffer>,
        callback: Option<Box<dyn Fn(ErrorCode, usize)>>,
    );
    fn close(&self);
    fn checkup(&self);
}

impl Socket for Arc<SocketImpl> {
    fn async_connect(&self, endpoint: SocketAddr, callback: Box<dyn Fn(ErrorCode)>) {
        let self_clone = self.clone();
        debug_assert!(self.endpoint_type == EndpointType::Client);
        self.checkup();
        self.set_default_timeout();
        self.tcp_socket.async_connect(
            endpoint,
            Box::new(move |ec| {
                if !ec.is_err() {
                    self_clone.set_last_completion()
                }
                {
                    let mut lk = self_clone.remote.lock().unwrap();
                    *lk = Some(endpoint);
                }
                let stats = self_clone.stats.clone();

                if ec.is_err() {
                    let _ = stats.inc(StatType::Tcp, DetailType::TcpConnectError, Direction::In);
                }
                callback(ec);
            }),
        );
    }

    fn async_read(
        &self,
        buffer: Arc<dyn BufferWrapper>,
        size: usize,
        callback: Box<dyn Fn(ErrorCode, usize)>,
    ) {
        if size <= buffer.len() {
            if !self.is_closed() {
                self.set_default_timeout();
                let self_clone = self.clone();

                self.tcp_socket.async_read(
                    &buffer,
                    size,
                    Box::new(move |ec, len| {
                        if ec.is_err() {
                            let _ = self_clone.stats.inc(
                                StatType::Tcp,
                                DetailType::TcpReadError,
                                Direction::In,
                            );
                        } else {
                            let _ = self_clone.stats.add(
                                StatType::TrafficTcp,
                                DetailType::All,
                                Direction::In,
                                len as u64,
                                false,
                            );
                            self_clone.set_last_completion();
                            self_clone.set_last_receive_time();
                        }
                        callback(ec, len);
                    }),
                );
            }
        } else {
            debug_assert!(false); // async_read called with incorrect buffer size
            callback(ErrorCode::no_buffer_space(), 0);
        }
    }

    fn async_write(
        &self,
        buffer: Arc<dyn SharedConstBuffer>,
        callback: Option<Box<dyn Fn(ErrorCode, usize)>>,
    ) {
        if self.is_closed() {
            if let Some(cb) = callback {
                self.tcp_socket.post(Box::new(move || {
                    cb(ErrorCode::not_supported(), 0);
                }));
            }

            return;
        }

        self.queue_size.fetch_add(1, Ordering::SeqCst);

        let self_clone = self.clone();
        self.tcp_socket.post(Box::new(move || {
            if self_clone.is_closed() {
                if let Some(cb) = &callback {
                    cb(ErrorCode::not_supported(), 0);
                }

                return;
            }

            self_clone.set_default_timeout();
            let self_clone_2 = self_clone.clone();

            self_clone.tcp_socket.async_write(
                buffer.as_ref(),
                Box::new(move |ec, size| {
                    let _ = buffer;
                    self_clone_2.queue_size.fetch_sub(1, Ordering::SeqCst);

                    if ec.is_err() {
                        let _ = self_clone_2.stats.inc(
                            StatType::Tcp,
                            DetailType::TcpWriteError,
                            Direction::In,
                        );
                    } else {
                        let _ = self_clone_2.stats.add(
                            StatType::TrafficTcp,
                            DetailType::All,
                            Direction::Out,
                            size as u64,
                            false,
                        );
                        self_clone_2.set_last_completion();
                    }

                    if let Some(cbk) = &callback {
                        cbk(ec, size);
                    }
                }),
            );
        }));
    }

    fn close(&self) {
        let clone = self.clone();
        self.tcp_socket.dispatch(Box::new(move || {
            clone.close_internal();
        }));
    }

    fn checkup(&self) {
        let socket = Arc::downgrade(self);
        self.thread_pool.add_timed_task(
            Duration::from_secs(2),
            Box::new(move || {
                if let Some(socket) = socket.upgrade() {
                    let now = seconds_since_epoch();
                    let mut condition_to_disconnect = false;

                    // if this is a server socket, and no data is received for silent_connection_tolerance_time seconds then disconnect
                    if socket.endpoint_type == EndpointType::Server
                        && (now - socket.last_receive_time_or_init.load(Ordering::SeqCst))
                            > socket
                                .silent_connection_tolerance_time
                                .load(Ordering::SeqCst)
                    {
                        let _ = socket.stats.inc(
                            StatType::Tcp,
                            DetailType::TcpSilentConnectionDrop,
                            Direction::In,
                        );
                        condition_to_disconnect = true;
                    }

                    // if there is no activity for timeout seconds then disconnect
                    if (now - socket.last_completion_time_or_init.load(Ordering::SeqCst))
                        > socket.timeout_seconds.load(Ordering::SeqCst)
                    {
                        let _ = socket.stats.inc(
                            StatType::Tcp,
                            DetailType::TcpIoTimeoutDrop,
                            if socket.endpoint_type == EndpointType::Server {
                                Direction::In
                            } else {
                                Direction::Out
                            },
                        );
                        condition_to_disconnect = true;
                    }

                    if condition_to_disconnect {
                        if socket.network_timeout_logging {
                            // The remote end may have closed the connection before this side timing out, in which case the remote address is no longer available.
                            if let Ok(ep) = socket.tcp_socket.remote_endpoint() {
                                socket
                                    .logger
                                    .try_log(&format!("Disconnecting from {} due to timeout", ep));
                            }
                        }
                        socket.timed_out.store(true, Ordering::SeqCst);
                        socket.close();
                    } else if !socket.is_closed() {
                        socket.checkup();
                    }
                }
            }),
        );
    }
}
