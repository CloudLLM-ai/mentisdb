//! Regression tests for the session-cohesion nearest-seed lookup.

use mentisdb::search::ranked::nearest_sorted_distance;

/// The binary-search lookup must agree with the old O(seeds) linear scan for
/// every probe value, including values below, between, on, and above seeds.
#[test]
fn nearest_sorted_distance_matches_linear_scan() {
    // Deterministic xorshift so the test needs no extra dependencies.
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for round in 0..200 {
        let seed_count = (next() % 30) as usize;
        let mut seeds: Vec<u64> = (0..seed_count).map(|_| next() % 500).collect();
        seeds.sort_unstable();
        for probe in 0..520u64 {
            let linear = seeds
                .iter()
                .map(|seed| seed.abs_diff(probe))
                .min()
                .unwrap_or(u64::MAX);
            assert_eq!(
                nearest_sorted_distance(&seeds, probe),
                linear,
                "round {round} probe {probe} seeds {seeds:?}"
            );
        }
    }
}

/// An empty seed list has no nearest neighbour.
#[test]
fn nearest_sorted_distance_of_empty_is_max() {
    assert_eq!(nearest_sorted_distance(&[], 0), u64::MAX);
}
