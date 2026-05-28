use std::thread::available_parallelism;

mod backpressure_handler;
mod cancellation_token;
pub mod container_info;
pub mod env;
mod event_handler;
mod event_processor;
pub mod fair_queue;
pub mod stats;
pub mod sync;
pub mod thread_factory;
pub mod thread_pool;
pub mod ticker;

pub use backpressure_handler::{
    BackpressureHandler, BackpressureHandlerMut, BackpressureHandlerRegistry,
};
pub use cancellation_token::CancellationToken;
pub use event_handler::{EventHandler, EventHandlerMut, EventHandlerRegistry};
pub use event_processor::{EventProcessor, EventSender};

pub fn get_cpu_count() -> usize {
    // Try to read overridden value from environment variable
    let value = std::env::var("NANO_HARDWARE_CONCURRENCY")
        .unwrap_or_else(|_| "0".into())
        .parse::<usize>()
        .unwrap_or_default();

    if value > 0 {
        return value;
    }

    available_parallelism().unwrap().get()
}
