//! Joint outliers: a row whose numbers are each ordinary on their own but
//! unusual together is found, and the numbers that make it so are named.

use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::exec::{page, ChunkCache, FormulaSet, MatchSet};
use logseek_core::query::unusual::unusual;
use logseek_core::store::Store;
use std::fmt::Write;
use std::sync::atomic::{AtomicBool, AtomicU64};

const N: u64 = 20_000;
const ODD: u64 = 13_579;

/// Bearing temperature follows load (about load / 10); one row is hot at low
/// load. Its load (500) is middling, its temperature (85) within the range.
fn make() -> (tempfile::TempDir, Store) {
    let mut t = String::from("Unit,Load,Temp,Fan\n");
    let mut lcg: u64 = 99;
    let mut rnd = || {
        lcg = lcg
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (lcg >> 33) as i64
    };
    for i in 0..N {
        // loads and fan speeds bunch around a middle, as measurements do
        let load = 550 + (0..4).map(|_| rnd() % 225 - 112).sum::<i64>();
        let temp = load / 10 + rnd() % 5 - 2; // follows the load
        let fan = 2000 + (0..4).map(|_| rnd() % 500 - 250).sum::<i64>(); // unrelated to both
        let (load, temp) = if i == ODD { (500, 85) } else { (load, temp) };
        writeln!(t, "U-{},{load},{temp},{fan}", i % 30).unwrap();
    }
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("plant.csv");
    std::fs::write(&src, t).unwrap();
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
fn numbers_ordinary_alone_but_unusual_together_are_found_and_named() {
    let (_d, st) = make();
    let cache = ChunkCache::new(Budget::new(64 << 20));
    let all = MatchSet::all(&st);
    let cols: Vec<usize> = (0..st.schema().len()).collect();
    let u = unusual(
        &st,
        &all,
        &cols,
        None,
        &FormulaSet::default(),
        &cache,
        &AtomicBool::new(false),
        &AtomicU64::new(0),
    )
    .unwrap();
    // every row, to compare
    let rows = page(&st, &all, 0, N as usize, &cols, &cache).unwrap();
    let joint_bits = |cells: &[Option<String>]| {
        u.model
            .why(cells)
            .iter()
            .find(|r| !r.together.is_empty())
            .map_or(0.0, |r| r.bits)
    };
    let odd = joint_bits(&rows.cells[ODD as usize]);
    // no other row's numbers are as unusual together
    for (i, c) in rows.cells.iter().enumerate() {
        if i as u64 != ODD {
            assert!(joint_bits(c) < odd, "row {i}: {} vs {odd}", joint_bits(c));
        }
    }
    // asked for numbers unusual together, it comes first
    let t = logseek_core::query::unusual::unusual_by(
        &st,
        &all,
        &cols,
        None,
        logseek_core::query::unusual::Weigh::Together,
        &FormulaSet::default(),
        &cache,
        &AtomicBool::new(false),
        &AtomicU64::new(0),
    )
    .unwrap();
    assert!(t.rows.contains(&ODD), "{:?}", t.rows);
    let why = u.model.why(&rows.cells[ODD as usize]);
    let names: Vec<String> = st.schema().columns.iter().map(|c| c.name.clone()).collect();
    // the strongest reason: the numbers together, naming load and temperature
    let top = &why[0];
    let together: Vec<&str> = top.together.iter().map(|&c| names[c].as_str()).collect();
    assert!(
        together.contains(&"Load") && together.contains(&"Temp"),
        "{why:?}"
    );
    assert!(!together.contains(&"Fan"), "{together:?}");
    // an ordinary row's numbers together say far less
    assert!(joint_bits(&rows.cells[5]) + 4.0 < top.bits);
    // the same answer every time
    let again = unusual(
        &st,
        &all,
        &cols,
        None,
        &FormulaSet::default(),
        &cache,
        &AtomicBool::new(false),
        &AtomicU64::new(0),
    )
    .unwrap();
    assert_eq!(again.rows, u.rows);
}
