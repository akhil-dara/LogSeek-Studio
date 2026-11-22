//! An index that keeps only some columns (the rest read from the source)
//! answers exactly like a full index.

use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState, Keep};
use logseek_core::query::agg::run_stats;
use logseek_core::query::ast::{Agg, AggFn};
use logseek_core::query::exec::{filter, page, ChunkCache, MatchSet};
use logseek_core::query::parse;
use logseek_core::store::Store;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

fn wait(job: &logseek_core::ingest::pipeline::IngestJob) -> JobState {
    let t = Instant::now();
    loop {
        let s = job.state();
        if !matches!(s, JobState::Running) {
            return s;
        }
        assert!(t.elapsed() < Duration::from_secs(120), "ingest timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn write_csv(path: &Path, rows: usize) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    writeln!(f, "time,host,status,bytes,message").unwrap();
    for i in 0..rows {
        let msg = if i % 97 == 0 {
            format!("\"mimikatz run, {i}\nsecond line\"")
        } else {
            format!("\"GET /a?id={i}, ok\"")
        };
        writeln!(
            f,
            "2026-11-{:02}T{:02}:{:02}:{:02}Z,web{:02},{},{},{msg}",
            1 + i / 20_000,
            (i / 3600) % 24,
            (i / 60) % 60,
            i % 60,
            i % 13,
            [200, 404, 500][i % 3],
            i * 31 % 9000
        )
        .unwrap();
    }
}

fn import(src: &Path, dir: &Path, keep: Keep) -> Store {
    let o = IngestOptions {
        chunk_bytes: 1 << 20,
        keep,
        ..Default::default()
    };
    assert_eq!(
        wait(&start_ingest(src, dir, Budget::new(256 << 20), o)),
        JobState::Done
    );
    Store::open(dir).unwrap()
}

/// The same file indexed fully and with only `time` kept.
fn pair(rows: usize) -> (tempfile::TempDir, Store, Store) {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("web.csv");
    write_csv(&src, rows);
    let full = import(&src, &d.path().join("full.lsidx"), Keep::All);
    let inpl = import(
        &src,
        &d.path().join("inplace.lsidx"),
        Keep::Only(vec!["time".into()]),
    );
    (d, full, inpl)
}

fn c() -> ChunkCache {
    ChunkCache::new(Budget::new(128 << 20))
}
fn no() -> AtomicBool {
    AtomicBool::new(false)
}
fn rows_of(st: &Store, m: &MatchSet) -> Vec<u64> {
    page(st, m, 0, m.count() as usize, &[], &c()).unwrap().rows
}

#[test]
fn inplace_store_answers_like_full() {
    let (_d, full, inpl) = pair(60_000);
    assert!(
        inpl.disk_bytes() * 2 < full.disk_bytes(),
        "{} vs {}",
        inpl.disk_bytes(),
        full.disk_bytes()
    );
    assert_eq!(inpl.source_columns(), vec![1, 2, 3, 4]);
    assert_eq!(inpl.row_count(), full.row_count());
    for q in [
        "mimikatz",
        "status=404",
        "host=web03 bytes>4000",
        "@2026-11-02..2026-11-03",
        r#"message:"id=59999,""#,
        "host:web1*",
    ] {
        let (a, b) = (
            filter(&full, &parse(q).unwrap(), &c(), &no()).unwrap(),
            filter(&inpl, &parse(q).unwrap(), &c(), &no()).unwrap(),
        );
        assert!(a.count() > 0, "{q}");
        assert_eq!(a.count(), b.count(), "{q}");
        assert_eq!(rows_of(&full, &a), rows_of(&inpl, &b), "{q}");
    }
    let all_f = MatchSet::all(&full);
    let all_i = MatchSet::all(&inpl);
    for start in [0u64, 31_337, 59_950] {
        let a = page(&full, &all_f, start, 50, &[0, 1, 2, 3, 4], &c()).unwrap();
        let b = page(&inpl, &all_i, start, 50, &[0, 1, 2, 3, 4], &c()).unwrap();
        assert_eq!(a.rows, b.rows);
        assert_eq!(a.cells, b.cells, "page at {start}");
    }
    let agg = [Agg {
        func: AggFn::Count,
        col: None,
    }];
    let by = ["host".to_string()];
    let a = run_stats(&full, &all_f, &agg, &by, &c(), &no()).unwrap();
    let b = run_stats(&inpl, &all_i, &agg, &by, &c(), &no()).unwrap();
    let mut a: Vec<_> = a.iter().collect();
    let mut b: Vec<_> = b.iter().collect();
    a.sort_by(|x, y| x.0.cmp(&y.0));
    b.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(a, b);
}

#[test]
fn rebuild_once_under_parallel_queries() {
    let (_d, _full, inpl) = pair(20_000);
    let cache = c();
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| {
                let m = MatchSet::all(&inpl);
                page(&inpl, &m, 0, 20, &[1, 4], &cache).unwrap();
            });
        }
    });
    assert_eq!(inpl.rebuilds(), 1);
}

