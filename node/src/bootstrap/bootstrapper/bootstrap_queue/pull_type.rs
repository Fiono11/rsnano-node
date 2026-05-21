#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullType {
    /// Optimistic requests start from the (possibly unconfirmed) account frontier
    /// and are vulnerable to bootstrap poisoning.
    Optimistic,
    /// Safe requests start from the confirmed frontier and given enough time
    /// will eventually resolve forks
    Safe,
}

impl Default for PullType {
    fn default() -> Self {
        PullType::Optimistic
    }
}
