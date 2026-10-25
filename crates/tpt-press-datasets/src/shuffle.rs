//! A deterministic shuffle of sample indices.
//!
//! Training usually reads samples in a random order, and each epoch needs a
//! different one. This gives the same order for the same seed on every platform,
//! so a run can be repeated exactly.

/// Returns a permutation of `0..len`, the same for the same `len` and `seed`.
/// Uses Fisher-Yates over a SplitMix64 generator.
pub fn shuffled_order(len: usize, seed: u64) -> Vec<usize> {
    let mut order: Vec<usize> = (0..len).collect();
    let mut state = seed;
    for i in (1..len).rev() {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        // The modulo bias is below 2^-32 for any dataset that fits in memory.
        let j = (z % (i as u64 + 1)) as usize;
        order.swap(i, j);
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_a_permutation_and_repeats_for_a_seed() {
        let order = shuffled_order(1000, 42);
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..1000).collect::<Vec<_>>());
        assert_eq!(order, shuffled_order(1000, 42));
        assert_ne!(order, shuffled_order(1000, 43));
    }

    #[test]
    fn empty_and_single_are_trivial() {
        assert!(shuffled_order(0, 1).is_empty());
        assert_eq!(shuffled_order(1, 1), vec![0]);
    }
}
