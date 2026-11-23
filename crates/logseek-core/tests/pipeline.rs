//! The import pipeline from source file to stored index.
use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::store::Store;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

fn write_csv(path: &Path, rows: usize) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    writeln!(f, "TimeCreated,EventID,Computer,User,CommandLine").unwrap();
    for i in 0..rows {
        writeln!(
            f,
            "2026-11-01 {:02}:{:02}:{:02},{},WS-{:02},\"DOMAIN\\user{}\",\"cmd.exe /c run {} \"\"q\"\"\"",
            (i / 3600) % 24, (i / 60) % 60, i % 60, [4624, 4625, 4688][i % 3], i % 40, i % 7, i
        )
        .unwrap();
    }
}

fn opts() -> IngestOptions {
    IngestOptions {
        chunk_bytes: 256 * 1024,
        ..Default::default()
    }
}

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

#[test]
fn ingest_then_reopen_counts_match() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("ev.csv");
    write_csv(&src, 200_000);
    let store_dir = d.path().join("ev.lsidx");
    let job = start_ingest(&src, &store_dir, Budget::new(256 << 20), opts());
    assert_eq!(wait(&job), JobState::Done);
    let st = Store::open(&store_dir).unwrap();
    assert_eq!(st.row_count(), 200_000);
    let names: Vec<&str> = st
        .schema()
        .columns
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(
        names,
        ["TimeCreated", "EventID", "Computer", "User", "CommandLine"]
    );
    assert_eq!(
        st.cell_text(123_456, 3).unwrap().as_deref(),
        Some("DOMAIN\\user4")
    ); // 123456 % 7 == 4
    assert_eq!(
        st.cell_text(123_456, 4).unwrap().as_deref(),
        Some("cmd.exe /c run 123456 \"q\"")
    );
    assert_eq!(st.schema().time_col, Some(0));
    drop(st);
    let st2 = Store::open(&store_dir).unwrap();
    assert_eq!(st2.row_count(), 200_000);
    // compressed store is much smaller than the CSV
    assert!(st2.disk_bytes() < std::fs::metadata(&src).unwrap().len());
}

#[test]
fn first_segment_visible_before_done() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("big.csv");
    write_csv(&src, 1_500_000);
    let store_dir = d.path().join("big.lsidx");
    let job = start_ingest(
        &src,
        &store_dir,
        Budget::new(256 << 20),
        IngestOptions {
            workers: 1,
            ..opts()
        },
    );
    let t = Instant::now();
    let mut seen_partial = false;
    while t.elapsed() < Duration::from_secs(60) {
        if let Ok(st) = Store::open(&store_dir) {
            let n = st.row_count();
            if n > 0 && matches!(job.state(), JobState::Running) {
                seen_partial = true;
                break;
            }
        }
        if !matches!(job.state(), JobState::Running) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        seen_partial,
        "rows must be readable while the import is still running"
    );
    assert_eq!(wait(&job), JobState::Done);
}

#[test]
fn resume_after_cancel() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("r.csv");
    write_csv(&src, 600_000);
    let store_dir = d.path().join("r.lsidx");
    let job = start_ingest(
        &src,
        &store_dir,
        Budget::new(256 << 20),
        IngestOptions {
            workers: 1,
            ..opts()
        },
    );
    while job.progress().rows() < 100_000 && matches!(job.state(), JobState::Running) {
        std::thread::sleep(Duration::from_millis(2));
    }
    job.cancel();
    assert_eq!(wait(&job), JobState::Cancelled);
    let partial = Store::open(&store_dir).unwrap().row_count();
    assert!(partial < 600_000);
    let job2 = start_ingest(&src, &store_dir, Budget::new(256 << 20), opts());
    assert_eq!(wait(&job2), JobState::Done);
    let st = Store::open(&store_dir).unwrap();
    assert_eq!(st.row_count(), 600_000);
    assert_eq!(st.cell_text(599_999, 1).unwrap().as_deref(), Some("4688"));
}

#[test]
fn raw_line_matches_source_bytes() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("raw.csv");
    write_csv(&src, 50_000);
    let store_dir = d.path().join("raw.lsidx");
    assert_eq!(
        wait(&start_ingest(
            &src,
            &store_dir,
            Budget::new(256 << 20),
            opts()
        )),
        JobState::Done
    );
    let st = Store::open(&store_dir).unwrap();
    let text = std::fs::read_to_string(&src).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    for row in [0u64, 1, 4_242, 49_999] {
        let raw = st.raw_line(row).unwrap();
        assert_eq!(
            String::from_utf8(raw).unwrap().trim_end(),
            lines[row as usize + 1]
        );
    }
}

#[test]
fn disk_full_is_partial() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("p.csv");
    write_csv(&src, 300_000);
    let store_dir = d.path().join("p.lsidx");
    let o = IngestOptions {
        workers: 1,
        fail_after_segments: Some(2),
        ..opts()
    };
    let state = wait(&start_ingest(&src, &store_dir, Budget::new(256 << 20), o));
    assert!(matches!(state, JobState::Partial(_)), "{state:?}");
    let st = Store::open(&store_dir).unwrap();
    assert!(st.row_count() > 0 && st.row_count() < 300_000);
    assert!(st.cell_text(0, 1).unwrap().is_some());
}

