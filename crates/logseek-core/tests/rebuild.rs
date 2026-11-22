//! A segment's columns rebuilt from the source equal what the import stored.

use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::ingest::rebuild::Rebuilder;
use logseek_core::store::{Store, OFFSET_COL};
use std::io::Write;
use std::path::Path;
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

fn import(src: &Path, dir: &Path, chunk: usize) -> Store {
    let o = IngestOptions {
        chunk_bytes: chunk,
        ..Default::default()
    };
    assert_eq!(
        wait(&start_ingest(src, dir, Budget::new(256 << 20), o)),
        JobState::Done
    );
    Store::open(dir).unwrap()
}

fn offsets(st: &Store, si: usize, b: &Budget) -> Vec<i64> {
    let seg = &st.segments()[si];
    let offs = seg.column(OFFSET_COL, b).unwrap().unwrap();
    (0..seg.meta().rows as usize)
        .map(|r| offs.int(r).unwrap())
        .collect()
}

/// Every column of every segment, rebuilt for `rows`, equals the stored rows.
fn same_as_stored(st: &Store, min_segments: usize) {
    let b = Budget::new(256 << 20);
    let rb = Rebuilder::new(st.manifest()).unwrap();
    assert!(
        st.segments().len() >= min_segments,
        "{}",
        st.segments().len()
    );
    for (si, seg) in st.segments().iter().enumerate() {
        let m = seg.meta();
        let offs = offsets(st, si, &b);
        let cols = rb.rows(m, &offs, 0..m.rows as usize).unwrap();
        for c in &m.cols {
            let stored = seg.column(c.col_id, &b).unwrap().unwrap();
            let re = cols
                .iter()
                .find(|e| e.col_id() == c.col_id)
                .unwrap_or_else(|| panic!("segment {si}: column {} not rebuilt", c.col_id));
            let re = re.decode(m.rows, &b).unwrap();
            for r in 0..m.rows as usize {
                assert_eq!(
                    stored.text(r),
                    re.text(r),
                    "segment {si} column {} row {r}",
                    c.col_id
                );
            }
        }
    }
}

fn write_csv(path: &Path, rows: usize) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    writeln!(f, "time,host,status,bytes,message").unwrap();
    for i in 0..rows {
        // quoted commas, doubled quotes and a newline inside a field
        let msg = if i % 97 == 0 {
            format!("\"line one, {i}\nline \"\"two\"\"\"")
        } else {
            format!("\"GET /a?id={i}, ok\"")
        };
        writeln!(
            f,
            "2026-11-01T{:02}:{:02}:{:02}.{:06}Z,web{:02},{},{},{msg}",
            (i / 3600) % 24,
            (i / 60) % 60,
            i % 60,
            i * 7 % 1_000_000,
            i % 13,
            [200, 404, 500][i % 3],
            i * 31 % 9000
        )
        .unwrap();
    }
}

#[test]
fn rebuild_equals_import_csv() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("a.csv");
    write_csv(&src, 3000);
    let st = import(&src, &d.path().join("a.lsidx"), 16 * 1024);
    same_as_stored(&st, 3);
}

#[test]
fn rebuild_equals_import_ndjson() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("a.jsonl");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
    for i in 0..3000 {
        if i % 50 == 0 {
            writeln!(f).unwrap(); // blank lines are skipped, not rows
        }
        writeln!(
            f,
            r#"{{"ts":"2026-11-01T00:00:{:02}Z","user":{{"name":"u{}","id":{}}},"ok":{},"tags":["a","b{}"]{}}}"#,
            i % 60,
            i % 17,
            i,
            i % 2 == 0,
            i % 5,
            if i % 7 == 0 { r#","extra":1.5"# } else { "" }
        )
        .unwrap();
    }
    drop(f);
    let st = import(&src, &d.path().join("a.lsidx"), 16 * 1024);
    same_as_stored(&st, 3);
}

#[test]
fn rebuild_equals_import_json_array() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("a.json");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
    writeln!(f, "[").unwrap();
    for i in 0..3000 {
        let sep = if i + 1 < 3000 { "," } else { "" };
        writeln!(
            f,
            "  {{\"t\": {}, \"msg\": \"m, {}\", \"n\": {{\"k\": [1, {}]}}}}{sep}",
            1_700_000_000 + i,
            i,
            i
        )
        .unwrap();
    }
    writeln!(f, "]").unwrap();
    drop(f);
    let st = import(&src, &d.path().join("a.lsidx"), 16 * 1024);
    same_as_stored(&st, 3);
}

#[test]
fn rebuild_window_equals_slice() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("a.csv");
    write_csv(&src, 20_000);
    let st = import(&src, &d.path().join("a.lsidx"), 4 << 20);
    let b = Budget::new(256 << 20);
    let rb = Rebuilder::new(st.manifest()).unwrap();
    let seg = &st.segments()[0];
    assert!(seg.meta().rows >= 2048, "{}", seg.meta().rows);
    let offs = offsets(&st, 0, &b);
    let cols = rb.rows(seg.meta(), &offs, 1024..2048).unwrap();
    for c in &seg.meta().cols {
        let stored = seg.column(c.col_id, &b).unwrap().unwrap();
        let re = cols.iter().find(|e| e.col_id() == c.col_id).unwrap();
        let re = re.decode(1024, &b).unwrap();
        for k in 0..1024 {
            assert_eq!(
                stored.text(1024 + k),
                re.text(k),
                "column {} row {}",
                c.col_id,
                1024 + k
            );
        }
    }
}

#[test]
fn rebuild_rejects_changed_source() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("evidence.csv");
    write_csv(&src, 3000);
    let st = import(&src, &d.path().join("a.lsidx"), 16 * 1024);
    let b = Budget::new(256 << 20);
    let rb = Rebuilder::new(st.manifest()).unwrap();
    // someone edits the file after it was indexed (same size)
    std::thread::sleep(Duration::from_millis(30));
    let mut bytes = std::fs::read(&src).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] = if bytes[mid] == b'7' { b'8' } else { b'7' };
    std::fs::write(&src, &bytes).unwrap();
    let seg = &st.segments()[1];
    let offs = offsets(&st, 1, &b);
    let err = rb
        .rows(seg.meta(), &offs, 0..seg.meta().rows as usize)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("evidence.csv") && err.contains("changed"),
        "{err}"
    );
    assert!(Rebuilder::new(st.manifest()).is_err());
}

#[test]
fn rebuild_equals_import_json_array_of_arrays() {
    // array-valued elements: each one's closing bracket belongs to it.
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("a.json");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
    writeln!(f, "[").unwrap();
    for i in 0..3000 {
        let sep = if i + 1 < 3000 { "," } else { "" };
        match i % 3 {
            0 => writeln!(f, "  [{i}, \"a, ]{i}\"]{sep}").unwrap(),
            1 => writeln!(f, "  [{i}, [\"b\", [{i}]]]{sep}").unwrap(),
            _ => writeln!(f, "  {{\"k\": [{i}, \"]\"]}}{sep}").unwrap(),
        }
    }
    writeln!(f, "]").unwrap();
    drop(f);
    let st = import(&src, &d.path().join("a.lsidx"), 16 * 1024);
    same_as_stored(&st, 3);
}
