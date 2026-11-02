use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
};

/// A node started over ssh: the local ssh client is only its stderr pipe,
/// the node itself is killed by the pid it wrote on its machine.
/// Its data directory has the same path there as here.
#[derive(Clone, Debug)]
pub(crate) struct RemoteNode {
    target: String,
    node_dir: PathBuf,
}

impl RemoteNode {
    pub(crate) fn new(target: String, node_dir: PathBuf) -> Self {
        Self { target, node_dir }
    }

    /// Writes the launch script into the local data directory and copies the
    /// directory to the node's machine, replacing what a previous run left
    pub(crate) fn push(&self, script: &str) -> io::Result<()> {
        std::fs::write(launcher_path(&self.node_dir), script)?;
        let parent = self.node_dir.parent().unwrap_or(Path::new("."));
        run(ssh(&self.target).arg(format!(
            "rm -rf {} && mkdir -p {}",
            shell_quote(&self.node_dir),
            shell_quote(parent)
        )))?;
        run(Command::new("scp")
            .args(["-rq", "-o", "BatchMode=yes"])
            .arg(&self.node_dir)
            .arg(format!("{}:{}/", scp_host(&self.target), parent.display())))
    }

    /// The command that starts (or restarts) the node: ssh runs the script
    /// pushed before and stays attached while the node runs
    pub(crate) fn start_command(&self) -> Command {
        let mut cmd = ssh(&self.target);
        cmd.arg(format!("sh {}", shell_quote(launcher_path(&self.node_dir))));
        cmd
    }

    /// SIGKILL, as a crash would; the pid is the node's on its machine
    pub(crate) fn kill(&self) -> io::Result<u32> {
        let pid_file = shell_quote(pid_path(&self.node_dir));
        let output = ssh(&self.target)
            .arg(format!(
                "p=$(cat {pid_file}) && kill -9 \"$p\" && echo \"$p\""
            ))
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "remote kill on {} failed: {}",
                self.target,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .map_err(io::Error::other)
    }
}

/// The script that starts the node on its machine. It records its pid
/// first, and `exec env` keeps that pid for the node: env names such as
/// nanospam's need no shell syntax that way.
pub(crate) fn launcher_script(
    program: &Path,
    env: &[(&str, String)],
    args: &[String],
    node_dir: &Path,
) -> String {
    let mut line = String::from("exec env");
    for (name, value) in env {
        line.push(' ');
        line.push_str(&shell_quote(format!("{name}={value}")));
    }
    line.push(' ');
    line.push_str(&shell_quote(program));
    for arg in args {
        line.push(' ');
        line.push_str(&shell_quote(arg));
    }
    format!(
        "#!/bin/sh\necho $$ > {}\n{line} >/dev/null\n",
        shell_quote(pid_path(node_dir))
    )
}

fn ssh(target: &str) -> Command {
    let mut cmd = Command::new("ssh");
    cmd.args([
        "-n",
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=accept-new",
        "-o",
        "ServerAliveInterval=15",
        target,
    ]);
    cmd
}

fn run(cmd: &mut Command) -> io::Result<()> {
    let output = cmd.output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{cmd:?} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn launcher_path(node_dir: &Path) -> PathBuf {
    node_dir.join("run-node.sh")
}

fn pid_path(node_dir: &Path) -> PathBuf {
    node_dir.join("node.pid")
}

/// scp needs brackets around an IPv6 address
fn scp_host(target: &str) -> String {
    if target.contains(':') {
        format!("[{target}]")
    } else {
        target.to_owned()
    }
}

fn shell_quote(value: impl AsRef<std::ffi::OsStr>) -> String {
    let value = value.as_ref().to_string_lossy();
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_single_quotes_for_the_shell() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn the_script_records_its_pid_then_becomes_the_node() {
        let script = launcher_script(
            Path::new("rai-bin/abc/rsnano"),
            &[
                ("RSNANO_NODE_TAG", "pr1".to_owned()),
                ("GENESIS", "{\"a\": 'b'}".to_owned()),
            ],
            &["--data-path".to_owned(), "/tmp/run/pr1".to_owned()],
            Path::new("/tmp/run/pr1"),
        );
        assert_eq!(
            script,
            "#!/bin/sh\necho $$ > '/tmp/run/pr1/node.pid'\n\
             exec env 'RSNANO_NODE_TAG=pr1' 'GENESIS={\"a\": '\\''b'\\''}' \
             'rai-bin/abc/rsnano' '--data-path' '/tmp/run/pr1' >/dev/null\n"
        );
    }

    #[test]
    fn brackets_ipv6_targets_for_scp() {
        assert_eq!(scp_host("10.0.0.1"), "10.0.0.1");
        assert_eq!(scp_host("fd00::7"), "[fd00::7]");
    }
}
