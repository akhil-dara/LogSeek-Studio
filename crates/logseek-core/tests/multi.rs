//! Many keywords in one pass: which found something, how often, and where.

use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::exec::{ChunkCache, FormulaSet, MatchSet};
use logseek_core::query::multi::{search_list, ListOpts};
use logseek_core::store::Store;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

struct Row {
    host: String,
    user: String,
    cmd: String,
}

fn make(rows: usize) -> (tempfile::TempDir, Store, Vec<Row>) {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("ev.csv");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
    writeln!(f, "Host,User,Cmd").unwrap();
    let mut data = Vec::new();
    for i in 0..rows {
        let r = Row {
            host: format!("WS-{:02}", i % 20),
            user: ["alice", "Bob", "SYSTEM", "carol"][i % 4].to_string(),
            cmd: match i % 500 {
                // only in the very first rows of the file
                _ if i < 40 => format!("early-marker-zq77 step {i}"),
                7 => "powershell -enc AAAA connect 10.9.8.7".to_string(),
                9 => "net use \\\\fs01\\share /user:Bob".to_string(),
                _ => format!("svchost.exe -k netsvcs {}", i % 97),
            },
        };
        writeln!(f, "{},{},\"{}\"", r.host, r.user, r.cmd).unwrap();
        data.push(r);
    }
    drop(f);
    let dir = d.path().join("ev.lsidx");
    let job = start_ingest(
        &src,
        &dir,
        Budget::new(256 << 20),
        IngestOptions {
            chunk_bytes: 128 * 1024,
            ..Default::default()
        },
    );
    while matches!(job.state(), JobState::Running) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(job.state(), JobState::Done);
    (d, Store::open(&dir).unwrap(), data)
}