#[test]
fn changed_source_is_an_error() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("evidence.csv");
    write_csv(&src, 5000);
    let inpl = import(
        &src,
        &d.path().join("i.lsidx"),
        Keep::Only(vec!["time".into()]),
    );
    std::thread::sleep(Duration::from_millis(30));
    let mut bytes = std::fs::read(&src).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] = if bytes[mid] == b'7' { b'8' } else { b'7' };
    std::fs::write(&src, &bytes).unwrap();
    let m = MatchSet::all(&inpl);
    let err = page(&inpl, &m, 0, 20, &[4], &c()).unwrap_err().to_string();
    assert!(
        err.contains("evidence.csv") && err.contains("changed"),
        "{err}"
    );
}

fn import_big(src: &Path, dir: &Path, keep: Keep) -> Store {
    let o = IngestOptions {
        chunk_bytes: 8 << 20,
        keep,
        ..Default::default()
    };
    assert_eq!(
        wait(&start_ingest(src, dir, Budget::new(256 << 20), o)),
        JobState::Done
    );
    Store::open(dir).unwrap()
}

#[test]
fn deep_page_parses_one_window() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("web.csv");
    write_csv(&src, 200_000);
    let full = import_big(&src, &d.path().join("f.lsidx"), Keep::All);
    let inpl = import_big(
        &src,
        &d.path().join("i.lsidx"),
        Keep::Only(vec!["time".into()]),
    );
    assert!(
        inpl.segments()[0].meta().rows >= 16_384,
        "{}",
        inpl.segments()[0].meta().rows
    );
    let (mf, mi) = (MatchSet::all(&full), MatchSet::all(&inpl));
    let cache = c();
    let a = page(&full, &mf, 150_000, 50, &[0, 1, 2, 3, 4], &c()).unwrap();
    let b = page(&inpl, &mi, 150_000, 50, &[0, 1, 2, 3, 4], &cache).unwrap();
    assert_eq!(a.cells, b.cells);
    assert!(
        inpl.rows_rebuilt() <= 2048,
        "parsed {} rows for a 50-row page",
        inpl.rows_rebuilt()
    );
    // the same page again: nothing parsed
    let before = inpl.rows_rebuilt();
    page(&inpl, &mi, 150_010, 50, &[0, 1, 2, 3, 4], &cache).unwrap();
    assert_eq!(inpl.rows_rebuilt(), before);
}

#[test]
fn page_across_segment_edge() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("web.csv");
    write_csv(&src, 200_000);
    let full = import_big(&src, &d.path().join("f.lsidx"), Keep::All);
    let inpl = import_big(
        &src,
        &d.path().join("i.lsidx"),
        Keep::Only(vec!["time".into()]),
    );
    let edge = inpl.segments()[1].meta().row_start;
    let (mf, mi) = (MatchSet::all(&full), MatchSet::all(&inpl));
    let a = page(&full, &mf, edge - 25, 50, &[4, 1], &c()).unwrap();
    let b = page(&inpl, &mi, edge - 25, 50, &[4, 1], &c()).unwrap();
    assert_eq!(a.rows, b.rows);
    assert_eq!(a.cells, b.cells);
    // a filtered page (matches spread across segments) also cuts windows exactly
    let q = parse("status=500").unwrap();
    let (fa, fb) = (
        filter(&full, &q, &c(), &no()).unwrap(),
        filter(&inpl, &q, &c(), &no()).unwrap(),
    );
    let a = page(&full, &fa, 20_000, 50, &[4, 3], &c()).unwrap();
    let b = page(&inpl, &fb, 20_000, 50, &[4, 3], &c()).unwrap();
    assert_eq!(a.cells, b.cells);
}

