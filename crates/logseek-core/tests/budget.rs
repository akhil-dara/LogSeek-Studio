//! The memory budget: reservations, caps and component accounting.
use logseek_core::budget::{Budget, Component};

#[test]
fn reserve_within_cap_succeeds_and_drop_releases() {
    let b = Budget::new(100);
    let r = b
        .try_reserve(60, Component::Ingest)
        .expect("60 of 100 fits");
    assert_eq!(b.used(), 60);
    drop(r);
    assert_eq!(b.used(), 0);
    assert_eq!(b.peak(), 60);
}

#[test]
fn reserve_over_cap_fails_without_side_effects() {
    let b = Budget::new(100);
    let _r = b.try_reserve(60, Component::Query).unwrap();
    assert!(b.try_reserve(50, Component::Query).is_err());
    assert_eq!(b.used(), 60);
}

#[test]
fn grow_and_shrink_track_usage() {
    let b = Budget::new(100);
    let mut r = b.try_reserve(10, Component::Cache).unwrap();
    r.grow(30).unwrap();
    assert_eq!(b.used(), 40);
    assert!(r.grow(70).is_err());
    assert_eq!(b.used(), 40);
    r.shrink(5);
    assert_eq!(b.used(), 5);
    assert_eq!(r.bytes(), 5);
}

#[test]
fn usage_is_reported_per_component() {
    let b = Budget::new(1000);
    let _a = b.try_reserve(100, Component::Ui).unwrap();
    let _c = b.try_reserve(250, Component::Cache).unwrap();
    let by = b.usage_by();
    assert!(by.contains(&(Component::Ui, 100)));
    assert!(by.contains(&(Component::Cache, 250)));
}

#[test]
fn auto_budget_is_clamped() {
    // floor 96 MiB, ceiling = the default cap (500 MiB on any machine)
    let cap = Budget::auto().cap();
    let total = logseek_core::budget::total_ram_bytes();
    assert!(cap >= 96 * 1024 * 1024, "cap {cap}");
    assert!(
        cap <= logseek_core::budget::default_user_cap(total),
        "cap {cap}"
    );
}

#[test]
fn concurrent_reservations_never_exceed_cap() {
    let b = Budget::new(1_000);
    std::thread::scope(|s| {
        for _ in 0..8 {
            let b = b.clone();
            s.spawn(move || {
                for _ in 0..10_000 {
                    if let Ok(r) = b.try_reserve(7, Component::Other) {
                        drop(r);
                    }
                }
            });
        }
    });
    assert_eq!(b.used(), 0);
    assert!(b.peak() <= 1_000);
}

#[test]
fn an_elastic_budget_moves_between_its_base_and_its_ceiling() {
    use logseek_core::budget::{Budget, Component};
    const MIB: u64 = 1024 * 1024;
    let b = Budget::elastic(100 * MIB, 400 * MIB);
    assert_eq!(
        (b.cap(), b.base(), b.ceiling()),
        (100 * MIB, 100 * MIB, 400 * MIB)
    );
    // at its base it refuses what does not fit
    let held = b.try_reserve(80 * MIB, Component::ALL[0]).unwrap();
    assert!(b.try_reserve(40 * MIB, Component::ALL[0]).is_err());
    // raised, the same request fits; it is never raised past the ceiling
    assert_eq!(b.set_cap(150 * MIB), 150 * MIB);
    let more = b.try_reserve(40 * MIB, Component::ALL[0]).unwrap();
    assert_eq!(b.set_cap(10_000 * MIB), 400 * MIB);
    // nor lowered under the base; lowering frees nothing by itself
    assert_eq!(b.set_cap(0), 100 * MIB);
    assert_eq!(b.used(), 120 * MIB);
    drop((held, more));
    assert_eq!(b.used(), 0);
    // a plain budget is fixed
    let f = Budget::new(64 * MIB);
    assert_eq!(f.set_cap(1 << 40), 64 * MIB);
}

#[test]
fn the_ceiling_follows_what_the_machine_has_free_now() {
    use logseek_core::budget::Budget;
    const MIB: u64 = 1024 * 1024;
    let b = Budget::elastic(100 * MIB, 400 * MIB);
    // more memory came free: the budget may now grow further
    b.set_ceiling(2000 * MIB);
    assert_eq!(b.ceiling(), 2000 * MIB);
    assert_eq!(b.set_cap(1500 * MIB), 1500 * MIB);
    // memory got tight: the ceiling comes down, and so does a cap above it
    b.set_ceiling(300 * MIB);
    assert_eq!(b.ceiling(), 300 * MIB);
    assert_eq!(b.cap(), 300 * MIB);
    // never under where it starts
    b.set_ceiling(10 * MIB);
    assert_eq!(b.ceiling(), 100 * MIB);
    // a plain (pinned) budget does not move
    let f = Budget::new(64 * MIB);
    f.set_ceiling(1 << 40);
    assert_eq!((f.ceiling(), f.set_cap(1 << 40)), (64 * MIB, 64 * MIB));
}
