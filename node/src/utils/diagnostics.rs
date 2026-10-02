use std::io::Write;

/// A diagnostic line for a benchmark run, written to stderr in one call.
/// nanospam collects the stderr of six nodes into one log, and a line
/// assembled from several writes comes out interleaved with the others.
pub(crate) fn emit_diagnostic(line: &str) {
    let mut out = Vec::with_capacity(line.len() + 32);
    let _ = write!(&mut out, "{line} t={}\n", unix_ms());
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
