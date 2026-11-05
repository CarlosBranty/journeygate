//! Deterministic Choice option permutation (calibration against position bias).
//!
//! SplitMix64 + Fisher-Yates exactly as written in
//! `docs/contracts/jev-questions.md`, so a seed recorded in the decision log
//! replays the same option order.

/// XOR applied to the seed for the `kind` Choice (`area` uses the seed as-is).
pub const KIND_SEED_XOR: u64 = 0xD1B5_4A32_D192_ED03;

/// SplitMix64 PRNG (all arithmetic wrapping).
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// Shuffles `items` in place: `for i in (1..n).rev() { j = next() % (i + 1); swap(i, j) }`.
pub fn permute<T>(items: &mut [T], seed: u64) {
    let mut rng = SplitMix64::new(seed);
    for i in (1..items.len()).rev() {
        let j = (rng.next_u64() % (i as u64 + 1)) as usize;
        items.swap(i, j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_seeds_usually_give_different_orders() {
        let mut a: Vec<u8> = (0..8).collect();
        let mut b = a.clone();
        permute(&mut a, 1);
        permute(&mut b, 2);
        assert_ne!(a, b);
    }
}
