//! Patterns in a column: lines of free text folded into patterns, the parts
//! that vary marked, with exact counts; the rare ones stand out.

use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::exec::{ChunkCache, FormulaSet, MatchSet};
use logseek_core::query::patterns::{pattern_rows, pattern_words, patterns, PatternOpts};
use logseek_core::store::Store;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64};

fn make() -> (tempfile::TempDir, Store, Vec<String>) {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("app.csv");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
    writeln!(f, "Line").unwrap();
    let mut lines = Vec::new();
    for i in 0..30_000u64 {
        let l = match i {
            17_777 => "FATAL WAL segment 000A checksum mismatch on block 9".to_string(),
            _ if i % 1000 == 3 => String::new(),
            _ => match i % 4 {
                0 => format!(
                    "Connection to 10.0.{}.{} timed out after {} ms",
                    i % 7,
                    i % 200,
                    100 + i % 900
                ),
                1 => format!(
                    "User {} logged in from host-{}",
                    ["ann", "bo", "cy", "di"][(i / 4 % 4) as usize],
                    i % 31
                ),
                2 => format!("GET /api/v1/orders/{} 200", i),
                _ => format!("Job {} finished in {}s", i, i % 60),
            },
        };
        writeln!(f, "\"{l}\"").unwrap();
        lines.push(l);
    }
    drop(f);
    let dir = d.path().join("a.lsidx");
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
    (d, Store::open(&dir).unwrap(), lines)
}

#[test]
fn lines_fold_into_patterns_with_exact_counts_and_the_rare_one_stands_out() {
    let (_d, st, lines) = make();
    let cache = ChunkCache::new(Budget::new(64 << 20));
    let no = AtomicBool::new(false);
    let all = MatchSet::all(&st);
    let p = patterns(
        &st,
        &all,
        0,
        &PatternOpts::default(),
        &FormulaSet::default(),
        &cache,
        &no,
        &AtomicU64::new(0),
    )
    .unwrap();
    let words: Vec<(String, u64)> = p
        .patterns
        .iter()
        .map(|x| (pattern_words(x), x.rows))
        .collect();
    let empty = lines.iter().filter(|l| l.is_empty()).count() as u64;
    assert_eq!(p.empty, empty);
    // every non-empty row is in exactly one pattern
    assert_eq!(
        p.patterns.iter().map(|x| x.rows).sum::<u64>() + p.empty,
        30_000
    );
    let has = |w: &str| {
        words
            .iter()
            .find(|(x, _)| x == w)
            .unwrap_or_else(|| panic!("{w} not in {words:#?}"))
            .1
    };
    let count = |f: &dyn Fn(&str) -> bool| lines.iter().filter(|l| f(l)).count() as u64;
    assert_eq!(
        has("Connection to [varies] timed out after [varies] ms"),
        count(&|l| l.starts_with("Connection"))
    );
    assert_eq!(
        has("User [varies] logged in from [varies]"),
        count(&|l| l.starts_with("User"))
    );
    assert_eq!(has("GET [varies] 200"), count(&|l| l.starts_with("GET")));
    assert_eq!(
        has("Job [varies] finished in [varies]"),
        count(&|l| l.starts_with("Job"))
    );
    // the one odd line is a pattern of its own, with one row (seen once,
    // nothing in it varies)
    assert_eq!(
        has("FATAL WAL segment 000A checksum mismatch on block 9"),
        1
    );
    assert_eq!(p.patterns.len(), 5);
    // most rows first
    for w in p.patterns.windows(2) {
        assert!(w[0].rows >= w[1].rows);
    }
    // a pattern's rows are exactly the rows counted for it
    let fatal = p.patterns.iter().position(|x| x.rows == 1).unwrap();
    let ids = pattern_rows(&st, &all, 0, &p, fatal, &FormulaSet::default(), &cache, &no).unwrap();
    assert_eq!(ids, vec![17_777]);
    let user = p
        .patterns
        .iter()
        .position(|x| pattern_words(x).starts_with("User"))
        .unwrap();
    let ids = pattern_rows(&st, &all, 0, &p, user, &FormulaSet::default(), &cache, &no).unwrap();
    assert_eq!(ids.len() as u64, p.patterns[user].rows);
    assert!(ids.iter().all(|&i| lines[i as usize].starts_with("User")));
    // the same answer every time (blocks are worked on side by side)
    let again = patterns(
        &st,
        &all,
        0,
        &PatternOpts::default(),
        &FormulaSet::default(),
        &cache,
        &no,
        &AtomicU64::new(0),
    )
    .unwrap();
    assert_eq!(
        again.patterns.iter().map(pattern_words).collect::<Vec<_>>(),
        p.patterns.iter().map(pattern_words).collect::<Vec<_>>()
    );
}

#[test]
fn stricter_likeness_keeps_lines_apart() {
    let (_d, st, _) = make();
    let cache = ChunkCache::new(Budget::new(64 << 20));
    let no = AtomicBool::new(false);
    let all = MatchSet::all(&st);
    let run = |alike: f64| {
        patterns(
            &st,
            &all,
            0,
            &PatternOpts { alike },
            &FormulaSet::default(),
            &cache,
            &no,
            &AtomicU64::new(0),
        )
        .unwrap()
    };
    // every word must match: the user names are no longer folded together
    let strict = run(1.0);
    assert!(strict.patterns.len() > run(0.5).patterns.len());
    assert!(strict
        .patterns
        .iter()
        .any(|x| pattern_words(x).starts_with("User ann logged in")));
}

#[test]
fn numbers_that_differ_vary_and_numbers_that_agree_stay_within_one_block() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("small.csv");
    let mut f = std::fs::File::create(&src).unwrap();
    writeln!(f, "Line").unwrap();
    for i in 0..40 {
        writeln!(f, "\"GET /item/{} 200\"", i).unwrap();
        writeln!(f, "\"Job {} done\"", i).unwrap();
    }
    drop(f);
    let dir = d.path().join("s.lsidx");
    let job = start_ingest(&src, &dir, Budget::new(256 << 20), IngestOptions::default());
    while matches!(job.state(), JobState::Running) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let st = Store::open(&dir).unwrap();
    assert_eq!(st.segments().len(), 1, "one block");
    let cache = ChunkCache::new(Budget::new(64 << 20));
    let p = patterns(
        &st,
        &MatchSet::all(&st),
        0,
        &PatternOpts::default(),
        &FormulaSet::default(),
        &cache,
        &AtomicBool::new(false),
        &AtomicU64::new(0),
    )
    .unwrap();
    let mut words: Vec<(String, u64)> = p
        .patterns
        .iter()
        .map(|x| (pattern_words(x), x.rows))
        .collect();
    words.sort();
    assert_eq!(
        words,
        vec![
            ("GET [varies] 200".to_string(), 40),
            ("Job [varies] done".to_string(), 40)
        ]
    );
}
