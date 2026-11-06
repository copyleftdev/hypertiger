//! A seeded generator, so that a seed is a complete description of a run.

/// xoshiro256** seeded through splitmix64, written out so that a dependency's version bump can
/// never change what a seed means.
#[derive(Clone, Debug)]
pub struct Prng {
    state: [u64; 4],
}

impl Prng {
    #[must_use]
    pub fn new(seed: u64) -> Prng {
        let mut spread = seed;
        let mut state = [0u64; 4];
        for word in &mut state {
            spread = spread.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut mixed = spread;
            mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            *word = mixed ^ (mixed >> 31);
        }
        assert!(state.iter().any(|&word| word != 0));
        Prng { state }
    }

    pub fn next_u64(&mut self) -> u64 {
        let result = self.state[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let shifted = self.state[1] << 17;
        self.state[2] ^= self.state[0];
        self.state[3] ^= self.state[1];
        self.state[1] ^= self.state[2];
        self.state[0] ^= self.state[3];
        self.state[2] ^= shifted;
        self.state[3] = self.state[3].rotate_left(45);
        result
    }

    /// A value in `low..=high`.
    pub fn range(&mut self, low: u64, high: u64) -> u64 {
        assert!(low <= high);
        let span = high - low;
        if span == u64::MAX {
            return self.next_u64();
        }
        low + self.next_u64() % (span + 1)
    }

    /// True `per_thousand` times in a thousand. Integer odds keep floats out of the simulator too.
    pub fn chance(&mut self, per_thousand: u32) -> bool {
        assert!(per_thousand <= 1000);
        self.range(0, 999) < u64::from(per_thousand)
    }

    pub fn index(&mut self, count: usize) -> usize {
        assert!(count > 0);
        self.range(0, count as u64 - 1) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_is_a_fixed_sequence() {
        let mut first = Prng::new(42);
        let mut second = Prng::new(42);
        for _ in 0..1000 {
            assert_eq!(first.next_u64(), second.next_u64());
        }
        // Pinned, so that changing the generator is seen as breaking every recorded seed.
        assert_eq!(Prng::new(0).next_u64(), 11_091_344_671_253_066_420);
    }

    #[test]
    fn ranges_and_chances_stay_in_bounds() {
        let mut prng = Prng::new(7);
        for _ in 0..10_000 {
            let value = prng.range(5, 9);
            assert!((5..=9).contains(&value));
        }
        assert!(!prng.chance(0));
        assert!(prng.chance(1000));
        assert_eq!(prng.range(3, 3), 3);
    }
}
