use std::process::Child;

#[derive(Default)]
pub(crate) struct NodeLifetime {
    node_handles: Vec<Child>,
}

impl NodeLifetime {
    pub(crate) fn new(node_handles: Vec<Child>) -> Self {
        Self { node_handles }
    }
}

impl Drop for NodeLifetime {
    fn drop(&mut self) {
        // Stop the whole cluster before waiting for individual processes. This
        // avoids leaving later nodes running while an earlier one shuts down.
        for child in &mut self.node_handles {
            let _ = child.kill();
        }

        // Reap every process before the next nanospam run can reuse its ports
        // or delete its data directory.
        for mut child in self.node_handles.drain(..) {
            let _ = child.wait();
        }
    }
}
