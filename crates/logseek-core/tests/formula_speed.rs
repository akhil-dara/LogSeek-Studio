//! Made columns over columns with few values are worked out once per value,
//! not once per row, and whole-block results are kept for the next question.

use logseek_core::budget::Budget;
use logseek_core::formula::{compile, FValue};
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::exec::{filter_with, page_with, ChunkCache, FormulaSet, MatchSet};
use logseek_core::query::parse;
use logseek_core::store::Store;
use logseek_core::types::ColType;
use std::sync::atomic::AtomicBool;

fn store_of(
    lines: impl Iterator<Item = String>,
    header: &str,
    chunk: usize,
) -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("t.csv");
    let mut text = format!("{header}\n");
    for l in lines {
        text.push_str(&l);
        text.push('\n');
    }
    std::fs::write(&src, text).unwrap();
    let dir = d.path().join("t.lsidx");
    let job = start_ingest(
        &src,
        &dir,
        Budget::new(256 << 20),
        IngestOptions {
            chunk_bytes: chunk,
            ..Default::default()
        },
    );
    while matches!(job.state(), JobState::Running) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(job.state(), JobState::Done);
    (d, Store::open(&dir).unwrap())
}

/// Every row's text in requested column `col`, in file order.
fn column_texts(st: &Store, fs: &FormulaSet, col: usize, c: &ChunkCache) -> Vec<Option<String>> {
    let all = MatchSet::all(st);
    let p = page_with(st, &all, 0, st.row_count() as usize, &[col], fs, c).unwrap();
    p.cells
        .into_iter()
        .map(|r| r.into_iter().next().flatten())
        .collect()
}

/// The formula worked out by hand-feeding one row's inputs, as a brute force.
fn by_hand(src: &str, names: &[&str], values: &[&str]) -> Option<String> {
    let cols: Vec<(String, ColType)> = names
        .iter()
        .map(|n| (n.to_string(), ColType::Text))
        .collect();
    let f = compile(src, &cols).unwrap();
    f.eval(&|i| FValue::Text(values[i].to_string())).text()
}

const DEEP: &str = r#"upper(trim(replace(lower(concat(" ", host, "-x ")), "-x", ""))) & "!""#;

fn hosts() -> (tempfile::TempDir, Store) {
    store_of(
        (0..60_000).map(|i| format!("{i},web-{}", i % 13)),
        "n,host",
        96 * 1024,
    )
}

#[test]
fn dict_inputs_evaluate_per_distinct_value() {
    let (_d, st) = hosts();
    let segs = st.segments().len() as u64;
    assert!(segs >= 3, "want several blocks, got {segs}");
    let mut fs = FormulaSet::default();
    fs.add(&st, "deep", DEEP).unwrap();
    let c = ChunkCache::new(Budget::new(64 << 20));
    let no = AtomicBool::new(false);
    let m = filter_with(&st, &parse("deep=WEB-3!").unwrap(), &fs, &c, &no).unwrap();
    assert_eq!(
        m.count(),
        (0..60_000).filter(|i| i % 13 == 3).count() as u64
    );
    let evals = fs.items[0].1.evals();
    assert!(evals <= 14 * segs, "{evals} evaluations over {segs} blocks");
    // every row equals the formula worked out by hand for its host
    let ns = st.schema().len();
    let got = column_texts(&st, &fs, ns, &c);
    for (i, v) in got.iter().enumerate() {
        let host = format!("web-{}", i % 13);
        assert_eq!(v, &by_hand(DEEP, &["host"], &[&host]), "row {i}");
    }
}

#[test]
fn second_filter_reuses_results() {
    let (_d, st) = hosts();
    let mut fs = FormulaSet::default();
    fs.add(&st, "deep", DEEP).unwrap();
    let c = ChunkCache::new(Budget::new(64 << 20));
    let no = AtomicBool::new(false);
    let a = filter_with(&st, &parse("deep=WEB-5!").unwrap(), &fs, &c, &no).unwrap();
    let after_first = fs.items[0].1.evals();
    let b = filter_with(&st, &parse("deep=WEB-7!").unwrap(), &fs, &c, &no).unwrap();
    assert_eq!(
        fs.items[0].1.evals(),
        after_first,
        "the second filter worked it out again"
    );
    assert_eq!(a.count(), b.count());
    // a page of rows from a block already worked out uses the kept results
    let ns = st.schema().len();
    let p = page_with(&st, &b, 0, 5, &[ns], &fs, &c).unwrap();
    assert_eq!(p.cells[0][0].as_deref(), Some("WEB-7!"));
    assert_eq!(fs.items[0].1.evals(), after_first);
}

#[test]
fn many_combinations_fall_back() {
    // the worst case a block can hold (at most 65,536 rows): two columns of 256
    // values each, every pairing once, so no two rows share their inputs
    let (_d, st) = store_of(
        (0..65_536).map(|i| format!("a{},b{}", i % 256, i / 256)),
        "a,b",
        64 << 20,
    );
    assert_eq!(st.segments().len(), 1);
    let src = r#"a & "_" & upper(b)"#;
    let mut fs = FormulaSet::default();
    fs.add(&st, "ab", src).unwrap();
    let c = ChunkCache::new(Budget::new(128 << 20));
    let no = AtomicBool::new(false);
    let m = filter_with(&st, &parse("ab=a7_B9").unwrap(), &fs, &c, &no).unwrap();
    assert_eq!(m.count(), 1);
    let got = column_texts(&st, &fs, st.schema().len(), &c);
    for (i, v) in got.iter().enumerate() {
        let (a, b) = (format!("a{}", i % 256), format!("b{}", i / 256));
        assert_eq!(v, &by_hand(src, &["a", "b"], &[&a, &b]), "row {i}");
    }
}

#[test]
fn mixed_encodings_agree() {
    // the first half has five hosts (kept as a dictionary), the second a new host per row
    let (_d, st) = store_of(
        (0..40_000).map(|i| {
            if i < 20_000 {
                format!("{i},h-{}", i % 5)
            } else {
                format!("{i},unique-{i}")
            }
        }),
        "n,host",
        64 * 1024,
    );
    assert!(st.segments().len() >= 4);
    let src = r#"upper(host) & "/" & len(host)"#;
    let mut fs = FormulaSet::default();
    fs.add(&st, "u", src).unwrap();
    let c = ChunkCache::new(Budget::new(64 << 20));
    let no = AtomicBool::new(false);
    let m = filter_with(&st, &parse("u=H-2/3").unwrap(), &fs, &c, &no).unwrap();
    assert_eq!(m.count(), 4_000);
    let got = column_texts(&st, &fs, st.schema().len(), &c);
    for (i, v) in got.iter().enumerate() {
        let host = if i < 20_000 {
            format!("h-{}", i % 5)
        } else {
            format!("unique-{i}")
        };
        assert_eq!(v, &by_hand(src, &["host"], &[&host]), "row {i}");
    }
}
