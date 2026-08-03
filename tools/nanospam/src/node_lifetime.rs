use std::process::Child;

pub(crate) struct NodeLifetime {
    node_handles: Vec<Child>,
    kill_on_drop: bool,
}

impl NodeLifetime {
    pub(crate) fn new(kill_on_drop: bool) -> Self {
        Self {
            node_handles: Vec::new(),
            kill_on_drop,
        }
    }

    /// Takes ownership immediately after a successful spawn. This is
    /// deliberately separate from the completion of the whole startup loop:
    /// if a later node fails to start, earlier nodes must still be cleaned up.
    pub(crate) fn track(&mut self, child: Child) {
        self.node_handles.push(child);
    }

    pub(crate) fn child_status(
        &mut self,
        index: usize,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.node_handles[index].try_wait()
    }

    pub(crate) fn shutdown(&mut self) {
        for mut child in self.node_handles.drain(..) {
            if self.kill_on_drop {
                // Cleanup is best-effort and must never panic before the
                // remaining children have been handled.
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

impl Default for NodeLifetime {
    fn default() -> Self {
        Self::new(true)
    }
}

impl Drop for NodeLifetime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn shutdown_handles_already_exited_children_without_panicking() {
        let mut lifetime = NodeLifetime::new(true);
        lifetime.track(Command::new("/usr/bin/true").spawn().unwrap());
        lifetime.shutdown();
        assert!(lifetime.node_handles.is_empty());
    }

    #[test]
    fn no_kill_mode_releases_ownership_without_signalling() {
        let mut lifetime = NodeLifetime::new(false);
        lifetime.track(Command::new("/usr/bin/true").spawn().unwrap());
        lifetime.shutdown();
        assert!(lifetime.node_handles.is_empty());
    }
}