#[test]
fn stale_when_source_changes() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("s.csv");
    write_csv(&src, 1_000);
    let store_dir = d.path().join("s.lsidx");
    assert_eq!(
        wait(&start_ingest(
            &src,
            &store_dir,
            Budget::new(256 << 20),
            opts()
        )),
        JobState::Done
    );
    assert!(!Store::open(&store_dir).unwrap().is_stale());
    let mut f = std::fs::OpenOptions::new().append(true).open(&src).unwrap();
    writeln!(f, "2026-11-02 00:00:00,1,WS-01,u,x").unwrap();
    drop(f);
    assert!(Store::open(&store_dir).unwrap().is_stale());
}

#[test]
fn ingest_respects_budget() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("b.csv");
    write_csv(&src, 400_000);
    let store_dir = d.path().join("b.lsidx");
    let budget = Budget::new(64 << 20);
    assert_eq!(
        wait(&start_ingest(
            &src,
            &store_dir,
            budget.clone(),
            IngestOptions {
                chunk_bytes: 4 << 20,
                ..Default::default()
            }
        )),
        JobState::Done
    );
    assert!(budget.peak() <= budget.cap());
    assert!(budget.peak() > 0, "ingest must account its buffers");
    assert_eq!(budget.used(), 0, "everything released");
}

#[test]
fn json_full_file_schema_is_exact() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("j.ndjson");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
    let n = 300_000;
    for i in 0..n {
        if i == 150_001 || i == 150_002 || i == 299_998 {
            writeln!(
                f,
                "{{\"ts\":\"2026-11-01T00:00:00Z\",\"kind\":\"a\",\"rare\":{{\"deep\":{i}}}}}"
            )
            .unwrap();
        } else {
            writeln!(
                f,
                "{{\"ts\":\"2026-11-01T00:00:00Z\",\"kind\":\"b\",\"n\":{i}}}"
            )
            .unwrap();
        }
    }
    drop(f);
    let store_dir = d.path().join("j.lsidx");
    assert_eq!(
        wait(&start_ingest(
            &src,
            &store_dir,
            Budget::new(256 << 20),
            opts()
        )),
        JobState::Done
    );
    let st = Store::open(&store_dir).unwrap();
    assert_eq!(st.row_count(), n as u64);
    let rare = st
        .schema()
        .columns
        .iter()
        .find(|c| c.name == "rare.deep")
        .expect("rare path listed");
    assert_eq!(rare.count, 3);
    let nn = st.schema().columns.iter().find(|c| c.name == "n").unwrap();
    assert_eq!(nn.count, n as u64 - 3);
    let ci = st.schema().index_of("rare.deep").unwrap();
    assert_eq!(
        st.cell_text(150_001, ci).unwrap().as_deref(),
        Some("150001")
    );
    assert_eq!(st.cell_text(5, ci).unwrap(), None);
}

#[test]
fn json_array_one_object_per_line_ingests() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("arr.json");
    let mut s = String::from("[");
    for i in 0..20_000 {
        if i > 0 {
            s.push_str(",\n");
        }
        s.push_str(&format!(
            "{{\"@timestamp\":{},\"name\":\"E{}\",\"event.type[0]\":\"start\"}}",
            1_794_564_646_100i64 + i,
            i % 5
        ));
    }
    s.push(']');
    std::fs::write(&src, s).unwrap();
    let store_dir = d.path().join("arr.lsidx");
    assert_eq!(
        wait(&start_ingest(
            &src,
            &store_dir,
            Budget::new(256 << 20),
            opts()
        )),
        JobState::Done
    );
    let st = Store::open(&store_dir).unwrap();
    assert_eq!(st.row_count(), 20_000);
    let c = st.schema().index_of("event.type[0]").unwrap();
    assert_eq!(st.cell_text(19_999, c).unwrap().as_deref(), Some("start"));
}

#[test]
fn old_index_format_is_rebuilt_not_reported_corrupt() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("v.csv");
    write_csv(&src, 2_000);
    let dir = d.path().join("v.lsidx");
    assert_eq!(
        wait(&start_ingest(&src, &dir, Budget::new(64 << 20), opts())),
        JobState::Done
    );
    // pretend an older LogSeek built it
    let mp = dir.join("manifest.json");
    let m = std::fs::read_to_string(&mp).unwrap().replace(
        &format!("\"version\": {}", logseek_core::store::manifest::VERSION),
        "\"version\": 1",
    );
    std::fs::write(&mp, m).unwrap();
    let e = Store::open(&dir).err().expect("old format must be refused");
    assert!(e.to_string().contains("rebuild"), "{e}");
    assert_eq!(
        wait(&start_ingest(&src, &dir, Budget::new(64 << 20), opts())),
        JobState::Done
    );
    assert_eq!(Store::open(&dir).unwrap().row_count(), 2_000);
}

#[test]
fn pretty_printed_json_objects_back_to_back_are_records_not_lines() {
    // Pretty-printed output from command-line tools and streaming APIs: one object over many lines,
    // objects concatenated with no array and no commas.
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("stream.json");
    let mut s = String::new();
    for i in 0..3 {
        s.push_str(&format!(
            "{{\n    \"metadata\": {{\n        \"eventType\": \"FirewallMatchEvent\",\n        \"offset\": {i}\n    }},\n    \"event\": {{\n        \"CommandLine\": \"cmd /c \\\"x {{y}}\\\"\",\n        \"Ports\": [1, 2]\n    }}\n}}\n"
        ));
    }
    std::fs::write(&src, s).unwrap();
    let store_dir = d.path().join("s.lsidx");
    let job = start_ingest(&src, &store_dir, Budget::new(256 << 20), opts());
    assert_eq!(wait(&job), JobState::Done);
    let st = Store::open(&store_dir).unwrap();
    assert_eq!(st.row_count(), 3);
    let col = st
        .schema()
        .columns
        .iter()
        .position(|c| c.name == "metadata.offset")
        .expect("metadata.offset column");
    assert_eq!(st.cell_text(2, col).unwrap().as_deref(), Some("2"));
    let cmd = st
        .schema()
        .columns
        .iter()
        .position(|c| c.name == "event.CommandLine")
        .unwrap();
    assert_eq!(
        st.cell_text(0, cmd).unwrap().as_deref(),
        Some("cmd /c \"x {y}\"")
    );

    // a single pretty-printed document is one record
    let one = d.path().join("one.json");
    std::fs::write(
        &one,
        "{\n  \"a\": 1,\n  \"b\": {\n    \"c\": \"x\"\n  }\n}\n",
    )
    .unwrap();
    let dir2 = d.path().join("one.lsidx");
    let job = start_ingest(&one, &dir2, Budget::new(256 << 20), opts());
    assert_eq!(wait(&job), JobState::Done);
    assert_eq!(Store::open(&dir2).unwrap().row_count(), 1);
}

