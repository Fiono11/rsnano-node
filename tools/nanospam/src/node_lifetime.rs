use std::{
    path::Path,
    process::{Child, Command},
    sync::Mutex,
};

use crate::setup::write_node_pids;

/// The node processes of this run. They are killed on drop unless the run
/// leaves them alive (`--no-kill`); either way a node can be restarted.
#[derive(Default)]
pub(crate) struct NodeLifetime {
    node_handles: Mutex<Vec<Child>>,
    kill_on_drop: bool,
}

impl NodeLifetime {
    pub(crate) fn new(node_handles: Vec<Child>, kill_on_drop: bool) -> Self {
        Self {
            node_handles: Mutex::new(node_handles),
            kill_on_drop,
        }
    }

    /// Kills PR`pr` with SIGKILL, as a crash would: no shutdown path runs,
    /// so nothing is flushed or synced that the node had not already written
    pub(crate) fn kill(&self, pr: usize) -> std::io::Result<u32> {
        let mut children = self.node_handles.lock().unwrap();
        let child = &mut children[pr];
        let pid = child.id();
        child.kill()?;
        child.wait()?;
        Ok(pid)
    }

    /// Starts PR`pr` again on its data directory
    pub(crate) fn respawn(
        &self,
        pr: usize,
        command: &mut Command,
        data_dir: &Path,
    ) -> std::io::Result<u32> {
        let mut children = self.node_handles.lock().unwrap();
        children[pr] = command.spawn()?;
        write_node_pids(data_dir, &children)?;
        Ok(children[pr].id())
    }
}

impl Drop for NodeLifetime {
    fn drop(&mut self) {
        if self.kill_on_drop {
            for child in self.node_handles.get_mut().unwrap().iter_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}