#[test]
fn word_search_reads_bytes_not_parses() {
    let (_d, full, inpl) = pair(60_000);
    for q in ["mimikatz", r#""id=5999""#, "SECOND LINE", "web03"] {
        let before = inpl.rows_rebuilt();
        let a = filter(&full, &parse(q).unwrap(), &c(), &no()).unwrap();
        let b = filter(&inpl, &parse(q).unwrap(), &c(), &no()).unwrap();
        assert!(a.count() > 0, "{q}");
        assert_eq!(rows_of(&full, &a), rows_of(&inpl, &b), "{q}");
        assert_eq!(
            inpl.rows_rebuilt(),
            before,
            "{q}: a CSV word search parsed rows"
        );
    }
    // words a raw scan cannot settle (a delimiter, a quote) still answer exactly
    for q in [r#""ok, ""#, r#""run, 9""#] {
        let a = filter(&full, &parse(q).unwrap(), &c(), &no()).unwrap();
        let b = filter(&inpl, &parse(q).unwrap(), &c(), &no()).unwrap();
        assert_eq!(rows_of(&full, &a), rows_of(&inpl, &b), "{q}");
    }
}

#[test]
fn json_word_search_parses_only_candidate_windows() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("a.jsonl");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
    for i in 0..50_000 {
        let msg = if i == 31_337 {
            "rare needle here".to_string()
        } else {
            format!("msg {i}")
        };
        writeln!(
            f,
            r#"{{"time":"2026-11-01T00:00:{:02}Z","msg":"{msg}","zqkey":{{"n":{i}}}}}"#,
            i % 60
        )
        .unwrap();
    }
    drop(f);
    let full = import_big(&src, &d.path().join("f.lsidx"), Keep::All);
    let inpl = import_big(
        &src,
        &d.path().join("i.lsidx"),
        Keep::Only(vec!["time".into()]),
    );
    for q in ["needle", r#""msg 4999""#] {
        let a = filter(&full, &parse(q).unwrap(), &c(), &no()).unwrap();
        let b = filter(&inpl, &parse(q).unwrap(), &c(), &no()).unwrap();
        assert!(a.count() > 0, "{q}");
        assert_eq!(rows_of(&full, &a), rows_of(&inpl, &b), "{q}");
    }
    // a key name is in every record's bytes but in no value: no window parsed
    let before = inpl.rows_rebuilt();
    let a = filter(&full, &parse("zqkey").unwrap(), &c(), &no()).unwrap();
    let b = filter(&inpl, &parse("zqkey").unwrap(), &c(), &no()).unwrap();
    assert_eq!(a.count(), b.count());
    assert_eq!(inpl.rows_rebuilt(), before, "a key-name search parsed rows");
    // "needle" + "msg 4999.." hit 1 + 11 windows at most
    assert!(
        inpl.rows_rebuilt() <= 12 * 1024,
        "parsed {}",
        inpl.rows_rebuilt()
    );
}

// ───────── zero-write: nothing on disk, the index in RAM ─────────

fn same_answers(full: &Store, other: &Store) {
    for q in [
        "mimikatz",
        "status=404",
        "host=web03 bytes>4000",
        "@2026-11-02..2026-11-03",
    ] {
        let a = filter(full, &parse(q).unwrap(), &c(), &no()).unwrap();
        let b = filter(other, &parse(q).unwrap(), &c(), &no()).unwrap();
        assert_eq!(rows_of(full, &a), rows_of(other, &b), "{q}");
    }
    let (mf, mo) = (MatchSet::all(full), MatchSet::all(other));
    for start in [0u64, 31_337] {
        let a = page(full, &mf, start, 50, &[0, 1, 2, 3, 4], &c()).unwrap();
        let b = page(other, &mo, start, 50, &[0, 1, 2, 3, 4], &c()).unwrap();
        assert_eq!(a.cells, b.cells, "page at {start}");
    }
}

