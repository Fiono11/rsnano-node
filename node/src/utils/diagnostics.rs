use std::{io::Write, sync::OnceLock};

/// Names the node in its diagnostics: nanospam sets it to `pr<i>`
const NODE_TAG_VAR: &str = "RSNANO_NODE_TAG";

/// A diagnostic line for a benchmark run, written to stderr in one call.
/// nanospam collects the stderr of six nodes into one log, and a line
/// assembled from several writes comes out interleaved with the others.
/// With `RSNANO_NODE_TAG` set, the line names its node before the time.
pub(crate) fn emit_diagnostic(line: &str) {
    let out = diagnostic_line(line, node_tag(), unix_ms());
    let _ = std::io::stderr().write_all(out.as_bytes());
}

fn node_tag() -> Option<&'static str> {
    static TAG: OnceLock<Option<String>> = OnceLock::new();
    TAG.get_or_init(|| std::env::var(NODE_TAG_VAR).ok().filter(|t| !t.is_empty()))
        .as_deref()
}

/// The time stays the last field: the log parsers match `... t=<ms>$`
fn diagnostic_line(line: &str, tag: Option<&str>, t: u128) -> String {
    match tag {
        Some(tag) => format!("{line} node={tag} t={t}\n"),
        None => format!("{line} t={t}\n"),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_node_tag_comes_before_the_time() {
        assert_eq!(
            diagnostic_line("EPOCH_CLOSED epoch=1", Some("pr2"), 5),
            "EPOCH_CLOSED epoch=1 node=pr2 t=5\n"
        );
        assert_eq!(
            diagnostic_line("EPOCH_CLOSED epoch=1", None, 5),
            "EPOCH_CLOSED epoch=1 t=5\n"
        );
    }
}
