//! Unusual rows: every row scored by how rare its values are in their own
//! columns, read from the file itself; the reasons said for each row.

use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::exec::{page, ChunkCache, FormulaSet, MatchSet};
use logseek_core::query::unusual::unusual;
use logseek_core::store::Store;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64};

const N: u64 = 20_000;
const HUGE: u64 = 7_777;
const STRANGER: u64 = 12_000;
const NO_HOST: u64 = 15_555;

fn make() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("net.csv");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
    writeln!(f, "Seq,Host,User,Bytes,Id,Trace").unwrap();
    for i in 0..N {
        let host = if i == NO_HOST {
            String::new()
        } else {
            format!("WS-{:02}", i % 20)
        };
        let user = if i == STRANGER {
            "mallory".to_string()
        } else {
            ["ann", "bo", "cy", "di"][(i % 4) as usize].to_string()
        };
        let bytes = if i == HUGE {
            94_500_000
        } else {
            1000 + (i * 7919) % 900
        };
        // Seq counts up row by row; Id names every row
        writeln!(
            f,
            "{},{},{},{},id-{:x},{}",
            100_000 + i,
            host,
            user,
            bytes,
            i * 2654435761 % 1_000_003,
            if i % 10 == 0 {
                format!("t-{}", i * 7)
            } else {
                String::new()
            }
        )
        .unwrap();
    }
    drop(f);
    let dir = d.path().join("n.lsidx");
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
fn the_rows_with_rare_values_come_first_and_say_why() {
    let (_d, st) = make();
    let cache = ChunkCache::new(Budget::new(64 << 20));
    let no = AtomicBool::new(false);
    let all = MatchSet::all(&st);
    let cols: Vec<usize> = (0..st.schema().len()).collect();
    let u = unusual(
        &st,
        &all,
        &cols,
        None,
        &FormulaSet::default(),
        &cache,
        &no,
        &AtomicU64::new(0),
    )
    .unwrap();
    // the spread covers every row
    assert_eq!(u.spread.iter().sum::<u64>(), N);
    // the planted rows are among the most unusual
    for r in [HUGE, STRANGER, NO_HOST] {
        assert!(u.rows.contains(&r), "row {r} not in {:?}", u.rows);
    }
    // a column that names every row, and one that counts up row by row,
    // weigh nothing: no row is unusual for them
    let names: Vec<String> = st.schema().columns.iter().map(|c| c.name.clone()).collect();
    let quiet = |n: &str| u.model.weight(names.iter().position(|x| x == n).unwrap());
    assert_eq!(quiet("Id"), 0.0);
    assert_eq!(quiet("Seq"), 0.0);
    // a column filled in few rows, a different value in each: nothing either
    assert_eq!(quiet("Trace"), 0.0);
    assert!(quiet("User") > 0.9 && quiet("Host") > 0.9 && quiet("Bytes") > 0.9);
    // why each planted row is there, in its own values with exact counts
    let why = |row: u64| {
        let p = page(&st, &all, row, 1, &cols, &cache).unwrap();
        assert_eq!(p.rows[0], row);
        u.model.why(&p.cells[0])
    };
    let w = why(STRANGER);
    assert_eq!(names[w[0].col], "User");
    assert_eq!(
        (w[0].value.as_deref(), w[0].rows, w[0].of),
        (Some("mallory"), 1, N)
    );
    let w = why(HUGE);
    assert_eq!(names[w[0].col], "Bytes");
    assert_eq!(w[0].value.as_deref(), Some("94500000"));
    assert!(w[0].largest, "the largest value of its column");
    let w = why(NO_HOST);
    assert_eq!(names[w[0].col], "Host");
    assert_eq!((w[0].value.as_deref(), w[0].rows), (None, 1));
    // reasons never name the quiet columns
    for r in [HUGE, STRANGER, NO_HOST, 3, 9_999] {
        for x in why(r) {
            assert!(names[x.col] != "Id" && names[x.col] != "Seq" && names[x.col] != "Trace");
        }
    }
    // an ordinary row's strongest reason is well below the planted ones'
    let top = |r: u64| why(r).first().map_or(0.0, |x| x.bits);
    for r in [HUGE, STRANGER, NO_HOST] {
        assert!(top(9_999) + 3.0 < top(r), "{} vs {}", top(9_999), top(r));
    }
}

#[test]
fn a_lower_line_takes_in_more_rows_and_it_can_be_stopped() {
    let (_d, st) = make();
    let cache = ChunkCache::new(Budget::new(64 << 20));
    let all = MatchSet::all(&st);
    let cols: Vec<usize> = (0..st.schema().len()).collect();
    let run = |line: Option<f64>, stop: bool| {
        unusual(
            &st,
            &all,
            &cols,
            line,
            &FormulaSet::default(),
            &cache,
            &AtomicBool::new(stop),
            &AtomicU64::new(0),
        )
    };
    let a = run(None, false).unwrap();
    let b = run(Some(a.usual), false).unwrap();
    assert!(b.rows.len() > a.rows.len());
    assert!(run(None, true).is_err());
}