#[test]
fn zero_write_creates_no_files() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("web.csv");
    write_csv(&src, 60_000);
    let full = import(&src, &d.path().join("full.lsidx"), Keep::All);
    let dir = d.path().join("zero.lsidx");
    let o = IngestOptions {
        chunk_bytes: 1 << 20,
        zero_write: true,
        ..Default::default()
    };
    assert_eq!(
        wait(&start_ingest(&src, &dir, Budget::new(256 << 20), o)),
        JobState::Done
    );
    assert!(!dir.exists(), "zero-write created the index folder");
    let st = Store::open(&dir).unwrap();
    assert_eq!(st.row_count(), 60_000);
    assert_eq!(st.disk_bytes(), 0);
    assert!(st.memory_bytes() > 0);
    same_answers(&full, &st);
}

#[test]
fn zero_write_respects_ram_budget() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("web.csv");
    write_csv(&src, 60_000);
    let full = import(&src, &d.path().join("full.lsidx"), Keep::All);
    let cap = full.disk_bytes() / 3;
    let dir = d.path().join("zero.lsidx");
    let o = IngestOptions {
        chunk_bytes: 1 << 20,
        zero_write: true,
        ram_index_cap: Some(cap),
        ..Default::default()
    };
    assert_eq!(
        wait(&start_ingest(&src, &dir, Budget::new(256 << 20), o)),
        JobState::Done
    );
    let st = Store::open(&dir).unwrap();
    assert_eq!(st.row_count(), 60_000);
    assert!(st.memory_bytes() <= cap, "{} > {cap}", st.memory_bytes());
    assert!(!st.source_columns().is_empty());
    same_answers(&full, &st);
}

#[test]
fn a_full_disk_continues_in_memory() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("web.csv");
    write_csv(&src, 60_000);
    let full = import(&src, &d.path().join("full.lsidx"), Keep::All);
    let dir = d.path().join("mixed.lsidx");
    // room for a few segments on disk, then none at all
    let o = IngestOptions {
        chunk_bytes: 1 << 20,
        disk_budget: Some(16 << 20),
        shrink_budget_after: Some((2, 0)),
        ..Default::default()
    };
    assert_eq!(
        wait(&start_ingest(&src, &dir, Budget::new(256 << 20), o)),
        JobState::Done
    );
    let st = Store::open(&dir).unwrap();
    assert_eq!(st.row_count(), 60_000);
    assert!(
        st.disk_bytes() > 0 && st.memory_bytes() > 0,
        "disk {} memory {} segments {:?}",
        st.disk_bytes(),
        st.memory_bytes(),
        st.manifest().segments
    );
    same_answers(&full, &st);
}

