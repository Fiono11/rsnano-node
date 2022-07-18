mod blake2b;
mod buffer;
mod io_context;
mod json;
mod stream;
mod thread_pool;
mod toml;

pub use blake2b::*;
pub use buffer::*;
pub use io_context::*;
pub use json::*;
pub use stream::*;
pub use thread_pool::*;
pub use toml::*;

pub fn seconds_since_epoch() -> u64 {
    chrono::Utc::now().timestamp() as u64
}

pub fn get_cpu_count() -> usize {
    //todo: use std::thread::available_concurrency once it's in stable
    if let Ok(cpuinfo) = std::fs::read_to_string("/proc/cpuinfo") {
        cpuinfo.match_indices("processor").count()
    } else {
        1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ErrorCode {
    pub val: i32,
    pub category: u8,
}

pub mod error_category {
    pub const GENERIC: u8 = 0;
}

impl ErrorCode {
    pub fn is_ok(&self) -> bool {
        !self.is_err()
    }

    pub fn is_err(&self) -> bool {
        self.val != 0
    }

    pub fn not_supported() -> Self {
        ErrorCode {
            val: 95,
            category: error_category::GENERIC,
        }
    }

    pub fn no_buffer_space() -> Self {
        ErrorCode {
            val: 105,
            category: error_category::GENERIC,
        }
    }

    pub fn host_unreachable() -> Self {
        ErrorCode {
            val: 113,
            category: error_category::GENERIC,
        }
    }
}
