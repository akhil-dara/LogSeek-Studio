//! Column roles from the data alone: constants (checked on every row), ids,
//! sparse columns of a wide file, and informative columns ranked for a smart view.

use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::exec::ChunkCache;
use logseek_core::query::roles::{profile, smart_columns, Role};
use logseek_core::store::Store;
use std::fmt::Write;
use std::sync::atomic::AtomicBool;

/// 14 columns, 6,000 rows: Time, a constant, an id, a sparse column, a column
/// that is constant except on one row late in the file, and informative ones.
fn make() -> (tempfile::TempDir, Store) {
    let mut text = String::from(
        "Time,Version,ReqId,Rare,AlmostSame,Level,Host,User,Status,Bytes,Path,Region,Kind,Note,NearSame,Stamp,LevelName,Pid,Sev\n",
    );
    for i in 0..6_000u32 {
        let rare = if i % 50 == 0 { "x" } else { "" };
        let almost = if i == 5_999 { "other" } else { "same" };
        // the same address on 99.5% of rows: hardly telling, though well filled
        let near = if i % 200 == 7 { "10.0.0.2" } else { "10.0.0.1" };
        // a 13-digit number different on 90% of rows: an identifier, not something to scan
        let stamp = 1_794_521_493_000u64 + (i - u32::from(i % 10 == 9)) as u64 * 7;
        // the same fact as Level, written another way
        let level_name = format!("{}V1", ["info", "warn", "error"][(i % 3) as usize]);
        // a 13-digit process number shared by a few rows each: 60% different in the sample
        let pid = 7_078_107_000_000u64 + (i % 3_000) as u64 * 13;
        // four categories, on 55% of the rows
        let sev = if i % 20 < 11 {
            ["low", "mid", "high", "crit"][(i % 4) as usize]
        } else {
            ""
        };
        writeln!(
            text,
            "2026-11-12 10:{:02}:{:02},1.6.0,req-{:016x},{rare},{almost},{},h{},u{},{},{},/a/{},r{},k{},n{},{near},{stamp},{level_name},{pid},{sev}",
            (i / 60) % 60,
            i % 60,
            (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15),
            ["info", "warn", "error"][(i % 3) as usize],
            i % 7,
            i % 11,
            [200, 404, 500][(i % 3) as usize],
            (i * 37) % 900,
            i % 40,
            i % 5,
            i % 4,
            i % 13
        )
        .unwrap();
    }
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("t.csv");
    std::fs::write(&src, text).unwrap();
    let dir = d.path().join("t.lsidx");
    let job = start_ingest(
        &src,
        &dir,
        Budget::new(256 << 20),
        IngestOptions {
            chunk_bytes: 64 * 1024,
            ..Default::default()
        },
    );
    while matches!(job.state(), JobState::Running) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(job.state(), JobState::Done);
    (d, Store::open(&dir).unwrap())
}

#[test]
fn each_column_is_told_by_its_numbers_and_constants_are_checked_on_every_row() {
    let (_d, st) = make();
    assert!(
        st.segments().len() > 3,
        "several blocks, so the sample is spread"
    );
    let c = ChunkCache::new(Budget::new(64 << 20));
    let p = profile(&st, &c, &AtomicBool::new(false)).unwrap();
    let role = |name: &str| {
        let i = st
            .schema()
            .columns
            .iter()
            .position(|c| c.name == name)
            .unwrap();
        p[i].role
    };
    assert_eq!(role("Time"), Role::Anchor);
    assert_eq!(role("Version"), Role::Constant);
    let v = st
        .schema()
        .columns
        .iter()
        .position(|c| c.name == "Version")
        .unwrap();
    assert_eq!(p[v].constant.as_deref(), Some("1.6.0"));
    assert_eq!(role("ReqId"), Role::Id);
    assert_eq!(role("Rare"), Role::Sparse);
    // one different row at the very end: not a constant, whatever the sample saw
    assert_ne!(role("AlmostSame"), Role::Constant);
    for name in ["Level", "Host", "User", "Status", "Region", "Kind"] {
        assert_eq!(role(name), Role::Informative, "{name}");
    }
    // the smart view: the anchor first, then the most telling informative columns
    let smart = smart_columns(&p, 8);
    assert_eq!(smart[0], st.schema().time_col.unwrap());
    assert_eq!(smart.len(), 9);
    for name in ["Version", "ReqId", "Rare"] {
        let i = st
            .schema()
            .columns
            .iter()
            .position(|c| c.name == name)
            .unwrap();
        assert!(!smart.contains(&i), "{name} is not in the smart view");
    }
}