#[test]
fn zero_write_large_file_does_not_hang() {
    // a tight budget the read pipeline fills: segments held in memory must not
    // wait for room the reader keeps taking.
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("web.csv");
    write_csv(&src, 600_000);
    let dir = d.path().join("zero.lsidx");
    let o = IngestOptions {
        chunk_bytes: 1 << 20,
        zero_write: true,
        ram_index_cap: Some(24 << 20),
        ..Default::default()
    };
    // six 1 MB chunks reserve 6 MB each: once the reader holds six, 64 KB is
    // left — less than any segment
    let job = start_ingest(&src, &dir, Budget::new((36 << 20) + (64 << 10)), o);
    let t = Instant::now();
    while matches!(job.state(), JobState::Running) {
        assert!(
            t.elapsed() < Duration::from_secs(60),
            "zero-write import hung"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(job.state(), JobState::Done);
    assert_eq!(Store::open(&dir).unwrap().row_count(), 600_000);
}

#[test]
fn json_escapes_are_found_like_a_full_index() {
    // PHP writes "\/", Go writes "&": the value has "/" and "&", the bytes
    // do not.
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("a.jsonl");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
    for i in 0..20_000 {
        writeln!(
            f,
            r#"{{"time":"2026-11-01T00:00:{:02}Z","url":"http:\/\/x.com\/a?i={i}","q":"a=1&b={i}","c":"cat {i}"}}"#,
            i % 60
        )
        .unwrap();
    }
    drop(f);
    let full = import_big(&src, &d.path().join("f.lsidx"), Keep::All);
    let inpl = import_big(
        &src,
        &d.path().join("i.lsidx"),
        Keep::Only(vec!["time".into()]),
    );
    for q in [r#""x.com/a?i=777""#, r#""a=1&b=1234""#, r#""cat 4321""#] {
        let a = filter(&full, &parse(q).unwrap(), &c(), &no()).unwrap();
        let b = filter(&inpl, &parse(q).unwrap(), &c(), &no()).unwrap();
        assert!(a.count() > 0, "{q}");
        assert_eq!(rows_of(&full, &a), rows_of(&inpl, &b), "{q}");
    }
}

#[test]
fn unbalanced_quotes_rebuild_like_a_full_index() {
    // a row with an unbalanced quote is read one line at a time at import;
    // cut at a window edge it would parse differently.
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("web.csv");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
    writeln!(f, "time,host,status,bytes,message").unwrap();
    for i in 0..9000usize {
        let msg = if i % 1024 == 1023 || i == 5000 {
            format!("\"broken quote {i}, rest")
        } else {
            format!("\"GET /a?id={i}, ok\"")
        };
        writeln!(
            f,
            "2026-11-01T00:{:02}:{:02}Z,web{:02},{},{},{msg}",
            (i / 60) % 60,
            i % 60,
            i % 13,
            [200, 404][i % 2],
            i
        )
        .unwrap();
    }
    drop(f);
    let full = import_big(&src, &d.path().join("f.lsidx"), Keep::All);
    let inpl = import_big(
        &src,
        &d.path().join("i.lsidx"),
        Keep::Only(vec!["time".into()]),
    );
    assert!(!full.manifest().report.bad_quote_spans.is_empty());
    let n = full.schema().len();
    let cols: Vec<usize> = (0..n).collect();
    let (mf, mi) = (MatchSet::all(&full), MatchSet::all(&inpl));
    for start in [0u64, 1000, 2040, 4990, 8000] {
        let a = page(&full, &mf, start, 60, &cols, &c()).unwrap();
        let b = page(&inpl, &mi, start, 60, &cols, &c()).unwrap();
        assert_eq!(a.cells, b.cells, "page at {start}");
    }
}

fn zero_import(src: &Path, dir: &Path) {
    let o = IngestOptions {
        chunk_bytes: 1 << 20,
        zero_write: true,
        ..Default::default()
    };
    assert_eq!(
        wait(&start_ingest(src, dir, Budget::new(256 << 20), o)),
        JobState::Done
    );
}
fn disk_import(src: &Path, dir: &Path) {
    let o = IngestOptions {
        chunk_bytes: 1 << 20,
        disk_budget: Some(1 << 30),
        ..Default::default()
    };
    assert_eq!(
        wait(&start_ingest(src, dir, Budget::new(256 << 20), o)),
        JobState::Done
    );
}

#[test]
fn memory_index_moves_to_disk_when_space_returns() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("web.csv");
    write_csv(&src, 20_000);
    let dir = d.path().join("x.lsidx");
    zero_import(&src, &dir);
    assert!(Store::open(&dir).unwrap().memory_bytes() > 0);
    // space came back: opening again finishes the index on disk
    disk_import(&src, &dir);
    let st = Store::open(&dir).unwrap();
    assert_eq!(st.row_count(), 20_000);
    assert!(
        st.disk_bytes() > 0 && st.memory_bytes() == 0,
        "disk {} memory {}",
        st.disk_bytes(),
        st.memory_bytes()
    );
}

#[test]
fn stale_memory_index_does_not_hide_the_new_one() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("web.csv");
    write_csv(&src, 20_000);
    let dir = d.path().join("x.lsidx");
    zero_import(&src, &dir);
    // the file grows, and there is room on the disk now
    std::thread::sleep(Duration::from_millis(30));
    let mut f = std::fs::OpenOptions::new().append(true).open(&src).unwrap();
    writeln!(f, "2026-11-30T00:00:00Z,web99,200,1,\"appended\"").unwrap();
    drop(f);
    disk_import(&src, &dir);
    let st = Store::open(&dir).unwrap();
    assert_eq!(st.row_count(), 20_001);
    assert_eq!(st.memory_bytes(), 0);
}

