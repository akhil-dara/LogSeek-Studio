//! Grouping rows by a column's values, with counts and times.
use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::exec::{ChunkCache, MatchSet};
use logseek_core::query::group::{GroupLevel, GroupRow, GroupSort};
use logseek_core::store::Store;
use logseek_core::types::fmt_time;
use std::io::Write;
use std::sync::atomic::AtomicBool;

fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("g.csv");
    let mut f = std::fs::File::create(&src).unwrap();
    write!(
        f,
        "time,host,user\n\
         2024-03-01 10:00:00,web1,alice\n\
         2024-03-01 10:05:00,web2,bob\n\
         2024-03-01 10:01:00,web1,carol\n\
         2024-03-01 09:59:00,web1,dave\n\
         2024-03-01 11:00:00,db1,alice\n\
         2024-03-01 08:00:00,,erin\n"
    )
    .unwrap();
    drop(f);
    let dir = d.path().join("g.lsidx");
    let job = start_ingest(&src, &dir, Budget::new(64 << 20), IngestOptions::default());
    while matches!(job.state(), JobState::Running) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    (d, Store::open(&dir).unwrap())
}

fn level(st: &Store, sort: GroupSort) -> GroupLevel {
    let cache = ChunkCache::new(Budget::new(32 << 20));
    let m = MatchSet::all(st);
    GroupLevel::build(st, &m, "host", sort, &cache, &AtomicBool::new(false)).unwrap()
}

fn names(rows: &[GroupRow]) -> Vec<Option<&str>> {
    rows.iter().map(|r| r.value.as_deref()).collect()
}

#[test]
fn groups_count_every_value_with_first_and_last_time() {
    let (_d, st) = store();
    let l = level(&st, GroupSort::MostRows);
    assert_eq!(l.len(), 4);
    let rows = l.page(0, 10);
    assert_eq!(
        names(&rows),
        [Some("web1"), Some("db1"), Some("web2"), None]
    );
    let web1 = &rows[0];
    assert_eq!(web1.rows, 3);
    assert_eq!(fmt_time(web1.first.unwrap()), "2024-03-01 09:59:00");
    assert_eq!(fmt_time(web1.last.unwrap()), "2024-03-01 10:01:00");
    // the empty value is a group of its own, named honestly
    assert_eq!(rows[3].rows, 1);
}

