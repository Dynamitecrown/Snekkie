//! Small seeded random numbers: the same seed and the same input always
//! play the same run.

/// SplitMix64. Fast, tiny and good enough for game decisions.
#[derive(Debug, Clone)]
pub struct Rng(u64);

/// Purposes for independent streams, so combat randomness can never shift
/// a later wave's layout or an upgrade offer.
pub const STREAM_COMBAT: u64 = 0xC0BA_7000_0000_0001;
pub const STREAM_LAYOUT: u64 = 0x1A70_0700_0000_0002;
pub const STREAM_OFFERS: u64 = 0x0FFE_2500_0000_0003;
pub const STREAM_SCHEDULE: u64 = 0x5CED_0100_0000_0004;
pub const STREAM_ASTEROIDS: u64 = 0xA57E_0D00_0000_0005;

/// The SplitMix64 finalizer: spreads every input bit across the output.
pub fn mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(mix(seed ^ 0x9E37_79B9_7F4A_7C15))
    }

    /// An independent stream for one purpose and index (a wave, an offer).
    pub fn stream(seed: u64, purpose: u64, index: u64) -> Self {
        Rng::new(mix(seed ^ purpose).wrapping_add(mix(index.wrapping_add(purpose))))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        mix(self.0)
    }

    /// Uniform in `0..n`; 0 when `n` is 0.
    pub fn below(&mut self, n: u32) -> u32 {
        (((self.next_u64() >> 32) * u64::from(n)) >> 32) as u32
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    pub fn range(&mut self, low: f32, high: f32) -> f32 {
        low + (high - low) * self.unit()
    }

    pub fn chance(&mut self, probability: f32) -> bool {
        self.unit() < probability
    }

    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i as u32 + 1) as usize;
            items.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_numbers_and_streams_are_independent() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        let first: Vec<u64> = (0..8).map(|_| a.next_u64()).collect();
        assert_eq!(first, (0..8).map(|_| b.next_u64()).collect::<Vec<_>>());
        assert_ne!(Rng::stream(42, STREAM_LAYOUT, 1).next_u64(), Rng::stream(42, STREAM_LAYOUT, 2).next_u64());
        assert_ne!(Rng::stream(42, STREAM_LAYOUT, 1).next_u64(), Rng::stream(42, STREAM_OFFERS, 1).next_u64());
        assert_ne!(Rng::stream(42, STREAM_COMBAT, 0).next_u64(), Rng::stream(42, STREAM_ASTEROIDS, 0).next_u64());
    }

    #[test]
    fn ranges_stay_in_bounds() {
        let mut rng = Rng::new(7);
        for _ in 0..10_000 {
            assert!(rng.below(6) < 6);
            let unit = rng.unit();
            assert!((0.0..1.0).contains(&unit));
            let value = rng.range(-2.0, 3.0);
            assert!((-2.0..3.0).contains(&value));
        }
        assert_eq!(rng.below(0), 0);
        let mut items: Vec<u32> = (0..20).collect();
        rng.shuffle(&mut items);
        items.sort_unstable();
        assert_eq!(items, (0..20).collect::<Vec<_>>());
    }
}
