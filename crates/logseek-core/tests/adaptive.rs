//! The adaptive plan: the memory budget stays at the default cap, while more
//! free RAM and cores must turn into more speed and more features,
//! automatically, never fewer.
use logseek_core::budget::{default_user_cap, Machine, Mode, Plan};

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

#[test]
fn the_default_cap_is_500_mb_on_any_machine() {
    assert_eq!(default_user_cap(2 * GIB), 500 * MIB);
    assert_eq!(default_user_cap(4 * GIB), 500 * MIB);
    assert_eq!(default_user_cap(32 * GIB), 500 * MIB);
}

#[test]
fn the_budget_takes_70_percent_of_free_ram_up_to_the_cap() {
    let m = Machine {
        total_ram: 16 * GIB,
        available_ram: 10 * GIB,
        cores: 8,
    };
    assert_eq!(m.budget_cap(None), 500 * MIB); // never more unless asked
    assert_eq!(m.budget_cap(Some(4 * GIB)), 4 * GIB); // the user can ask for more
    assert_eq!(m.budget_cap(Some(256 * MIB)), 256 * MIB); // or for less
    let tight = Machine {
        total_ram: 16 * GIB,
        available_ram: 300 * MIB,
        cores: 8,
    };
    assert_eq!(tight.budget_cap(None), 210 * MIB); // other apps took the RAM
    let tiny = Machine {
        total_ram: 512 * MIB,
        available_ram: 100 * MIB,
        cores: 1,
    };
    assert_eq!(tiny.budget_cap(None), 96 * MIB); // floor
}

#[test]
fn modes_follow_budget_and_cores() {
    assert_eq!(Mode::pick(512 * MIB, 2), Mode::Lean);
    assert_eq!(Mode::pick(512 * MIB, 16), Mode::Lean); // memory is the limit
    assert_eq!(Mode::pick(GIB, 4), Mode::Standard);
    assert_eq!(Mode::pick(4 * GIB, 4), Mode::Standard); // cores are the limit
    assert_eq!(Mode::pick(4 * GIB, 8), Mode::Turbo);
}

#[test]
fn more_resources_never_remove_a_feature_and_only_add_speed() {
    let lean = Plan::for_resources(512 * MIB, 2);
    let std_ = Plan::for_resources(2 * GIB, 6);
    let turbo = Plan::for_resources(8 * GIB, 16);
    assert_eq!(
        (lean.mode, std_.mode, turbo.mode),
        (Mode::Lean, Mode::Standard, Mode::Turbo)
    );
    for (a, b) in [(&lean, &std_), (&std_, &turbo)] {
        assert!(b.workers >= a.workers);
        assert!(b.cache_bytes >= a.cache_bytes);
        assert!(b.bloom_bytes_per_block >= a.bloom_bytes_per_block);
        assert!(b.single_pass_groups >= a.single_pass_groups);
        // extras only ever switch on
        assert!(b.token_index >= a.token_index);
        assert!(b.second_rollups >= a.second_rollups);
        assert!(b.full_previews >= a.full_previews);
    }
    assert!(!lean.token_index && turbo.token_index && turbo.full_previews);
    // every plan stays inside its budget: cache + ingest working set ≤ cap
    for p in [&lean, &std_, &turbo] {
        assert!(p.cache_bytes + p.ingest_bytes <= p.cap, "{p:?}");
        assert!(p.workers >= 1);
    }
    assert_eq!(lean.workers, 2); // a 2-core machine
    assert_eq!(Plan::for_resources(512 * MIB, 16).workers, 8); // threads are free; memory is governed
    assert!(turbo.workers >= 8);
}

#[test]
fn the_status_word_and_hint_are_plain_language() {
    let lean = Plan::for_resources(512 * MIB, 4);
    assert_eq!(lean.mode.label(), "Lean");
    assert!(lean.unlock_hint().unwrap().contains("RAM"));
    let turbo = Plan::for_resources(8 * GIB, 16);
    assert_eq!(turbo.unlock_hint(), None);
}

#[test]
fn the_machine_can_be_measured() {
    let m = Machine::detect();
    assert!(m.cores >= 1);
    assert!(m.total_ram >= m.available_ram);
    let p = Plan::for_machine(&m, None);
    assert!(p.cap >= 96 * MIB);
}

#[test]
fn the_app_may_grow_to_what_the_machine_can_spare_and_no_further() {
    let m = Machine {
        total_ram: 16 * GIB,
        available_ram: 10 * GIB,
        cores: 8,
    };
    // starts at 500 MB; may grow to most of what is free (no fixed share of
    // the machine: a big machine with memory to spare makes it faster)
    assert_eq!(m.budget_cap(None), 500 * MIB);
    assert_eq!(m.elastic_ceiling(), 10 * GIB / 10 * 7);
    let big = Machine {
        total_ram: 128 * GIB,
        available_ram: 100 * GIB,
        cores: 32,
    };
    assert_eq!(big.elastic_ceiling(), 100 * GIB / 10 * 7);
    // little free: 70 % of it
    let busy = Machine {
        total_ram: 16 * GIB,
        available_ram: 2 * GIB,
        cores: 8,
    };
    assert_eq!(busy.elastic_ceiling(), 2 * GIB / 10 * 7);
    // nothing to spare: it stays where it starts
    let tight = Machine {
        total_ram: 16 * GIB,
        available_ram: 300 * MIB,
        cores: 8,
    };
    assert_eq!(tight.elastic_ceiling(), tight.budget_cap(None));
}