#[test]
fn every_sort_order() {
    let (_d, st) = store();
    let n = |s| {
        names(&level(&st, s).page(0, 10))
            .into_iter()
            .map(|v| v.unwrap_or("-").to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(n(GroupSort::FewestRows), ["db1", "web2", "-", "web1"]);
    assert_eq!(n(GroupSort::FirstSeen), ["-", "web1", "web2", "db1"]);
    assert_eq!(n(GroupSort::LastSeen), ["db1", "web2", "web1", "-"]);
    assert_eq!(n(GroupSort::Name), ["db1", "web1", "web2", "-"]);
}

#[test]
fn pages_cover_every_group_without_limit() {
    let (_d, st) = store();
    let l = level(&st, GroupSort::Name);
    assert_eq!(names(&l.page(1, 2)), [Some("web1"), Some("web2")]);
    assert_eq!(names(&l.page(3, 50)), [None]);
    assert!(l.page(4, 5).is_empty());
}

#[test]
fn values_of_any_source_are_all_counted_even_when_memory_is_tight() {
    use logseek_core::query::agg::count_values;
    let d = tempfile::tempdir().unwrap();
    // 60,000 distinct values, each twice, plus 7 empty: a tiny budget forces passes
    let cache = ChunkCache::new(Budget::new(2 << 20));
    let read = |each: &mut dyn FnMut(Option<&str>)| -> logseek_core::Result<()> {
        for i in 0..120_000u64 {
            let v = format!("value-{:06}", i / 2);
            each(Some(&v));
        }
        for _ in 0..7 {
            each(None);
        }
        Ok(())
    };
    let r = count_values("made", d.path(), &cache, &AtomicBool::new(false), read).unwrap();
    assert_eq!(r.len(), 60_001);
    assert!(r.passes > 1, "the tight budget should split the counting");
    let total: f64 = r.iter().map(|(_, v)| v[0]).sum();
    assert_eq!(total, 120_007.0);
    let l = GroupLevel::from_stats(r, GroupSort::MostRows);
    let first = l.page(0, 1);
    assert_eq!(first[0].value, None);
    assert_eq!(first[0].rows, 7);
    assert_eq!(l.page(60_000, 5).len(), 1);
}

#[test]
fn search_finds_every_matching_value_and_clearing_it_brings_all_back() {
    let (_d, st) = store();
    let mut l = level(&st, GroupSort::MostRows);
    l.search("WEB");
    assert_eq!(names(&l.page(0, 10)), [Some("web1"), Some("web2")]);
    assert_eq!(l.len(), 2);
    l.search("");
    assert_eq!(l.len(), 4);
}

#[test]
fn groups_of_two_columns_with_a_sum_are_exact_even_when_memory_is_tight() {
    use logseek_core::query::agg::{count_groups, KEY_SEP};
    use logseek_core::query::ast::AggFn;
    use logseek_core::query::group::{SummarySort, SummaryTable};
    let d = tempfile::tempdir().unwrap();
    let cache = ChunkCache::new(Budget::new(2 << 20));
    // 40,000 (host, user) pairs; bytes = i % 10; each pair twice
    let read = |each: &mut dyn FnMut(&str, Option<&str>)| -> logseek_core::Result<()> {
        for i in 0..80_000u64 {
            let k = format!("h{}{}user{:05}", (i / 2) % 7, KEY_SEP, i / 2);
            let b = ((i / 2) % 10).to_string();
            each(&k, Some(&b));
        }
        Ok(())
    };
    let r = count_groups(
        2,
        Some(AggFn::Sum),
        d.path(),
        &cache,
        &AtomicBool::new(false),
        read,
    )
    .unwrap();
    assert!(r.passes > 1, "the tight budget should split the counting");
    assert_eq!(r.len(), 40_000);
    let mut t = SummaryTable::new(r);
    assert_eq!(t.len(), 40_000);
    // every group: 2 rows, sum = 2 × (pair % 10)
    t.sort(SummarySort::Value);
    let top = t.page(0, 1);
    assert_eq!(top[0].rows, 2);
    assert_eq!(top[0].value, 18.0);
    t.sort(SummarySort::Key);
    let first = t.page(0, 1);
    assert_eq!(first[0].key, ["h0", "user00000"]);
    // the longest value per column, for widths
    assert_eq!(t.widths(), &[2, 9]);
    assert_eq!(t.page(39_999, 10).len(), 1);
}

#[test]
fn distinct_and_max_per_group() {
    use logseek_core::query::agg::count_groups;
    use logseek_core::query::ast::AggFn;
    let d = tempfile::tempdir().unwrap();
    let cache = ChunkCache::new(Budget::new(32 << 20));
    let rows = [
        ("a", "x"),
        ("a", "y"),
        ("a", "x"),
        ("b", "7"),
        ("b", "12"),
        ("", "z"),
    ];
    let run = |f| {
        count_groups(
            1,
            Some(f),
            d.path(),
            &cache,
            &AtomicBool::new(false),
            |each| {
                for (k, v) in rows {
                    each(k, Some(v));
                }
                Ok(())
            },
        )
        .unwrap()
    };
    let got = |r: &logseek_core::query::agg::StatsResult, k: &str| {
        r.iter()
            .find(|(key, _)| key[0] == k)
            .map(|(_, v)| (v[0], v[1]))
            .unwrap()
    };
    let dc = run(AggFn::Dc);
    assert_eq!(got(&dc, "a"), (3.0, 2.0));
    // rows with no value in the key column are a group of their own
    assert_eq!(got(&dc, ""), (1.0, 1.0));
    let mx = run(AggFn::Max);
    assert_eq!(got(&mx, "b"), (2.0, 12.0));
}

#[test]
fn the_rows_of_every_group_are_found_again_including_the_group_with_no_value() {
    use logseek_core::query::exec::filter_with;
    use logseek_core::query::group::narrow_term;
    use logseek_core::query::parse;
    let (_d, st) = store();
    let cache = ChunkCache::new(Budget::new(32 << 20));
    let fs = Default::default();
    let l = level(&st, GroupSort::MostRows);
    for g in l.page(0, 10) {
        // opening a group asks for its rows with this term: it must find
        // exactly as many rows as the group says it has
        let term = narrow_term("host", g.value.as_deref());
        let parsed = parse(&term).unwrap().rows_only().unwrap();
        let m = filter_with(&st, &parsed, &fs, &cache, &AtomicBool::new(false)).unwrap();
        assert_eq!(m.count(), g.rows, "group {:?} via `{term}`", g.value);
    }
}

#[test]
fn a_time_column_is_compared_with_a_date_and_time_written_out() {
    use logseek_core::query::exec::filter_with;
    use logseek_core::query::parse;
    let (_d, st) = store();
    let cache = ChunkCache::new(Budget::new(32 << 20));
    let fs = Default::default();
    let count = |q: &str| {
        let parsed = parse(q).unwrap().rows_only().unwrap();
        filter_with(&st, &parsed, &fs, &cache, &AtomicBool::new(false))
            .unwrap()
            .count()
    };
    // the rows are at 08:00, 09:59, 10:00, 10:01, 10:05 and 11:00
    assert_eq!(count(r#"time>="2024-03-01 10:00:00""#), 4);
    assert_eq!(count(r#"time<"2024-03-01 10:00:00""#), 2);
    assert_eq!(
        count(r#"(time>="2024-03-01 10:00:00" time<="2024-03-01 10:05:00")"#),
        3
    );
}