#[test]
fn every_keyword_is_counted_in_one_pass_with_the_columns_it_was_found_in() {
    let (_d, st, data) = make(30_000);
    let cache = ChunkCache::new(Budget::new(64 << 20));
    let terms: Vec<String> = ["bob", "10.9.8.7", "netsvcs", "not-in-the-file", "", "WS-0"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let done = AtomicU64::new(0);
    let all = MatchSet::all(&st);
    let cols = [0usize, 1, 2];
    let r = search_list(
        &st,
        &all,
        &cols,
        &FormulaSet::default(),
        &cache,
        &terms,
        &ListOpts::default(),
        &AtomicBool::new(false),
        &done,
    )
    .unwrap();
    let n = |f: &dyn Fn(&Row) -> bool| data.iter().filter(|r| f(r)).count() as u64;
    // "bob": the User column, and the Cmd of the "net use" rows (any capitals)
    let bob_user = n(&|r| r.user == "Bob");
    let bob_cmd = n(&|r| r.cmd.contains("Bob"));
    assert_eq!(r.terms[0].cells, bob_user + bob_cmd);
    assert_eq!(
        r.terms[0].rows,
        n(&|r| r.user == "Bob" || r.cmd.contains("Bob"))
    );
    // most cells first
    assert_eq!(r.terms[0].cols, vec![(1, bob_user), (2, bob_cmd)]);
    // an address, a common word, nothing, an empty keyword
    assert_eq!(r.terms[1].rows, n(&|r| r.cmd.contains("10.9.8.7")));
    assert_eq!(r.terms[1].cols, vec![(2, r.terms[1].cells)]);
    assert_eq!(r.terms[2].rows, n(&|r| r.cmd.contains("netsvcs")));
    assert_eq!((r.terms[3].rows, r.terms[3].cells), (0, 0));
    assert_eq!((r.terms[4].rows, r.terms[4].cells), (0, 0));
    assert_eq!(r.terms[5].rows, n(&|r| r.host.starts_with("WS-0")));
    // the rows with any keyword at all
    assert_eq!(
        r.rows.count(),
        n(&|r| r.user == "Bob"
            || r.cmd.contains("Bob")
            || r.cmd.contains("10.9.8.7")
            || r.cmd.contains("netsvcs")
            || r.host.starts_with("WS-0"))
    );
    assert_eq!((r.scanned, done.load(Ordering::Relaxed)), (30_000, 30_000));
}

#[test]
fn capitals_and_whole_words_are_respected_and_rows_without_a_hit_are_left_out() {
    let (_d, st, data) = make(10_000);
    let cache = ChunkCache::new(Budget::new(64 << 20));
    let all = MatchSet::all(&st);
    let run = |terms: &[&str], o: ListOpts| {
        let terms: Vec<String> = terms.iter().map(|s| s.to_string()).collect();
        search_list(
            &st,
            &all,
            &[0, 1, 2],
            &FormulaSet::default(),
            &cache,
            &terms,
            &o,
            &AtomicBool::new(false),
            &AtomicU64::new(0),
        )
        .unwrap()
    };
    let n = |f: &dyn Fn(&Row) -> bool| data.iter().filter(|r| f(r)).count() as u64;
    // capitals matter: "bob" finds nothing, "Bob" does
    let r = run(
        &["bob", "Bob"],
        ListOpts {
            case: true,
            word: false,
        },
    );
    assert_eq!(r.terms[0].rows, 0);
    assert_eq!(
        r.terms[1].rows,
        n(&|r| r.user == "Bob" || r.cmd.contains("Bob"))
    );
    assert_eq!(r.rows.count(), r.terms[1].rows);
    // whole word: "net" is in "net use" but not in "netsvcs"
    let r = run(
        &["net"],
        ListOpts {
            case: false,
            word: true,
        },
    );
    assert_eq!(r.terms[0].rows, n(&|r| r.cmd.starts_with("net use")));
    let r = run(&["net"], ListOpts::default());
    assert_eq!(r.terms[0].rows, n(&|r| r.cmd.contains("net")));
    // a stop asked for is honoured
    let stop = AtomicBool::new(true);
    let e = search_list(
        &st,
        &all,
        &[0, 1, 2],
        &FormulaSet::default(),
        &cache,
        &["x".to_string()],
        &ListOpts::default(),
        &stop,
        &AtomicU64::new(0),
    );
    assert!(e.is_err());
}

#[test]
fn blocks_that_cannot_hold_a_keyword_are_not_read_and_nothing_is_missed() {
    let (_d, st, data) = make(60_000);
    assert!(st.segments().len() > 3, "the test needs several blocks");
    let cache = ChunkCache::new(Budget::new(64 << 20));
    let all = MatchSet::all(&st);
    let run = |terms: &[&str]| {
        let terms: Vec<String> = terms.iter().map(|s| s.to_string()).collect();
        search_list(
            &st,
            &all,
            &[0, 1, 2],
            &FormulaSet::default(),
            &cache,
            &terms,
            &ListOpts::default(),
            &AtomicBool::new(false),
            &AtomicU64::new(0),
        )
        .unwrap()
    };
    let n = |f: &dyn Fn(&Row) -> bool| data.iter().filter(|r| f(r)).count() as u64;
    // a keyword that is only in the first rows: the other blocks are skipped whole
    let r = run(&["early-marker-zq77", "nowhere-at-all-qqq"]);
    assert_eq!(r.terms[0].rows, n(&|r| r.cmd.contains("early-marker-zq77")));
    assert_eq!(r.terms[0].rows, 40);
    assert_eq!(r.terms[1].rows, 0);
    assert_eq!(r.rows.count(), 40);
    assert!(
        r.blocks_skipped > 0 && r.blocks_skipped < r.blocks,
        "{} of {} blocks skipped",
        r.blocks_skipped,
        r.blocks
    );
    // every row is still accounted for
    assert_eq!(r.scanned, 60_000);
    // a keyword nowhere in the file reads nothing at all
    let r = run(&["nowhere-at-all-qqq"]);
    assert_eq!((r.blocks_skipped, r.rows.count()), (r.blocks, 0));
    assert_eq!(r.columns_read, 0);
    // a keyword in one column reads that column only
    let r = run(&["alice"]);
    assert_eq!(r.terms[0].rows, n(&|r| r.user == "alice"));
    assert!(
        r.columns_skipped > 0,
        "the other columns should not be read"
    );
}
