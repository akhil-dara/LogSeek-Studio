//! Every shape a column holds, with exact counts, read from the values.

use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::exec::{filter, ChunkCache, FormulaSet, MatchSet};
use logseek_core::query::parse;
use logseek_core::query::shape::{column_shapes, shape_term, shape_words, Shape};
use logseek_core::store::Store;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64};

fn make() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("people.csv");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
    writeln!(f, "Postal,Ip,Code").unwrap();
    for i in 0..20_000u32 {
        let postal = match i {
            // a shifted row: an address where a postal code should be
            1234 | 15_000 => "ann.lee@example.org".to_string(),
            _ if i % 500 == 0 => String::new(),
            _ => format!("{:06}", 100_000 + i % 899_999),
        };
        let ip = format!("10.{}.{}.{}", i % 7, i % 250, i % 13);
        let code = if i % 2 == 0 {
            format!("AB-{:04}", i)
        } else {
            format!("x{}y", i % 9)
        };
        writeln!(f, "{postal},{ip},{code}").unwrap();
    }
    drop(f);
    let dir = d.path().join("p.lsidx");
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
fn every_shape_of_a_column_is_counted_and_the_odd_ones_stand_out() {
    let (_d, st) = make();
    let cache = ChunkCache::new(Budget::new(64 << 20));
    let no = AtomicBool::new(false);
    let all = MatchSet::all(&st);
    let col = |n: &str| st.schema().index_of(n).unwrap();
    let shapes = |c: &str, how: Shape| {
        column_shapes(
            &st,
            &all,
            col(c),
            how,
            &FormulaSet::default(),
            &cache,
            &no,
            &AtomicU64::new(0),
        )
        .unwrap()
    };
    // postal codes: digits nearly everywhere, two addresses, some empty
    let (s, empty) = shapes("Postal", Shape::AnyLength);
    // every 500th row is empty (40), but row 15,000 holds an address instead
    assert_eq!(empty, 39);
    let words: Vec<(String, u64)> = s
        .iter()
        .map(|x| (shape_words(&x.pattern).unwrap(), x.rows))
        .collect();
    assert_eq!(
        words,
        vec![
            ("digits".to_string(), 20_000 - 39 - 2),
            (
                "letters, \".\", letters, \"@\", letters, \".\", letters".to_string(),
                2
            ),
        ]
    );
    assert_eq!(s[1].example, "ann.lee@example.org");
    // the search a shape becomes keeps exactly its rows
    for x in &s {
        let q = shape_term("Postal", &x.pattern);
        let n = filter(&st, &parse(&q).unwrap(), &cache, &no)
            .unwrap()
            .count();
        assert_eq!(n, x.rows, "{q}");
    }
    // addresses: one shape, however long each piece
    let (s, empty) = shapes("Ip", Shape::AnyLength);
    assert_eq!((s.len(), empty), (1, 0));
    assert_eq!(
        shape_words(&s[0].pattern).unwrap(),
        "digits, \".\", digits, \".\", digits, \".\", digits"
    );
    // the same lengths: as many shapes as there are length mixes
    let (s, _) = shapes("Ip", Shape::SameLength);
    assert!(s.len() > 1);
    assert_eq!(s.iter().map(|x| x.rows).sum::<u64>(), 20_000);
    // letters and digits alike: "x3y" and "AB-0002" are two shapes, not more
    let (s, _) = shapes("Code", Shape::Alike);
    assert_eq!(s.len(), 2);
    assert_eq!((s[0].rows, s[1].rows), (10_000, 10_000));
}

#[test]
fn shapes_can_be_read_for_some_rows_only_and_stopped() {
    let (_d, st) = make();
    let cache = ChunkCache::new(Budget::new(64 << 20));
    let no = AtomicBool::new(false);
    let some = filter(&st, &parse("Code:AB").unwrap(), &cache, &no).unwrap();
    let (s, _) = column_shapes(
        &st,
        &some,
        st.schema().index_of("Code").unwrap(),
        Shape::AnyLength,
        &FormulaSet::default(),
        &cache,
        &no,
        &AtomicU64::new(0),
    )
    .unwrap();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].rows, 10_000);
    let r = column_shapes(
        &st,
        &some,
        0,
        Shape::AnyLength,
        &FormulaSet::default(),
        &cache,
        &AtomicBool::new(true),
        &AtomicU64::new(0),
    );
    assert!(r.is_err());
}
