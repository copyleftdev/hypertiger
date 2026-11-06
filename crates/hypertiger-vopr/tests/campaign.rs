//! A small campaign in the ordinary test run. The gate runs a far larger one with the release binary.

use hypertiger_vopr::world::{Coverage, Durations, run_seed};

const DURATIONS: Durations = Durations { faults_ms: 60_000, quiet_ms: 30_000 };

fn seeds() -> u64 {
    std::env::var("HYPERTIGER_VOPR_TEST_SEEDS")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(24)
}

#[test]
fn seeds_pass_and_exercise_the_ambiguous_paths() {
    let mut coverage = Coverage::default();
    for seed in 0..seeds() {
        let outcome = run_seed(seed, DURATIONS);
        assert_eq!(outcome.violation, None, "seed {seed}");
        coverage.add(&outcome.coverage);
    }
    assert!(coverage.ambiguous > 0);
    assert!(coverage.never_landed > 0);
    assert!(coverage.fills > 0);
    assert!(coverage.orders_landed > 0);
    assert!(coverage.cancels_landed > 0);
}

#[test]
fn a_seed_replays_exactly() {
    for seed in [1, 7, 99] {
        let first = run_seed(seed, DURATIONS);
        let second = run_seed(seed, DURATIONS);
        assert_eq!(first.trace_digest, second.trace_digest, "seed {seed}");
        assert_eq!(first.coverage, second.coverage, "seed {seed}");
    }
}