#[test]
fn a_finished_disk_index_is_used_when_the_disk_is_full() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("web.csv");
    write_csv(&src, 20_000);
    let dir = d.path().join("x.lsidx");
    disk_import(&src, &dir);
    let disk = Store::open(&dir).unwrap().disk_bytes();
    // next day the disk is full: yesterday's index still opens, nothing re-read
    zero_import(&src, &dir);
    let st = Store::open(&dir).unwrap();
    assert_eq!(st.row_count(), 20_000);
    assert_eq!(st.disk_bytes(), disk);
    assert_eq!(st.memory_bytes(), 0);
}

#[test]
fn rebuilt_segments_give_way_under_a_small_cache() {
    // rebuilt segments are held compressed, but must yield memory when a
    // query needs it.
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("web.csv");
    write_csv(&src, 200_000);
    let o = IngestOptions {
        chunk_bytes: 256 << 10,
        keep: Keep::Only(vec!["time".into()]),
        ..Default::default()
    };
    let dir = d.path().join("i.lsidx");
    assert_eq!(
        wait(&start_ingest(&src, &dir, Budget::new(256 << 20), o)),
        JobState::Done
    );
    let inpl = Store::open(&dir).unwrap();
    assert!(inpl.segments().len() > 40, "{}", inpl.segments().len());
    let small = ChunkCache::new(Budget::new(6 << 20));
    for q in [
        r#"message:"id=1""#,
        r#"host:"web0""#,
        "status=404 bytes>100",
    ] {
        let m =
            filter(&inpl, &parse(q).unwrap(), &small, &no()).unwrap_or_else(|e| panic!("{q}: {e}"));
        page(&inpl, &m, m.count() / 2, 50, &[0, 1, 2, 3, 4], &small)
            .unwrap_or_else(|e| panic!("{q} page: {e}"));
    }
}

#[test]
fn window_reservation_counts_the_window_bytes() {
    // window 0 holds long rows in a segment of short ones: reserve for what is
    // parsed, not the segment's average.
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("web.csv");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
    writeln!(f, "time,host,message").unwrap();
    let long = "x".repeat(1500);
    let mut window_bytes = 0u64;
    for i in 0..60_000usize {
        let msg = if i < 1024 { long.as_str() } else { "ok" };
        let line = format!("2026-11-01T00:00:{:02}Z,web{:02},{msg}\n", i % 60, i % 9);
        if i < 1024 {
            window_bytes += line.len() as u64;
        }
        f.write_all(line.as_bytes()).unwrap();
    }
    drop(f);
    let inpl = import_big(
        &src,
        &d.path().join("i.lsidx"),
        Keep::Only(vec!["time".into()]),
    );
    assert!(inpl.segments()[0].meta().rows > 20_000);
    let budget = Budget::new(256 << 20);
    let cache = ChunkCache::new(budget.clone());
    page(&inpl, &MatchSet::all(&inpl), 0, 10, &[2], &cache).unwrap();
    assert!(
        budget.peak() >= window_bytes * 4,
        "peak {} for a {window_bytes}-byte window",
        budget.peak()
    );
}

#[test]
fn zero_write_spends_ram_on_columns_not_bloom_filters() {
    // in memory, a column read from the source drops its Bloom filter (the
    // byte scan settles words anyway) so cheap columns can be kept instead
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("web.csv");
    write_csv(&src, 60_000);
    let full = import(&src, &d.path().join("full.lsidx"), Keep::All);
    let dir = d.path().join("zero.lsidx");
    let o = IngestOptions {
        chunk_bytes: 1 << 20,
        zero_write: true,
        ram_index_cap: Some(full.disk_bytes() / 3),
        ..Default::default()
    };
    assert_eq!(
        wait(&start_ingest(&src, &dir, Budget::new(256 << 20), o)),
        JobState::Done
    );
    let st = Store::open(&dir).unwrap();
    let mut source_cols = 0;
    for s in st.segments() {
        for c in s.meta().cols.iter().filter(|c| c.in_source) {
            source_cols += 1;
            assert_eq!(
                c.stored_len(),
                0,
                "column {} kept a Bloom filter in memory",
                c.col_id
            );
        }
    }
    assert!(source_cols > 0);
    same_answers(&full, &st);
}
