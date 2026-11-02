use std::net::{IpAddr, Ipv6Addr, SocketAddr};

/// Where each running representative's node lives. Without `--hosts` every
/// node runs on this machine at `[::1]`; with it, line i names PR`i`'s
/// machine and nanospam reaches it at that address, starting the node over
/// ssh unless the line says `local`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NodeHosts {
    hosts: Vec<NodeHost>,
}

#[derive(Clone, Debug, PartialEq)]
struct NodeHost {
    address: IpAddr,
    remote: bool,
}

impl NodeHosts {
    /// Every node on this machine at loopback
    pub(crate) const fn local() -> Self {
        Self { hosts: Vec::new() }
    }

    /// One node per line: `ADDRESS` (started over ssh) or `ADDRESS local`
    /// (started on this machine, but reached by the others at ADDRESS).
    /// Blank lines and `#` comments are skipped.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let mut hosts = Vec::new();
        for (number, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap().trim();
            if line.is_empty() {
                continue;
            }
            let mut fields = line.split_whitespace();
            let address = fields.next().unwrap();
            let address: IpAddr = address
                .parse()
                .map_err(|_| format!("line {}: {address} is not an IP address", number + 1))?;
            let remote = match fields.next() {
                None => true,
                Some("local") => false,
                Some(other) => {
                    return Err(format!(
                        "line {}: expected `local` after the address, not {other}",
                        number + 1
                    ));
                }
            };
            if let Some(extra) = fields.next() {
                return Err(format!("line {}: unexpected {extra}", number + 1));
            }
            hosts.push(NodeHost { address, remote });
        }
        Ok(Self { hosts })
    }

    /// Nodes listed; none without `--hosts`
    pub(crate) fn len(&self) -> usize {
        self.hosts.len()
    }

    /// The address PR`i` is reached at, by nanospam and by the other nodes
    pub(crate) fn address(&self, i: usize) -> IpAddr {
        self.hosts
            .get(i)
            .map(|host| host.address)
            .unwrap_or(IpAddr::V6(Ipv6Addr::LOCALHOST))
    }

    pub(crate) fn socket(&self, i: usize, port: u16) -> SocketAddr {
        SocketAddr::new(self.address(i), port)
    }

    /// The ssh destination PR`i` is started on, None if it runs here
    pub(crate) fn ssh_target(&self, i: usize) -> Option<String> {
        self.hosts
            .get(i)
            .filter(|host| host.remote)
            .map(|host| host.address.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_list_every_node_is_local_at_loopback() {
        let hosts = NodeHosts::local();
        assert_eq!(hosts.len(), 0);
        assert_eq!(hosts.socket(3, 17105).to_string(), "[::1]:17105");
        assert_eq!(hosts.ssh_target(3), None);
    }

    #[test]
    fn a_line_per_node_with_comments_and_local_nodes() {
        let hosts = NodeHosts::parse(
            "# controller runs PR0\n10.128.0.2 local\n\n10.142.0.3  # us-east1\nfd00::7\n",
        )
        .unwrap();
        assert_eq!(hosts.len(), 3);
        assert_eq!(hosts.socket(0, 17075).to_string(), "10.128.0.2:17075");
        assert_eq!(hosts.ssh_target(0), None);
        assert_eq!(hosts.ssh_target(1), Some("10.142.0.3".to_owned()));
        assert_eq!(hosts.socket(2, 17095).to_string(), "[fd00::7]:17095");
        assert_eq!(hosts.ssh_target(2), Some("fd00::7".to_owned()));
    }

    #[test]
    fn rejects_what_is_not_an_address_or_local() {
        assert_eq!(
            NodeHosts::parse("node-1\n"),
            Err("line 1: node-1 is not an IP address".to_owned())
        );
        assert_eq!(
            NodeHosts::parse("10.0.0.1\n10.0.0.2 remote\n"),
            Err("line 2: expected `local` after the address, not remote".to_owned())
        );
        assert_eq!(
            NodeHosts::parse("10.0.0.1 local x\n"),
            Err("line 1: unexpected x".to_owned())
        );
    }
}
