//! Rare pairs of two columns: a value of B rarely seen with a common value
//! of A, and values of B seen with the fewest values of A; exact counts.

use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::exec::{ChunkCache, FormulaSet, MatchSet};
use logseek_core::query::pairs::rare_pairs;
use logseek_core::store::Store;
use std::fmt::Write;
use std::sync::atomic::{AtomicBool, AtomicU64};

fn ingest(name: &str, text: &str) -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join(name);
    std::fs::write(&src, text).unwrap();
    let dir = d.path().join("x.lsidx");
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

fn run(st: &Store, a: &str, b: &str) -> logseek_core::query::pairs::Pairs {
    let cache = ChunkCache::new(Budget::new(64 << 20));
    rare_pairs(
        st,
        &MatchSet::all(st),
        st.schema().index_of(a).unwrap(),
        st.schema().index_of(b).unwrap(),
        &FormulaSet::default(),
        &cache,
        &AtomicBool::new(false),
        &AtomicU64::new(0),
    )
    .unwrap()
}

#[test]
fn a_value_rarely_seen_with_a_common_one_comes_first() {
    let mut t = String::from("Vendor,Account,Amount\n");
    for i in 0..12_000u32 {
        let (v, acc) = match i {
            7_000 => ("Acme", "ACC-9"),
            _ if i % 2 == 0 && i < 10_000 => ("Acme", "ACC-1"),
            _ if i % 2 == 1 && i < 6_000 => ("Zed", "ACC-9"),
            _ => (["Bolt", "Cove", "Dune"][(i % 3) as usize], "ACC-5"),
        };
        writeln!(t, "{v},{acc},{}", 100 + i % 50).unwrap();
    }
    // rows with no vendor, each with an account of its own; and a vendor that
    // pays a different account every time (no habit to break)
    for i in 0..3_000u32 {
        writeln!(t, ",ACC-X{i},1").unwrap();
        writeln!(t, "Scatter,ACC-S{i},1").unwrap();
    }
    let (_d, st) = ingest("ap.csv", &t);
    let p = run(&st, "Vendor", "Account");
    let first = &p.unusual[0];
    assert_eq!(
        (
            first.a.as_deref(),
            first.b.as_deref(),
            first.rows,
            first.a_rows
        ),
        (Some("Acme"), Some("ACC-9"), 1, 5_000)
    );
    // an empty first column is not something with a habit: left out, and a
    // value only ever seen where it is empty has no peers to count
    assert!(p.unusual.iter().all(|l| l.a.is_some()));
    assert!(p.peers.iter().all(|l| l.peers > 0), "{:?}", &p.peers[..3]);
    // every other pair is there once, with exact counts
    assert_eq!(
        p.unusual.iter().map(|l| l.rows).sum::<u64>(),
        12_000 + 3_000
    );
    // a value with a different partner every time breaks no habit: it comes
    // below the vendor that broke its one habit
    let scatter = p
        .unusual
        .iter()
        .position(|l| l.a.as_deref() == Some("Scatter"))
        .unwrap();
    assert!(scatter > 0);
    // strongest first
    for w in p.unusual.windows(2) {
        assert!(w[0].surprise >= w[1].surprise - 1e-12, "{w:?}");
    }
}

#[test]
fn a_value_seen_with_one_peer_comes_first_however_busy_it_is() {
    let mut t = String::from("ClientIp,Path\n");
    let mut n = 0u32;
    // ordinary paths, each visited by hundreds of clients
    for i in 0..30_000u32 {
        let ip = format!("10.1.{}.{}", i % 20, (i / 20) % 25);
        let path = ["/", "/login", "/cart", "/search"][(i % 4) as usize];
        writeln!(t, "{ip},{path}").unwrap();
        n += 1;
    }
    // one client, many times, a path nobody else asks for
    for _ in 0..2_000 {
        writeln!(t, "10.0.0.7,/admin/export.sql").unwrap();
        n += 1;
    }
    let (_d, st) = ingest("web.csv", &t);
    let p = run(&st, "ClientIp", "Path");
    assert_eq!(p.total, n as u64);
    let first = &p.peers[0];
    assert_eq!(
        (first.b.as_deref(), first.peers, first.rows),
        (Some("/admin/export.sql"), 1, 2_000)
    );
    assert_eq!(p.a_values, 501);
    // fewest peers first, then the most rows
    for w in p.peers.windows(2) {
        assert!(w[0].peers < w[1].peers || (w[0].peers == w[1].peers && w[0].rows >= w[1].rows));
    }
    assert_eq!(p.peers.len(), 5);
}