#[test]
fn each_column_names_its_most_common_sampled_values_with_their_rows() {
    let (_d, st) = make();
    let c = ChunkCache::new(Budget::new(64 << 20));
    let p = profile(&st, &c, &AtomicBool::new(false)).unwrap();
    let at = |name: &str| {
        st.schema()
            .columns
            .iter()
            .position(|c| c.name == name)
            .unwrap()
    };
    // Level: three values, evenly spread; most first, then by text
    let lv = &p[at("Level")];
    let names: Vec<&str> = lv.top.iter().map(|(v, _)| v.as_str()).collect();
    assert_eq!(names.len(), 3);
    for v in ["info", "warn", "error"] {
        assert!(names.contains(&v), "{v} in {names:?}");
    }
    let total: u64 = lv.top.iter().map(|(_, n)| *n).sum();
    assert_eq!(total, lv.sampled, "three values cover every sampled row");
    // a constant: its one value, on every sampled row
    let ver = &p[at("Version")];
    assert_eq!(ver.top, vec![("1.6.0".to_string(), ver.sampled)]);
    // never more than three named
    assert!(p.iter().all(|x| x.top.len() <= 3));
}

#[test]
fn a_long_number_different_on_most_rows_ranks_below_the_categories() {
    let (_d, st) = make();
    let c = ChunkCache::new(Budget::new(64 << 20));
    let p = profile(&st, &c, &AtomicBool::new(false)).unwrap();
    let at = |name: &str| {
        st.schema()
            .columns
            .iter()
            .position(|c| c.name == name)
            .unwrap()
    };
    // a few categories on 55% of rows say more at a glance than a full column
    // of process numbers each shared by a couple of rows
    assert!(
        p[at("Sev")].score > p[at("Pid")].score,
        "Sev {} vs Pid {}: a process number seen on few rows each is not something to scan",
        p[at("Sev")].score,
        p[at("Pid")].score
    );
}

#[test]
fn a_long_number_different_on_nearly_every_row_is_an_identifier() {
    let (_d, st) = make();
    let c = ChunkCache::new(Budget::new(64 << 20));
    let p = profile(&st, &c, &AtomicBool::new(false)).unwrap();
    let i = st
        .schema()
        .columns
        .iter()
        .position(|c| c.name == "Stamp")
        .unwrap();
    assert_eq!(p[i].role, Role::Id);
}

#[test]
fn two_columns_that_say_the_same_thing_take_one_place() {
    let (_d, st) = make();
    let c = ChunkCache::new(Budget::new(64 << 20));
    let p = profile(&st, &c, &AtomicBool::new(false)).unwrap();
    // more places than informative columns: only a mirror would be left out
    let smart = smart_columns(&p, 16);
    let at = |name: &str| {
        st.schema()
            .columns
            .iter()
            .position(|c| c.name == name)
            .unwrap()
    };
    let both = [at("Level"), at("LevelName")]
        .iter()
        .filter(|i| smart.contains(i))
        .count();
    assert_eq!(both, 1, "Level and LevelName mirror each other");
}

#[test]
fn a_column_with_nearly_one_value_is_not_among_the_most_telling() {
    let (_d, st) = make();
    let c = ChunkCache::new(Budget::new(64 << 20));
    let p = profile(&st, &c, &AtomicBool::new(false)).unwrap();
    let smart = smart_columns(&p, 8);
    // eight columns with real variety exist, so the near-constant ones stay out
    for name in ["NearSame", "AlmostSame"] {
        let i = st
            .schema()
            .columns
            .iter()
            .position(|c| c.name == name)
            .unwrap();
        assert!(
            !smart.contains(&i),
            "{name} holds one value on nearly every row"
        );
    }
}
