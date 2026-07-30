/// Monotonically increasing RAI epoch number.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RaiEpoch(u64);

impl RaiEpoch {
    pub const ZERO: Self = Self(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn number(self) -> u64 {
        self.0
    }
}

impl From<u64> for RaiEpoch {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl From<RaiEpoch> for u64 {
    fn from(value: RaiEpoch) -> Self {
        value.0
    }
}