#[test]
fn a_chunk_that_crashes_the_parser_is_reported_and_the_import_finishes() {
    // Fault containment: one bad chunk must not kill a long import.
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("ev.csv");
    write_csv(&src, 20_000);
    let store_dir = d.path().join("s.lsidx");
    let o = IngestOptions {
        workers: 2,
        chunk_bytes: 64 * 1024,
        panic_on_chunk: Some(3),
        ..opts()
    };
    let job = start_ingest(&src, &store_dir, Budget::new(256 << 20), o);
    assert_eq!(wait(&job), JobState::Done);
    let st = Store::open(&store_dir).unwrap();
    let rep = &st.manifest().report;
    assert_eq!(rep.unreadable_spans.len(), 1, "the crashed chunk is listed");
    let (a, b) = rep.unreadable_spans[0];
    assert!(b > a);
    // every other row is there; the lost rows are exactly the ones in that byte span
    let text = std::fs::read(&src).unwrap();
    let lost = text[a as usize..b as usize]
        .iter()
        .filter(|&&c| c == b'\n')
        .count() as u64;
    assert!(lost > 0);
    assert_eq!(st.row_count() + lost, 20_000);
    // a later import of the same store still works (no poisoned state)
    let job2 = start_ingest(&src, &store_dir, Budget::new(256 << 20), opts());
    assert_eq!(wait(&job2), JobState::Done);
}

#[test]
fn displayed_times_are_exactly_the_source_text_whatever_the_zone() {
    // Some exports write local time + zone abbreviation; others write offsets
    // or nothing. What the grid shows must be the original text, byte for byte.
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("t.csv");
    let vals = [
        "2016-08-28 17:58:45.000 MDT",
        "2016-08-28 17:58:46.000 MDT",
        "2016-11-06 01:30:00.000 MST",
        "2016-08-28T17:58:45+05:30",
        "2016-08-28 17:58:45 UTC",
        "17/May/2015:08:05:32 +0000",
        "2016-08-28 17:58:47.000 MDT",
    ];
    let mut s = String::from("when,n\n");
    for (i, v) in vals.iter().enumerate() {
        s.push_str(&format!("\"{v}\",{i}\n"));
    }
    std::fs::write(&src, s).unwrap();
    let dir = d.path().join("s.lsidx");
    let job = start_ingest(&src, &dir, Budget::new(256 << 20), opts());
    assert_eq!(wait(&job), JobState::Done);
    let st = Store::open(&dir).unwrap();
    for (i, v) in vals.iter().enumerate() {
        assert_eq!(
            st.cell_text(i as u64, 0).unwrap().as_deref(),
            Some(*v),
            "row {i}"
        );
    }
}

fn dir_bytes(p: &Path) -> u64 {
    let mut n = 0;
    for e in std::fs::read_dir(p).unwrap().flatten() {
        let m = e.metadata().unwrap();
        n += if m.is_dir() {
            dir_bytes(&e.path())
        } else {
            m.len()
        };
    }
    n
}

#[test]
fn disk_budget_stops_before_the_limit() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("b.csv");
    write_csv(&src, 300_000);
    // the whole index, to size a budget too small even for the row offsets
    // (11 % of this index) and the time column: the only case that still stops
    let full = d.path().join("full.lsidx");
    assert_eq!(
        wait(&start_ingest(&src, &full, Budget::new(256 << 20), opts())),
        JobState::Done
    );
    let total = dir_bytes(&full);
    let budget = total / 12;
    let dir = d.path().join("b.lsidx");
    // and no memory to hold the rest either (otherwise it continues in RAM)
    let o = IngestOptions {
        workers: 1,
        disk_budget: Some(budget),
        ram_index_cap: Some(0),
        ..opts()
    };
    let state = wait(&start_ingest(&src, &dir, Budget::new(256 << 20), o));
    match &state {
        // says what happened and only what the person can do now
        JobState::Partial(m) => assert!(
            m.contains("disk is nearly full")
                && m.contains("open the file again")
                && !m.contains("index folder"),
            "{m}"
        ),
        other => panic!("{other:?}"),
    }
    assert!(
        dir_bytes(&dir) <= budget + (64 << 10),
        "{} > {}",
        dir_bytes(&dir),
        budget
    );
    let rows = Store::open(&dir).unwrap().row_count();
    assert!(rows > 0 && rows < 300_000, "{rows}");
    // space came back: the same index resumes and finishes
    assert_eq!(
        wait(&start_ingest(&src, &dir, Budget::new(256 << 20), opts())),
        JobState::Done
    );
    assert_eq!(Store::open(&dir).unwrap().row_count(), 300_000);
}

