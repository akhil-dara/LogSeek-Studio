//! Free-text search must find every row whose ORIGINAL text contains the term.
use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::exec::{filter, page, ChunkCache};
use logseek_core::query::parse;
use logseek_core::store::Store;
use rand::{Rng, SeedableRng};
use std::io::Write;
use std::sync::atomic::AtomicBool;

fn ingest(src: &std::path::Path) -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("x.lsidx");
    let job = start_ingest(
        src,
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

fn count(st: &Store, q: &str) -> u64 {
    filter(
        st,
        &parse(q).unwrap(),
        &ChunkCache::new(Budget::new(64 << 20)),
        &AtomicBool::new(false),
    )
    .unwrap()
    .count()
}

#[test]
fn original_formats_are_searchable_and_displayed() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("f.csv");
    let mut f = std::fs::File::create(&src).unwrap();
    writeln!(f, "When,Amount,Flag,Note").unwrap();
    for i in 0..500 {
        writeln!(
            f,
            "{:02}/02/2018 08:31:01,{}.50,{},n{}",
            13 + i % 10,
            i,
            if i % 2 == 0 { "True" } else { "False" },
            i
        )
        .unwrap();
    }
    drop(f);
    let (_t, st) = ingest(&src);
    assert_eq!(
        st.schema().columns[0].ty,
        logseek_core::types::ColType::Time
    );
    assert_eq!(count(&st, "\"14/02/2018\""), 50);
    assert_eq!(count(&st, "\"7.50\""), 50); // every i ending in 7: 7.50, 17.50 … 497.50
    assert_eq!(count(&st, "True"), 250);
    let c = ChunkCache::new(Budget::new(64 << 20));
    let m = filter(&st, &parse("Note=n7").unwrap(), &c, &AtomicBool::new(false)).unwrap();
    let p = page(&st, &m, 0, 1, &[0, 1, 2], &c).unwrap();
    assert_eq!(p.cells[0][0].as_deref(), Some("20/02/2018 08:31:01"));
    assert_eq!(p.cells[0][1].as_deref(), Some("7.50"));
    assert_eq!(p.cells[0][2].as_deref(), Some("False"));
    // typed semantics still work
    assert_eq!(count(&st, "Amount>=490"), 10);
    assert_eq!(count(&st, "When>=2018-02-22"), 50);
}

#[test]
fn free_text_finds_every_raw_line_occurrence() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("p.csv");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
    let mut rng = rand::rngs::StdRng::seed_from_u64(9);
    writeln!(f, "Time,Id,Score,Host,Msg").unwrap();
    let words = [
        "alpha",
        "Bravo",
        "charlie-7",
        "DELTA_9",
        "echo.local",
        "fox/trot",
        "golf=1",
        "Hotel 42",
    ];
    let mut lines = Vec::new();
    for i in 0..20_000 {
        let line = format!(
            "2026-11-{:02}T{:02}:{:02}:{:02}Z,{},{}.{:02},ws-{:03},{} {} {}",
            1 + i % 28,
            i % 24,
            i % 60,
            (i * 7) % 60,
            i * 13,
            rng.gen_range(0..900),
            rng.gen_range(0..100),
            rng.gen_range(0..300),
            words[rng.gen_range(0..8)],
            words[rng.gen_range(0..8)],
            rng.gen_range(0..100000)
        );
        writeln!(f, "{line}").unwrap();
        lines.push(line);
    }
    drop(f);
    let (_t, st) = ingest(&src);
    for _ in 0..60 {
        let l = &lines[rng.gen_range(0..lines.len())];
        let a = rng.gen_range(0..l.len() - 6);
        let len = rng.gen_range(3..7);
        let needle = &l[a..a + len];
        if needle.contains(',') || needle.contains('"') || needle.trim() != needle {
            continue;
        }
        let want = lines
            .iter()
            .filter(|x| {
                x.to_ascii_lowercase()
                    .contains(&needle.to_ascii_lowercase())
            })
            .count() as u64;
        let q = format!("\"{}\"", needle.replace('\\', "\\\\"));
        let got = count(&st, &q);
        assert!(
            got >= want,
            "needle {needle:?}: raw lines {want}, search found {got}"
        );
    }
}
