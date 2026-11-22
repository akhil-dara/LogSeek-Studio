//! Columns that point at each other, found from the values alone: a column
//! whose values turn up as another column's values on other rows (a parent
//! id pointing at an id), never from what the columns are called.

use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::exec::ChunkCache;
use logseek_core::query::links::linked_columns;
use logseek_core::store::Store;
use std::fmt::Write;
use std::sync::atomic::AtomicBool;

fn make(text: &str) -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("t.csv");
    std::fs::write(&src, text).unwrap();
    let dir = d.path().join("t.lsidx");
    let job = start_ingest(
        &src,
        &dir,
        Budget::new(64 << 20),
        IngestOptions {
            chunk_bytes: 64 * 1024,
            ..Default::default()
        },
    );
    while matches!(job.state(), JobState::Running) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    (d, Store::open(&dir).unwrap())
}

#[test]
fn a_column_whose_values_are_another_columns_on_other_rows_points_at_it() {
    // names say nothing: "a" holds ids, "b" the id of the row that started it,
    // "c" another pointer back to "a", "d" a copy of "a" on the same row
    let mut t = String::from("a,b,c,d,n,v\n");
    for i in 1000..6000u32 {
        let parent = 1000 + (i - 1000) / 3; // every row started by an earlier one
        let ctx = 1000 + (i - 1000) / 2;
        writeln!(t, "{i},{parent},{ctx},{i},{},{}", i % 7, (i * 37) % 900).unwrap();
    }
    let (_d, st) = make(&t);
    let c = ChunkCache::new(Budget::new(32 << 20));
    let links = linked_columns(&st, &c, &AtomicBool::new(false)).unwrap();
    let at = |n: &str| st.schema().index_of(n).unwrap();
    let best = links.first().expect("a link");
    assert_eq!(best.to, at("a"), "the id column pointed at");
    assert!(
        best.from == at("b") || best.from == at("c"),
        "a pointer column, not a copy"
    );
    // the copy on the same row is no pointer, and small counts link nothing
    assert!(links.iter().all(|l| l.from != at("d") && l.to != at("d")));
    assert!(links.iter().all(|l| l.from != at("n") && l.to != at("n")));
    // both pointers back to "a" are found
    let into_a: Vec<usize> = links
        .iter()
        .filter(|l| l.to == at("a"))
        .map(|l| l.from)
        .collect();
    assert!(
        into_a.contains(&at("b")) && into_a.contains(&at("c")),
        "{into_a:?}"
    );
}

#[test]
fn columns_that_share_no_values_link_nothing() {
    let mut t = String::from("x,y\n");
    for i in 0..3000u32 {
        writeln!(t, "u{i},h{}", i * 7).unwrap();
    }
    let (_d, st) = make(&t);
    let c = ChunkCache::new(Budget::new(32 << 20));
    assert!(linked_columns(&st, &c, &AtomicBool::new(false))
        .unwrap()
        .is_empty());
}

#[test]
fn a_pointer_to_rows_far_away_is_found_though_a_sample_would_miss_them() {
    // each row's parent is any earlier row: in a few thousand sampled rows
    // most parents are not there, over the file every one is
    let mut t = String::from("id,parent,host\n");
    for i in 1..40_000u64 {
        let parent = (i.wrapping_mul(2_654_435_761) >> 7) % i;
        writeln!(t, "{},{},h{}", 100_000 + i, 100_000 + parent, i % 5).unwrap();
    }
    let (_d, st) = make(&t);
    let c = ChunkCache::new(Budget::new(32 << 20));
    let links = linked_columns(&st, &c, &AtomicBool::new(false)).unwrap();
    let at = |n: &str| st.schema().index_of(n).unwrap();
    let best = links.first().expect("a link");
    assert_eq!((best.from, best.to), (at("parent"), at("id")));
    assert!(best.share > 0.95, "{}", best.share);
}

#[test]
fn the_same_field_under_two_names_on_different_rows_is_no_pointer() {
    // one kind of row writes "size", another "bytes": the same values, never
    // on the same row; "parent" sits beside "id" on every row
    let mut t = String::from("id,parent,size,bytes\n");
    for i in 1..6000u32 {
        let v = 500 + (i / 2) % 300;
        let (size, bytes) = if i % 2 == 0 {
            (v.to_string(), String::new())
        } else {
            (String::new(), v.to_string())
        };
        writeln!(t, "{},{},{size},{bytes}", 10_000 + i, 10_000 + i / 2).unwrap();
    }
    let (_d, st) = make(&t);
    let c = ChunkCache::new(Budget::new(32 << 20));
    let links = linked_columns(&st, &c, &AtomicBool::new(false)).unwrap();
    let at = |n: &str| st.schema().index_of(n).unwrap();
    assert!(
        links
            .iter()
            .all(|l| ![at("size"), at("bytes")].contains(&l.from)),
        "{links:?}"
    );
    assert_eq!((links[0].from, links[0].to), (at("parent"), at("id")));
}
