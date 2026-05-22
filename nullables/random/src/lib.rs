use std::convert::Infallible;

use rand::{Rng, TryCryptoRng, TryRng, rng};

pub struct NullableRng {
    strategy: RngStrategy,
}

impl NullableRng {
    pub fn new_null() -> Self {
        Self::new_null_u64(42)
    }

    pub fn new_null_u64(val: u64) -> Self {
        Self {
            strategy: RngStrategy::Nulled(RngStub::new(val.to_be_bytes().to_vec())),
        }
    }

    pub fn new_null_bytes(bytes: &[u8]) -> Self {
        Self {
            strategy: RngStrategy::Nulled(RngStub::new(bytes.to_vec())),
        }
    }

    pub fn rng() -> Self {
        Self {
            strategy: RngStrategy::Thread,
        }
    }
}

impl TryRng for NullableRng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        Ok(match &mut self.strategy {
            RngStrategy::Thread => rng().next_u32(),
            RngStrategy::Nulled(i) => i.next_u32(),
        })
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        Ok(match &mut self.strategy {
            RngStrategy::Thread => rng().next_u64(),
            RngStrategy::Nulled(i) => i.next_u64(),
        })
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Self::Error> {
        match &mut self.strategy {
            RngStrategy::Thread => rng().fill_bytes(dest),
            RngStrategy::Nulled(i) => i.fill_bytes(dest),
        }
        Ok(())
    }
}

enum RngStrategy {
    Thread,
    Nulled(RngStub),
}

impl TryCryptoRng for NullableRng {}

struct RngStub {
    data: Vec<u8>,
    index: usize,
}

impl RngStub {
    pub fn new(data: Vec<u8>) -> Self {
        Self { data, index: 0 }
    }

    fn next_u32(&mut self) -> u32 {
        let mut buf = [0u8; 4];
        self.fill_bytes(&mut buf);
        u32::from_be_bytes(buf)
    }

    fn next_u64(&mut self) -> u64 {
        let mut buf = [0u8; 8];
        self.fill_bytes(&mut buf);
        u64::from_be_bytes(buf)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for i in dest {
            *i = self.data[self.index];
            self.index += 1;
            if self.index >= self.data.len() {
                self.index = 0;
            }
        }
    }
}

pub struct NullableRngFactory {
    strategy: RngFactoryStrategy,
}

impl NullableRngFactory {
    pub fn new_null() -> Self {
        Self {
            strategy: RngFactoryStrategy::Nulled(42),
        }
    }

    pub fn new_null_u64(val: u64) -> Self {
        Self {
            strategy: RngFactoryStrategy::Nulled(val),
        }
    }

    pub fn rng(&self) -> NullableRng {
        match &self.strategy {
            RngFactoryStrategy::Real => NullableRng::rng(),
            RngFactoryStrategy::Nulled(value) => NullableRng::new_null_u64(*value),
        }
    }
}

impl Default for NullableRngFactory {
    fn default() -> Self {
        Self {
            strategy: RngFactoryStrategy::Real,
        }
    }
}

enum RngFactoryStrategy {
    Real,
    Nulled(u64),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_rng() {
        let mut rng = NullableRng::rng();
        let a1 = rng.next_u64();
        let a2 = rng.next_u64();
        let a3 = rng.next_u64();
        assert!(a1 > 0 || a2 > 0 || a3 > 0);

        let b1 = rng.next_u32();
        let b2 = rng.next_u32();
        let b3 = rng.next_u32();
        assert!(b1 > 0 || b2 > 0 || b3 > 0);

        let mut buffer = [0; 32];

        rng.fill_bytes(&mut buffer);
        assert_eq!(buffer.iter().all(|&b| b == 0), false);

        buffer = [0; 32];
        rng.fill_bytes(&mut buffer);
        assert_eq!(buffer.iter().all(|&b| b == 0), false);
    }

    #[test]
    fn nullable_with_u64() {
        let mut rng = NullableRng::new_null();
        assert_eq!(rng.next_u64(), 42);

        assert_eq!(rng.next_u32(), 0);
        assert_eq!(rng.next_u32(), 42);

        let mut buffer = [0; 32];
        rng.fill_bytes(&mut buffer);
        assert_eq!(
            buffer,
            [
                0, 0, 0, 0, 0, 0, 0, 42, 0, 0, 0, 0, 0, 0, 0, 42, 0, 0, 0, 0, 0, 0, 0, 42, 0, 0, 0,
                0, 0, 0, 0, 42,
            ]
        );

        buffer = [0; 32];
        rng.fill_bytes(&mut buffer);
        assert_eq!(
            buffer,
            [
                0, 0, 0, 0, 0, 0, 0, 42, 0, 0, 0, 0, 0, 0, 0, 42, 0, 0, 0, 0, 0, 0, 0, 42, 0, 0, 0,
                0, 0, 0, 0, 42,
            ]
        );
    }

    #[test]
    fn nullable_with_byte_slice() {
        let mut rng = NullableRng::new_null_bytes(&[1, 2, 3, 4, 5, 6]);
        let mut buffer = [0; 10];
        rng.fill_bytes(&mut buffer);
        assert_eq!(buffer, [1, 2, 3, 4, 5, 6, 1, 2, 3, 4]);
    }

    #[test]
    fn factory_can_create_nulled_rng() {
        let rng_factory = NullableRngFactory::new_null();
        let mut rng = rng_factory.rng();
        assert_eq!(rng.next_u64(), 42);
        assert_eq!(rng.next_u64(), 42);
        assert_eq!(rng.next_u64(), 42);
    }

    #[test]
    fn nulled_factory_supports_configurable_responses() {
        let rng_factory = NullableRngFactory::new_null_u64(123);
        let mut rng = rng_factory.rng();
        assert_eq!(rng.next_u64(), 123);
        assert_eq!(rng.next_u64(), 123);
        assert_eq!(rng.next_u64(), 123);
    }

    #[test]
    fn factory_can_create_real_rng() {
        let rng_factory = NullableRngFactory::default();
        let mut rng = rng_factory.rng();
        let created = [rng.next_u64(), rng.next_u64(), rng.next_u64()];
        assert_ne!(created, [42, 42, 42]);
    }
}
