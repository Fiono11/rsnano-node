use std::process::Child;

#[derive(Default)]
pub(crate) struct NodeLifetime {
    node_handles: Vec<Child>,
}

impl NodeLifetime {
    pub(crate) fn new(node_handles: Vec<Child>) -> Self {
        Self { node_handles }
    }

    pub(crate) fn stop(&mut self) {
        for mut child in self.node_handles.drain(..) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for NodeLifetime {
    fn drop(&mut self) {
        self.stop();
    }
}
