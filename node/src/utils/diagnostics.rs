use std::{io::Write, time::Duration};

/// A diagnostic line for a benchmark run, written to stderr in one call.
/// nanospam collects the stderr of six nodes into one log, and a line
/// assembled from several writes comes out interleaved with the others.
pub(crate) fn emit_diagnostic(line: &str) {
    let mut out = Vec::with_capacity(line.len() + 32);
    let _ = write!(
        &mut out,
        "{line} t={} pid={}\n",
        unix_ms(),
        std::process::id()
    );
    let _ = std::io::stderr().write_all(&out);
}

/// Milliseconds since the epoch, for lining diagnostics up across nodes
pub(crate) fn unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default()
}

/// Writes one diagnostic line, formatted like `format!`
macro_rules! diagnostic {
    ($($arg:tt)*) => {
        $crate::utils::emit_diagnostic(&format!($($arg)*))
    };
}

pub(crate) use diagnostic;

/// Diagnostic: the CPU time used so far by the calling thread and by the
/// whole process. A thread busy for longer than its CPU time was waiting.
#[cfg(unix)]
pub(crate) fn cpu_times() -> Option<(Duration, Duration)> {
    let mut thread = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: both calls only write into the structs passed to them
    let thread_ok = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut thread) } == 0;
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let usage_ok = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } == 0;
    if !thread_ok || !usage_ok {
        return None;
    }
    let timeval = |t: libc::timeval| {
        Duration::from_secs(t.tv_sec as u64) + Duration::from_micros(t.tv_usec as u64)
    };
    Some((
        Duration::from_secs(thread.tv_sec as u64) + Duration::from_nanos(thread.tv_nsec as u64),
        timeval(usage.ru_utime) + timeval(usage.ru_stime),
    ))
}

#[cfg(not(unix))]
pub(crate) fn cpu_times() -> Option<(Duration, Duration)> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn the_process_has_used_at_least_the_cpu_time_of_this_thread() {
        let (thread, process) = cpu_times().unwrap();

        assert!(process >= thread);
    }
}
