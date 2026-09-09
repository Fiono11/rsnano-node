use rsnano_types::{BlockHash, QualifiedRoot};
use std::collections::HashSet;

/// Opt-in nanospam audit, bounded and disabled during ordinary node operation.
pub(crate) struct TerminationAudit {
    enabled: bool,
    overflow: bool,
    seen: HashSet<(u8, QualifiedRoot, BlockHash, u64)>,
    events: Vec<(u8, QualifiedRoot, BlockHash, u64, u64)>,
}
impl Default for TerminationAudit {
    fn default() -> Self {
        Self {
            enabled: std::env::var_os("NANOSPAM_TERMINATION_AUDIT").is_some(),
            overflow: false,
            seen: Default::default(),
            events: Vec::new(),
        }
    }
}
impl TerminationAudit {
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    pub fn record(&mut self, kind: u8, root: QualifiedRoot, hash: BlockHash, epoch: u64) {
        if !self.enabled {
            return;
        }
        if self.events.len() >= 1_000_000 {
            self.overflow = true;
            return;
        }
        if self.seen.insert((kind, root.clone(), hash, epoch)) {
            let time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64;
            self.events.push((kind, root, hash, epoch, time));
        }
    }
    pub fn page(&self, offset: usize) -> serde_json::Value {
        serde_json::json!({"enabled":self.enabled,"overflow":self.overflow,"total":self.events.len(),"events":self.events.iter().skip(offset).take(1000).collect::<Vec<_>>()})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn records_distinct_epoch_certificates_and_deduplicates_signer_arrivals() {
        let mut audit = TerminationAudit {
            enabled: true,
            ..Default::default()
        };
        let root = QualifiedRoot::new_test_instance();
        audit.record(1, root.clone(), BlockHash::from(1), 0);
        audit.record(1, root.clone(), BlockHash::from(1), 0);
        audit.record(1, root.clone(), BlockHash::from(2), 0);
        audit.record(1, root, BlockHash::from(1), 1);
        assert_eq!(audit.events.len(), 3);
        assert_eq!(audit.page(2)["events"].as_array().unwrap().len(), 1);
    }
}