#[test]
fn tiny_budget_writes_no_segment() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("t.csv");
    write_csv(&src, 50_000);
    let dir = d.path().join("t.lsidx");
    // no disk and no memory to spare: the only case that stops with nothing
    let o = IngestOptions {
        workers: 1,
        disk_budget: Some(1024),
        ram_index_cap: Some(0),
        ..opts()
    };
    assert!(matches!(
        wait(&start_ingest(&src, &dir, Budget::new(256 << 20), o)),
        JobState::Partial(_)
    ));
    assert_eq!(Store::open(&dir).unwrap().row_count(), 0);
}

// ───────── tiering: keep what fits, read the rest from the source ─────────

use logseek_core::ingest::tiering::choose_kept;

#[test]
fn choose_kept_all_when_it_fits() {
    // 3 columns, 10 MB of source so far, 100 MB left, 1 GB of budget
    let cols = [(0u32, 100_000u64), (1, 2_000_000), (2, 50_000)];
    assert_eq!(
        choose_kept(&cols, &[2], 4096, 10 << 20, 100 << 20, 1 << 30),
        vec![0, 1, 2]
    );
}

#[test]
fn choose_kept_cheapest_first() {
    // scale ×10: all = ~21.5 MB > 5 MB budget; cheapest after the always-kept fit
    let cols = [
        (0u32, 100_000u64),
        (1, 2_000_000),
        (2, 50_000),
        (3, 150_000),
    ];
    let k = choose_kept(&cols, &[2], 4096, 1 << 20, 10 << 20, 5 << 20);
    assert_eq!(k, vec![0, 2, 3]);
}

#[test]
fn choose_kept_never_drops_always() {
    let cols = [(0u32, 100_000u64), (1, 2_000_000), (2, 50_000)];
    assert_eq!(
        choose_kept(&cols, &[1, 2], 4096, 1 << 20, 100 << 20, 1 << 10),
        vec![1, 2]
    );
}

fn rows_cells(st: &Store, q: &str) -> (u64, Vec<Vec<Option<String>>>) {
    use logseek_core::query::exec::{filter, page, ChunkCache};
    let c = ChunkCache::new(Budget::new(128 << 20));
    let m = filter(
        st,
        &logseek_core::query::parse(q).unwrap(),
        &c,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    let n = st.schema().len();
    let p = page(st, &m, m.count() / 2, 40, &(0..n).collect::<Vec<_>>(), &c).unwrap();
    (m.count(), p.cells)
}

#[test]
fn budget_a_third_still_indexes_every_row() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("ev.csv");
    write_csv(&src, 200_000);
    let full_dir = d.path().join("full.lsidx");
    assert_eq!(
        wait(&start_ingest(
            &src,
            &full_dir,
            Budget::new(256 << 20),
            opts()
        )),
        JobState::Done
    );
    let full = Store::open(&full_dir).unwrap();
    let budget = full.disk_bytes() / 3;
    let dir = d.path().join("third.lsidx");
    let o = IngestOptions {
        disk_budget: Some(budget),
        ..opts()
    };
    assert_eq!(
        wait(&start_ingest(&src, &dir, Budget::new(256 << 20), o)),
        JobState::Done
    );
    let st = Store::open(&dir).unwrap();
    assert_eq!(st.row_count(), 200_000);
    assert!(st.disk_bytes() <= budget, "{} > {budget}", st.disk_bytes());
    assert!(!st.source_columns().is_empty());
    // the time column and row offsets stay in the index
    let tc = st.schema().time_col.unwrap();
    assert!(!st.source_columns().contains(&tc));
    for q in ["EventID=4688", "mimikatz", "run", "User:user3"] {
        assert_eq!(rows_cells(&full, q), rows_cells(&st, q), "{q}");
    }
}

#[test]
fn shrinking_free_space_narrows_kept_set() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("ev.csv");
    write_csv(&src, 200_000);
    let full_dir = d.path().join("full.lsidx");
    assert_eq!(
        wait(&start_ingest(
            &src,
            &full_dir,
            Budget::new(256 << 20),
            opts()
        )),
        JobState::Done
    );
    let full_bytes = Store::open(&full_dir).unwrap().disk_bytes();
    let dir = d.path().join("shrink.lsidx");
    // room for the whole index at first; after 4 segments another program
    // takes most of the disk: a third of the full size is left (CommandLine
    // alone is 79 % of this index, so it must move to the source)
    let o = IngestOptions {
        disk_budget: Some(full_bytes * 2),
        shrink_budget_after: Some((4, full_bytes / 3)),
        ..opts()
    };
    assert_eq!(
        wait(&start_ingest(&src, &dir, Budget::new(256 << 20), o)),
        JobState::Done
    );
    let st = Store::open(&dir).unwrap();
    assert_eq!(st.row_count(), 200_000);
    let src_cols = |i: usize| {
        st.segments()[i]
            .meta()
            .cols
            .iter()
            .filter(|c| c.in_source)
            .count()
    };
    assert_eq!(src_cols(0), 0);
    // after the shrink, segments read columns from the source (near the end
    // the room left may fit everything again, which is fine)
    let n = st.segments().len();
    let narrowed = (4..n).filter(|&i| src_cols(i) > 0).count();
    assert!(
        narrowed * 2 > n - 4,
        "{narrowed} of {} later segments narrowed",
        n - 4
    );
    let early: u64 = (0..4)
        .map(|i| std::fs::metadata(st.segments()[i].path()).unwrap().len())
        .sum();
    assert!(
        st.disk_bytes() <= early + full_bytes / 3 + 64 * 1024,
        "{} bytes",
        st.disk_bytes()
    );
}

