use std::process::Child;

#[derive(Default)]
pub(crate) struct NodeLifetime {
    node_handles: Vec<Child>,
}

impl NodeLifetime {
    pub(crate) fn push(&mut self, child: Child) {
        self.node_handles.push(child);
    }

    pub(crate) fn last_mut(&mut self) -> Option<&mut Child> {
        self.node_handles.last_mut()
    }

    pub(crate) fn release(mut self) -> Vec<Child> {
        std::mem::take(&mut self.node_handles)
    }
}

impl Drop for NodeLifetime {
    fn drop(&mut self) {
        for mut child in self.node_handles.drain(..) {
            // A node may already have exited after a startup/runtime error.
            // Killing and then waiting prevents both panics and port races with
            // the next nanospam invocation.
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
