//! Priority, affinity and efficiency mode on the real backend (spec Phase 6): set and read
//! back on a `sleeper` this test spawned, through a pin.
#![cfg(windows)]
#![allow(clippy::unwrap_used)]

use atropos_core::platform::Platform;
use atropos_core::platform::windows::WindowsPlatform;
use atropos_core::tune::{ACCESS, Priority, Tune};

#[test]
fn priority_affinity_and_eco_round_trip_on_a_sleeper() {
    let child = atropos_fixtures::spawn("sleeper", &["--secs", "60"]).unwrap();
    let mut p = WindowsPlatform::new();
    let pin = p.pin(child.key, ACCESS).unwrap();
    let before = p.tuning(&pin).unwrap();
    assert_eq!(before.priority, Priority::Normal);
    assert_ne!(before.system_affinity, 0);

    for pr in [Priority::BelowNormal, Priority::Idle, Priority::AboveNormal] {
        p.tune(&pin, Tune::Priority(pr)).unwrap();
        assert_eq!(p.tuning(&pin).unwrap().priority, pr);
    }
    assert!(p.tune(&pin, Tune::Priority(Priority::Realtime)).is_err());

    // The lowest CPU the system offers.
    let one = before.system_affinity & before.system_affinity.wrapping_neg();
    p.tune(&pin, Tune::Affinity(one)).unwrap();
    assert_eq!(p.tuning(&pin).unwrap().affinity, one);

    p.tune(&pin, Tune::Eco(true)).unwrap();
    if let Some(eco) = p.tuning(&pin).unwrap().eco {
        assert!(eco, "efficiency mode reads back on");
        p.tune(&pin, Tune::Eco(false)).unwrap();
        assert_eq!(p.tuning(&pin).unwrap().eco, Some(false));
    }
}
