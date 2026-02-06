//! A simple adaptor to convert between `rand_core` V0.6 and `rand_core` V0.9
//!
use crate::matter::crypto::{CryptoRng, RngCore};

/// A simple adaptor to convert between `rand_core` V0.6 and `rand_core` V0.9
pub struct RandAdaptor<T>(T);

impl<T> RandAdaptor<T> {
    /// Create a new `RandAdaptor` instance wrapping the provided RNG.
    pub const fn new(rng: T) -> Self {
        Self(rng)
    }
}

impl<T> rand_core09::RngCore for RandAdaptor<T>
where
    T: RngCore,
{
    fn next_u32(&mut self) -> u32 {
        self.0.next_u32()
    }

    fn next_u64(&mut self) -> u64 {
        self.0.next_u64()
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.0.fill_bytes(dest);
    }
}

impl<T> rand_core09::CryptoRng for RandAdaptor<T> where T: RngCore + CryptoRng {}

impl<T> RngCore for RandAdaptor<T>
where
    T: rand_core09::RngCore,
{
    fn next_u32(&mut self) -> u32 {
        self.0.next_u32()
    }

    fn next_u64(&mut self) -> u64 {
        self.0.next_u64()
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.0.fill_bytes(dest);
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core06::Error> {
        self.0.fill_bytes(dest);

        Ok(())
    }
}