#[test]
fn a_mostly_empty_time_column_loses_to_a_full_epoch_column() {
    // A "Time" text field on 1 row in 10, epoch ms on every row
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("cs.jsonl");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
    for i in 0..2000u64 {
        if i % 10 == 0 {
            writeln!(
                f,
                r#"{{"Time":"2026-11-11 10:30:00","timestamp":{},"event":"x"}}"#,
                1_794_564_000_000 + i * 1000
            )
            .unwrap();
        } else {
            writeln!(
                f,
                r#"{{"timestamp":{},"event":"y"}}"#,
                1_794_564_000_000 + i * 1000
            )
            .unwrap();
        }
    }
    drop(f);
    let dir = d.path().join("cs.lsidx");
    assert_eq!(
        wait(&start_ingest(&src, &dir, Budget::new(256 << 20), opts())),
        JobState::Done
    );
    let st = Store::open(&dir).unwrap();
    let tc = st.schema().time_col.expect("a time column");
    assert_eq!(st.schema().columns[tc].name, "timestamp");
    assert_eq!(st.schema().time_unit_us, Some(1000));
}

fn time_choice_file(d: &Path) -> std::path::PathBuf {
    let src = d.join("tc.jsonl");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
    for i in 0..500u64 {
        let t = if i % 10 == 0 {
            r#""Time":"2026-11-11 10:30:00","#
        } else {
            ""
        };
        writeln!(
            f,
            r#"{{{t}"timestamp":{},"event":"y"}}"#,
            1_794_564_000_000 + i * 1000
        )
        .unwrap();
    }
    src
}

#[test]
fn the_users_time_choice_beats_the_guess() {
    use logseek_core::ingest::pipeline::TimeChoice;
    let d = tempfile::tempdir().unwrap();
    let src = time_choice_file(d.path());
    for (choice, want) in [
        (TimeChoice::Column("Time".into()), Some("Time")),
        (TimeChoice::Column("timestamp".into()), Some("timestamp")),
        (TimeChoice::Off, None),
    ] {
        let dir = d.path().join(format!("{}.lsidx", want.unwrap_or("off")));
        let o = IngestOptions {
            time: choice,
            ..opts()
        };
        assert_eq!(
            wait(&start_ingest(&src, &dir, Budget::new(256 << 20), o)),
            JobState::Done
        );
        let st = Store::open(&dir).unwrap();
        let got = st
            .schema()
            .time_col
            .map(|t| st.schema().columns[t].name.clone());
        assert_eq!(got.as_deref(), want);
        if want == Some("timestamp") {
            assert_eq!(st.schema().time_unit_us, Some(1000));
        }
    }
}

