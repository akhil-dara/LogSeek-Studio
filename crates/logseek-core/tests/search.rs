//! Searching for a keyword across the columns of an index.
use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::exec::{ChunkCache, FormulaSet, MatchSet};
use logseek_core::query::search::{cell_spans, search_all, SearchOpts};
use logseek_core::store::Store;
use std::io::Write;
use std::sync::atomic::AtomicBool;

fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("s.csv");
    let mut f = std::fs::File::create(&src).unwrap();
    writeln!(f, "user,cmd,note").unwrap();
    writeln!(f, "root,cat /etc/passwd,ok").unwrap(); // 0
    writeln!(f, "Root,ls,root shell and ROOT again").unwrap(); // 1
    writeln!(f, "alice,grep rootkit /var,fine").unwrap(); // 2
    writeln!(f, "bob,whoami,nothing").unwrap(); // 3
    drop(f);
    let dir = d.path().join("s.lsidx");
    let job = start_ingest(&src, &dir, Budget::new(64 << 20), IngestOptions::default());
    while matches!(job.state(), JobState::Running) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    (d, Store::open(&dir).unwrap())
}

fn run(
    st: &Store,
    text: &str,
    case: bool,
    word: bool,
    col: Option<usize>,
) -> (Vec<u64>, Vec<u64>, u64) {
    let cache = ChunkCache::new(Budget::new(32 << 20));
    let all = MatchSet::all(st);
    let o = SearchOpts {
        text: text.into(),
        case,
        word,
        col,
    };
    let r = search_all(
        st,
        &all,
        &[0, 1, 2],
        &FormulaSet::default(),
        &cache,
        &o,
        &AtomicBool::new(false),
    )
    .unwrap();
    let rows: Vec<u64> = (0..4)
        .filter(|&i| r.rows.restrict(st, &[i]).count() == 1)
        .collect();
    (rows, r.per_col, r.cells)
}

#[test]
fn every_row_with_the_keyword_and_how_many_cells_per_column() {
    let (_d, st) = store();
    let (rows, per_col, cells) = run(&st, "root", false, false, None);
    assert_eq!(rows, [0, 1, 2]);
    // user: root, Root · cmd: rootkit · note: "root shell and ROOT again"
    assert_eq!(per_col, [2, 1, 1]);
    assert_eq!(cells, 4);
}

#[test]
fn match_case_and_whole_word_narrow_it() {
    let (_d, st) = store();
    assert_eq!(run(&st, "root", true, false, None).0, [0, 1, 2]);
    assert_eq!(run(&st, "Root", true, false, None).0, [1]);
    // whole word: "rootkit" is not "root"
    assert_eq!(run(&st, "root", false, true, None).0, [0, 1]);
    // only in one column
    assert_eq!(run(&st, "root", false, false, Some(1)).0, [2]);
    assert!(run(&st, "zebra", false, false, None).0.is_empty());
}

#[test]
fn where_the_keyword_sits_inside_a_cell() {
    let o = |case, word| SearchOpts {
        text: "root".into(),
        case,
        word,
        col: None,
    };
    assert_eq!(
        cell_spans("root shell and ROOT again", &o(false, false)),
        vec![(0, 4), (15, 19)]
    );
    assert_eq!(
        cell_spans("root shell and ROOT again", &o(true, false)),
        vec![(0, 4)]
    );
    assert_eq!(
        cell_spans("grep rootkit root", &o(false, true)),
        vec![(13, 17)]
    );
    assert!(cell_spans("nothing", &o(false, false)).is_empty());
}

#[test]
fn wide_files_are_read_fewer_rows_at_a_time() {
    use logseek_core::query::exec::rows_per_page;
    assert_eq!(rows_per_page(1), 20_000);
    assert_eq!(rows_per_page(20), 20_000);
    assert_eq!(rows_per_page(848), 471);
    // never so few that paging itself becomes the cost
    assert_eq!(rows_per_page(1_000_000), 64);
    assert_eq!(rows_per_page(0), 20_000);
}