#[test]
fn a_date_compares_with_a_timeline_kept_as_epoch_numbers() {
    use logseek_core::query::exec::{filter, ChunkCache};
    // epoch milliseconds, one event a second for 2,000 seconds
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("cs.jsonl");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
    let base = 1_794_564_000_000i64;
    for i in 0..2000i64 {
        writeln!(f, r#"{{"timestamp":{},"event":"y"}}"#, base + i * 1000).unwrap();
    }
    drop(f);
    let dir = d.path().join("cs.lsidx");
    assert_eq!(
        wait(&start_ingest(&src, &dir, Budget::new(256 << 20), opts())),
        JobState::Done
    );
    let st = Store::open(&dir).unwrap();
    assert_eq!(st.schema().time_unit_us, Some(1000));
    let cache = ChunkCache::new(Budget::new(64 << 20));
    let count = |q: &str| {
        let q = logseek_core::query::parse(q).unwrap();
        filter(&st, &q, &cache, &std::sync::atomic::AtomicBool::new(false))
            .unwrap()
            .count()
    };
    // the moment of event 1,000, written as a date
    let at = logseek_core::types::fmt_time((base + 1_000_000) * 1000);
    assert_eq!(count(&format!("timestamp>=\"{at}\"")), 1000);
    assert_eq!(count(&format!("timestamp<\"{at}\"")), 1000);
    // a plain number still compares as the number it is
    assert_eq!(count(&format!("timestamp>={}", base + 1_500_000)), 500);
}

#[test]
fn time_axis_is_the_time_column_filled_in_every_row() {
    // as event logs read: one time on every event, another time-like field on a few
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("ev.jsonl");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
    for i in 0..5000 {
        let t = format!(
            "2026-11-14T{:02}:{:02}:{:02}Z",
            (i / 3600) % 24,
            (i / 60) % 60,
            i % 60
        );
        if i % 250 == 0 {
            writeln!(
                f,
                r#"{{"TimeCreated":"{t}","EventID":1,"Date":"2026-09-17"}}"#
            )
            .unwrap();
        } else {
            writeln!(f, r#"{{"TimeCreated":"{t}","EventID":2}}"#).unwrap();
        }
    }
    drop(f);
    let store_dir = d.path().join("ev.lsidx");
    let job = start_ingest(&src, &store_dir, Budget::new(256 << 20), opts());
    assert_eq!(wait(&job), JobState::Done);
    let st = Store::open(&store_dir).unwrap();
    let tc = st
        .schema()
        .time_col
        .map(|c| st.schema().columns[c].name.clone());
    assert_eq!(tc.as_deref(), Some("TimeCreated"));
}

// ───────────── ingest defects found by the 2026-11-21 audit ─────────────

fn import_bytes(
    name: &str,
    bytes: &[u8],
    chunk_bytes: usize,
    cap: u64,
) -> (tempfile::TempDir, Store, JobState) {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join(name);
    std::fs::write(&src, bytes).unwrap();
    let dir = d.path().join("x.lsidx");
    let job = start_ingest(
        &src,
        &dir,
        Budget::new(cap),
        IngestOptions {
            chunk_bytes,
            ..Default::default()
        },
    );
    let state = wait(&job);
    let st = Store::open(&dir).unwrap();
    (d, st, state)
}

fn non_ws(s: &str) -> u64 {
    s.bytes().filter(|b| !b.is_ascii_whitespace()).count() as u64
}

#[test]
fn ndjson_line_starting_with_a_bracket_at_a_chunk_edge_loses_no_record() {
    // reads are at least 64 KiB, so the first chunk edge is at byte 65536:
    // put a line that starts with '[' across it
    let mut data = Vec::new();
    let mut lines = 0u64;
    let mut n = 0;
    while data.len() < 65_536 - 10 {
        data.extend_from_slice(format!("{{\"a\":{n}}}\n").as_bytes());
        n += 1;
        lines += 1;
    }
    data.extend_from_slice(b"[0]");
    data.extend(std::iter::repeat(b' ').take(60));
    data.push(b'\n');
    lines += 1;
    for i in 0..1000 {
        data.extend_from_slice(format!("{{\"a\":{}}}\n", 1_000_000 + i).as_bytes());
        lines += 1;
    }
    let (_d, st, state) = import_bytes("n.ndjson", &data, 16 * 1024, 256 << 20);
    assert_eq!(state, JobState::Done);
    assert_eq!(st.row_count(), lines);
    let a = st.schema().index_of("a").unwrap();
    assert_eq!(
        st.cell_text(lines - 1, a).unwrap().as_deref(),
        Some("1000999")
    );
}

#[test]
fn records_after_the_first_array_or_left_open_at_the_end_are_counted_not_forgotten() {
    // a decoy array first: the later array is not read, and the report says so
    let tail = ",\n  \"Records\": [ {\"evil\": 1}, {\"evil\": 2} ]\n";
    let doc = format!("{{\n  \"decoy\": [ {{\"a\": 1}} ]{tail}}}\n");
    let (_d, st, state) = import_bytes("decoy.json", doc.as_bytes(), 16 * 1024, 256 << 20);
    assert_eq!(state, JobState::Done);
    assert_eq!(st.row_count(), 1);
    let rep = &st.manifest().report;
    assert_eq!(rep.unread_after_records, non_ws(tail));
    assert!(!rep.is_clean());

    // two documents joined
    let second = "{\n \"Records\": [ {\"n\": 2} ]\n}\n";
    let doc = format!("{{\n \"Records\": [ {{\"n\": 1}} ]\n}}\n{second}");
    let (_d, st, _) = import_bytes("two.json", doc.as_bytes(), 16 * 1024, 256 << 20);
    assert_eq!(st.row_count(), 1);
    let rep = &st.manifest().report;
    assert_eq!(rep.unread_after_records, non_ws(second));
    assert!(!rep.is_clean());

    // a file that ends inside its last record
    let doc = "{\n \"Records\": [ {\"a\":1}, {\"a\":2}";
    let (_d, st, state) = import_bytes("cut.json", doc.as_bytes(), 16 * 1024, 256 << 20);
    assert_eq!(state, JobState::Done);
    assert_eq!(st.row_count(), 1);
    let rep = &st.manifest().report;
    assert_eq!(rep.unfinished_record_bytes, 7);
    assert_eq!(rep.unread_after_records, 0);
    assert!(!rep.is_clean());

    // a whole, ordinary wrapper reports nothing
    let doc = "{\n \"Records\": [ {\"a\":1}, {\"a\":2} ]\n}\n";
    let (_d, st, _) = import_bytes("ok.json", doc.as_bytes(), 16 * 1024, 256 << 20);
    assert_eq!(st.row_count(), 2);
    assert!(st.manifest().report.is_clean());
}

fn stray_quote_file(stray_row: &str) -> Vec<u8> {
    let mut s = String::from("ts,user,note\n");
    s.push_str(stray_row);
    s.push('\n');
    s.push_str("2024-01-02,bob,\"line one\n2024-01-03,mallory,deleted logs\"\n");
    for _ in 0..20_000 {
        s.push_str("2024-01-04,x,y\n");
    }
    s.into_bytes()
}

#[test]
fn a_quote_that_csv_core_reads_as_text_does_not_move_a_chunk_edge_into_a_quoted_cell() {
    for stray in [
        "2024-01-01,al\"ice,ok",     // a quote inside an unquoted field
        "2024-01-01,5'10\",ok",      // a quote ending an unquoted field
        "2024-01-01,\"ab\"c\"d,ok",  // a quote after a closing quote, then text
    ] {
        let data = stray_quote_file(stray);
        // reads are 64 KiB, the file is about 300 KB: several chunk edges
        let (_d, st, state) = import_bytes("q.csv", &data, 16 * 1024, 256 << 20);
        assert_eq!(state, JobState::Done, "{stray}");
        assert_eq!(st.row_count(), 2 + 20_000, "{stray}");
        let note = st.schema().index_of("note").unwrap();
        assert_eq!(
            st.cell_text(1, note).unwrap().as_deref(),
            Some("line one\n2024-01-03,mallory,deleted logs"),
            "{stray}"
        );
        let user = st.schema().index_of("user").unwrap();
        for r in 0..st.row_count() {
            assert_ne!(
                st.cell_text(r, user).unwrap().as_deref(),
                Some("mallory"),
                "{stray}: a line inside one cell became a row"
            );
        }
        assert!(st.manifest().report.bad_quote_spans.is_empty(), "{stray}");
    }
}

fn wait_secs(job: &logseek_core::ingest::pipeline::IngestJob, secs: u64) -> JobState {
    let t = Instant::now();
    loop {
        let s = job.state();
        if !matches!(s, JobState::Running) {
            return s;
        }
        if t.elapsed() > Duration::from_secs(secs) {
            job.cancel();
            job.join();
            panic!("the import is still Running after {secs} s: it stalled");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_record_too_long_for_the_memory_budget_is_cut_reported_and_the_import_goes_on() {
    // an opening quote that never closes, past the 1 MiB the format is judged on
    let mut s = String::from("ts,user,note\n");
    let mut lines = 0u64;
    for i in 0..60_000 {
        s.push_str(&format!("2026-01-01,u{},note {i}\n", i % 10));
        lines += 1;
    }
    s.push_str("2026-01-01,\"oops,never closed\n");
    lines += 1;
    for i in 0..150_000 {
        s.push_str(&format!("2026-01-02,u{},later {i}\n", i % 10));
        lines += 1;
    }
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("long.csv");
    std::fs::write(&src, &s).unwrap();
    let dir = d.path().join("long.lsidx");
    let job = start_ingest(
        &src,
        &dir,
        Budget::new(16 << 20),
        IngestOptions {
            chunk_bytes: 64 * 1024,
            ..Default::default()
        },
    );
    assert_eq!(wait_secs(&job, 30), JobState::Done);
    let st = Store::open(&dir).unwrap();
    // every line is a row: the record was cut at a line end, not dropped
    assert_eq!(st.row_count(), lines);
    let rep = &st.manifest().report;
    assert_eq!(rep.unreadable_spans.len(), 1, "{:?}", rep.unreadable_spans);
    let (a, b) = rep.unreadable_spans[0];
    let at = s.find("\"oops").unwrap() as u64;
    assert_eq!(a, at - "2026-01-01,".len() as u64);
    assert!(b > a && s.as_bytes()[b as usize - 1] == b'\n');
    assert!(!rep.is_clean());
}

#[test]
fn a_line_longer_than_the_memory_budget_is_left_out_counted_and_the_rows_after_it_are_read() {
    let mut s = String::new();
    for i in 0..1000 {
        s.push_str(&format!("{{\"n\":{i},\"msg\":\"before\"}}\n"));
    }
    let at = s.len() as u64;
    s.push_str("{\"n\":1000,\"msg\":\"");
    s.push_str(&"x".repeat(4 << 20));
    s.push_str("\"}\n");
    let after = s.len() as u64;
    for i in 1001..2000 {
        s.push_str(&format!("{{\"n\":{i},\"msg\":\"after\"}}\n"));
    }
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("monster.jsonl");
    std::fs::write(&src, &s).unwrap();
    let dir = d.path().join("monster.lsidx");
    let job = start_ingest(
        &src,
        &dir,
        Budget::new(16 << 20),
        IngestOptions {
            chunk_bytes: 64 * 1024,
            ..Default::default()
        },
    );
    assert_eq!(wait_secs(&job, 30), JobState::Done);
    let st = Store::open(&dir).unwrap();
    // every row but the one that could not be held: those before it and those after
    assert_eq!(st.row_count(), 1999);
    let n = st.schema().index_of("n").unwrap();
    assert_eq!(st.cell_text(999, n).unwrap().as_deref(), Some("999"));
    assert_eq!(st.cell_text(1000, n).unwrap().as_deref(), Some("1001"));
    assert_eq!(st.cell_text(1998, n).unwrap().as_deref(), Some("1999"));
    // and it is said: one part, from the start of the line to just past its end
    let rep = &st.manifest().report;
    assert_eq!(rep.unreadable_spans, vec![(at, after)]);
    assert!(!rep.is_clean());
}

#[test]
fn a_last_line_with_no_end_longer_than_the_memory_budget_is_left_out_and_counted() {
    let mut s = String::from("ts,user\n");
    for i in 0..1000 {
        s.push_str(&format!("2026-01-01,u{i}\n"));
    }
    let at = s.len() as u64;
    s.push_str(&"x".repeat(4 << 20));
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("nonl.csv");
    std::fs::write(&src, &s).unwrap();
    let dir = d.path().join("nonl.lsidx");
    let job = start_ingest(
        &src,
        &dir,
        Budget::new(16 << 20),
        IngestOptions {
            chunk_bytes: 64 * 1024,
            ..Default::default()
        },
    );
    assert_eq!(wait_secs(&job, 30), JobState::Done);
    let st = Store::open(&dir).unwrap();
    assert_eq!(st.row_count(), 1000);
    assert_eq!(st.manifest().report.unreadable_spans, vec![(at, s.len() as u64)]);
}

#[test]
fn a_list_element_longer_than_the_memory_budget_is_left_out_counted_and_the_rest_is_read() {
    let mut s = String::from("[{\"n\":0,\"msg\":\"a\"},");
    let at = s.len() as u64;
    s.push_str("{\"n\":1,\"msg\":\"");
    s.push_str(&"x".repeat(4 << 20));
    s.push_str("\"}");
    let end = s.len() as u64;
    s.push_str(",{\"n\":2,\"msg\":\"b\"},{\"n\":3,\"msg\":\"c\"}]");
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("big-element.json");
    std::fs::write(&src, &s).unwrap();
    let dir = d.path().join("big-element.lsidx");
    let job = start_ingest(
        &src,
        &dir,
        Budget::new(16 << 20),
        IngestOptions {
            chunk_bytes: 64 * 1024,
            ..Default::default()
        },
    );
    assert_eq!(wait_secs(&job, 30), JobState::Done);
    let st = Store::open(&dir).unwrap();
    assert_eq!(st.row_count(), 3);
    let n = st.schema().index_of("n").unwrap();
    let got: Vec<Option<String>> = (0..3).map(|r| st.cell_text(r, n).unwrap()).collect();
    assert_eq!(got, [Some("0".into()), Some("2".into()), Some("3".into())]);
    assert_eq!(st.manifest().report.unreadable_spans, vec![(at, end)]);
}

#[test]
fn a_record_written_over_several_lines_among_one_line_records_is_one_row() {
    let data = b"{\"n\":1,\"u\":\"alice\"}\n{\n  \"n\": 2,\n  \"u\": \"bob } [\",\n  \"tags\": [\n    \"a\",\n    \"b\"\n  ]\n}\n{\"n\":3,\"u\":\"carol\"}\n";
    let (_d, st, state) = import_bytes("mixed.jsonl", data, 16 * 1024, 256 << 20);
    assert_eq!(state, JobState::Done);
    // three records, three rows: the lines of the second are not rows of their own
    assert_eq!(st.row_count(), 3);
    assert_eq!(st.manifest().report.invalid_json, 0);
    let u = st.schema().index_of("u").unwrap();
    let got: Vec<Option<String>> = (0..3).map(|r| st.cell_text(r, u).unwrap()).collect();
    assert_eq!(
        got,
        [Some("alice".into()), Some("bob } [".into()), Some("carol".into())]
    );
}

#[test]
fn records_over_several_lines_stay_whole_where_the_file_is_cut_into_chunks() {
    // many chunks: the cuts must fall between records, never inside one
    let mut s = String::new();
    for i in 0..6000 {
        if i % 2 == 0 {
            s.push_str(&format!("{{\"n\":{i},\"u\":\"u{}\"}}
", i % 50));
        } else {
            s.push_str(&format!(
                "{{
  \"n\": {i},
  \"u\": \"u{}\",
  \"tags\": [
    \"a\",
    \"b\"
  ]
}}
",
                i % 50
            ));
        }
    }
    let (_d, st, state) = import_bytes("mixed-big.jsonl", s.as_bytes(), 4 * 1024, 256 << 20);
    assert_eq!(state, JobState::Done);
    assert_eq!(st.row_count(), 6000);
    assert_eq!(st.manifest().report.invalid_json, 0);
}

#[test]
fn a_record_that_opens_and_never_closes_is_still_counted_as_it_was() {
    let data = b"{\"n\":1}\n{\"n\": 2,\n\"u\": \"never closed\n{\"n\":3}\n";
    let (_d, st, state) = import_bytes("broken.jsonl", data, 16 * 1024, 256 << 20);
    assert_eq!(state, JobState::Done);
    // nothing is lost: each line is a row, the broken ones counted as not JSON
    assert_eq!(st.row_count(), 4);
    assert_eq!(st.manifest().report.invalid_json, 2);
}

#[test]
fn a_repeated_json_key_does_not_shift_the_values_of_the_rows_after_it() {
    let data = b"{\"u\":\"alice\",\"u\":\"mallory\"}\n{\"u\":\"bob\"}\n{\"u\":\"carol\"}\n";
    let (_d, st, state) = import_bytes("dup.ndjson", data, 16 * 1024, 256 << 20);
    assert_eq!(state, JobState::Done);
    assert_eq!(st.row_count(), 3);
    let u = st.schema().index_of("u").unwrap();
    let got: Vec<Option<String>> = (0..3).map(|r| st.cell_text(r, u).unwrap()).collect();
    assert_eq!(
        got,
        [Some("mallory".into()), Some("bob".into()), Some("carol".into())]
    );
}

#[test]
fn a_source_column_named_file_keeps_its_values_next_to_the_files_own_name_column() {
    let d = tempfile::tempdir().unwrap();
    let (a, b) = (d.path().join("a.csv"), d.path().join("b.csv"));
    let mut sa = String::from("time,_file,msg\n");
    let mut sb = String::from("time,msg\n");
    for i in 0..6 {
        sa.push_str(&format!("{i},claims-{i}.csv,hello {i}\n"));
        sb.push_str(&format!("{i},other {i}\n"));
    }
    std::fs::write(&a, sa).unwrap();
    std::fs::write(&b, sb).unwrap();
    let dir = d.path().join("set.lsidx");
    let job = logseek_core::ingest::pipeline::start_ingest_many(
        &[a, b],
        &dir,
        Budget::new(256 << 20),
        opts(),
    );
    assert_eq!(wait(&job), JobState::Done);
    let st = Store::open(&dir).unwrap();
    assert_eq!(st.row_count(), 12);
    let file = st.schema().index_of("_file").unwrap();
    let theirs = st.schema().index_of("_file_2").expect("the source's own field");
    for r in 0..6 {
        assert_eq!(st.cell_text(r, file).unwrap().as_deref(), Some("a.csv"));
        assert_eq!(
            st.cell_text(r, theirs).unwrap(),
            Some(format!("claims-{r}.csv"))
        );
        assert_eq!(st.cell_text(6 + r, file).unwrap().as_deref(), Some("b.csv"));
        assert_eq!(st.cell_text(6 + r, theirs).unwrap(), None);
    }
    // one column of each name: no two columns share an id
    let n = st
        .schema()
        .columns
        .iter()
        .filter(|c| c.name == "_file")
        .count();
    assert_eq!(n, 1);
}
