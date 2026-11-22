//! Query execution: filters, time ranges and paging over an index.
use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::exec::{filter, page, ChunkCache};
use logseek_core::query::parse;
use logseek_core::store::Store;
use rand::{Rng, SeedableRng};
use std::io::Write;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

struct Row {
    t: String,
    ev: i64,
    host: String,
    user: String,
    cmd: String,
}

fn make(rows: usize) -> (tempfile::TempDir, Store, Vec<Row>) {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("ev.csv");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
    writeln!(f, "Time,EventID,Host,User,Cmd").unwrap();
    let mut data = Vec::new();
    let mut rng = rand::rngs::StdRng::seed_from_u64(42);
    for i in 0..rows {
        // EventID clustered by position so zone maps can prune
        let ev = if i < rows / 2 { 4624 } else { 4688 };
        let r = Row {
            t: format!(
                "2026-11-{:02} {:02}:{:02}:00",
                1 + i * 10 / rows,
                (i / 60) % 24,
                i % 60
            ),
            ev: ev + (i % 2) as i64,
            host: format!("WS-{:02}", rng.gen_range(0..20)),
            user: ["alice", "Bob", "SYSTEM", "carol"][rng.gen_range(0..4)].to_string(),
            cmd: if i == rows - 7 {
                "C:\\tools\\MimiKatz.exe privilege::debug".to_string()
            } else {
                format!("svchost.exe -k netsvcs {}", rng.gen_range(0..1000))
            },
        };
        writeln!(f, "{},{},{},{},\"{}\"", r.t, r.ev, r.host, r.user, r.cmd).unwrap();
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
    let st = Store::open(&dir).unwrap();
    (d, st, data)
}

fn cache() -> ChunkCache {
    ChunkCache::new(Budget::new(64 << 20))
}

#[test]
fn eq_filter_on_clustered_column_prunes_segments() {
    let (_d, st, data) = make(120_000);
    let c = cache();
    let m = filter(
        &st,
        &parse("EventID=4688").unwrap(),
        &c,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        m.count(),
        data.iter().filter(|r| r.ev == 4688).count() as u64
    );
    assert!(m.stats.segments_pruned > 0, "{:?}", m.stats);
}

#[test]
fn time_range_prunes_by_zone_map() {
    let (_d, st, data) = make(120_000);
    let m = filter(
        &st,
        &parse("@2026-11-03..2026-11-04").unwrap(),
        &cache(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let want = data
        .iter()
        .filter(|r| r.t.starts_with("2026-11-03"))
        .count() as u64;
    assert_eq!(m.count(), want);
    assert!(m.stats.segments_pruned > 0);
}

#[test]
fn substring_uses_bloom_and_is_case_insensitive() {
    let (_d, st, data) = make(120_000);
    let m = filter(
        &st,
        &parse("mimikatz").unwrap(),
        &cache(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(m.count(), 1);
    assert!(m.stats.segments_pruned + m.stats.columns_skipped > 0);
    let p = page(&st, &m, 0, 10, &[4], &cache()).unwrap();
    assert_eq!(p.rows, vec![(data.len() - 7) as u64]);
    assert!(p.cells[0][0].as_deref().unwrap().contains("MimiKatz"));
}

#[test]
fn page_returns_requested_columns_in_order() {
    let (_d, st, data) = make(30_000);
    let c = cache();
    let m = filter(
        &st,
        &parse("User=bob").unwrap(),
        &c,
        &AtomicBool::new(false),
    )
    .unwrap();
    let bobs: Vec<usize> = data
        .iter()
        .enumerate()
        .filter(|(_, r)| r.user == "Bob")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(m.count(), bobs.len() as u64);
    let p = page(&st, &m, 100, 5, &[2, 3], &c).unwrap();
    for (k, row) in p.rows.iter().enumerate() {
        let i = bobs[100 + k];
        assert_eq!(*row, i as u64);
        assert_eq!(p.cells[k][0].as_deref(), Some(data[i].host.as_str()));
        assert_eq!(p.cells[k][1].as_deref(), Some("Bob"));
    }
}

fn brute(r: &Row, q: &str) -> bool {
    match q {
        "EventID>4625" => r.ev > 4625,
        "Host=WS-03 OR Host=ws-07" => r.host == "WS-03" || r.host == "WS-07",
        "User!=SYSTEM svchost" => r.user != "SYSTEM" && r.cmd.to_lowercase().contains("svchost"),
        "NOT (User=alice OR User=carol)" => !(r.user == "alice" || r.user == "carol"),
        "Cmd:netsvcs*" => false,
        r#"Cmd:"*netsvcs 99*""# => r.cmd.to_lowercase().contains("netsvcs 99"),
        "Host~/WS-1[0-4]/" => r.host >= "WS-10".to_string() && r.host <= "WS-14".to_string(),
        "EventID<=4624 Host:ws-1" => r.ev <= 4624 && r.host.to_lowercase().contains("ws-1"),
        _ => unreachable!(),
    }
}

#[test]
fn results_equal_bruteforce() {
    let (_d, st, data) = make(50_000);
    let c = cache();
    for q in [
        "EventID>4625",
        "Host=WS-03 OR Host=ws-07",
        "User!=SYSTEM svchost",
        "NOT (User=alice OR User=carol)",
        "Cmd:netsvcs*",
        r#"Cmd:"*netsvcs 99*""#,
        "Host~/WS-1[0-4]/",
        "EventID<=4624 Host:ws-1",
    ] {
        let m = filter(&st, &parse(q).unwrap(), &c, &AtomicBool::new(false))
            .unwrap_or_else(|e| panic!("{q}: {e}"));
        let want = data.iter().filter(|r| brute(r, q)).count() as u64;
        assert_eq!(m.count(), want, "{q}");
    }
}

#[test]
fn unknown_column_is_a_clear_error() {
    let (_d, st, _) = make(1_000);
    let e = filter(
        &st,
        &parse("Usr=bob").unwrap(),
        &cache(),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    let msg = e.to_string();
    assert!(msg.contains("Usr") && msg.contains("User"), "{msg}");
}

#[test]
fn cancel_stops_quickly() {
    let (_d, st, _) = make(200_000);
    let cancel = Arc::new(AtomicBool::new(true));
    let t = std::time::Instant::now();
    let r = filter(&st, &parse("svchost").unwrap(), &cache(), &cancel);
    assert!(r.is_err());
    assert!(t.elapsed().as_millis() < 100);
}

#[test]
fn cache_stays_within_budget() {
    let (_d, st, _) = make(120_000);
    let c = ChunkCache::new(Budget::new(2 << 20));
    let m = filter(&st, &parse("svchost").unwrap(), &c, &AtomicBool::new(false)).unwrap();
    assert!(m.count() > 0);
    assert!(c.budget().peak() <= c.budget().cap());
}

#[test]
fn a_result_can_be_restricted_to_tagged_rows() {
    // Tags are row ids kept by the app; "show tagged rows" = query result ∩ tagged.
    let (_d, st, data) = make(50_000);
    let c = cache();
    let q = filter(
        &st,
        &parse("EventID=4688").unwrap(),
        &c,
        &AtomicBool::new(false),
    )
    .unwrap();
    let tagged: Vec<u64> = vec![
        3, 10, 24_999, 25_000, 25_001, 30_000, 49_999, 60_000, /* out of range */
    ];
    let r = q.restrict(&st, &tagged);
    let want: Vec<u64> = tagged
        .iter()
        .copied()
        .filter(|&i| (i as usize) < data.len() && data[i as usize].ev == 4688)
        .collect();
    assert_eq!(r.count(), want.len() as u64);
    let p = page(&st, &r, 0, 100, &[1], &c).unwrap();
    assert_eq!(p.rows, want);
    // the whole source restricted to the tags = every tagged row in range
    let all = logseek_core::query::exec::MatchSet::all(&st).restrict(&st, &tagged);
    assert_eq!(all.count(), 7);
}

#[test]
fn the_cache_grows_when_it_keeps_re_reading_and_gives_it_back_when_idle() {
    let (_d, st, _) = make(60_000);
    let pairs: Vec<(usize, u32)> = (0..st.segments().len())
        .flat_map(|s| (0..5u32).map(move |c| (s, c)))
        .collect();
    assert!(pairs.len() >= 40, "{} columns to read", pairs.len());
    let read_all = |c: &ChunkCache| {
        for &(s, col) in &pairs {
            c.get(&st, s, col).unwrap();
        }
    };
    // how much everything takes when it all fits
    let big = ChunkCache::new(Budget::new(1 << 30));
    read_all(&big);
    let all = big.budget().used();
    assert!(all > 0);
    // a cache that starts with room for a quarter of it
    let c = ChunkCache::new(Budget::elastic(all / 4, all * 2));
    let base = c.budget().base();
    // reading everything once, end to end, is a scan: nothing is asked twice
    read_all(&c);
    assert_eq!(c.budget().cap(), base, "a scan must not grow the cache");
    // going over the same columns again and again is pressure: it grows
    for _ in 0..12 {
        read_all(&c);
    }
    let grown = c.budget().cap();
    assert!(grown > base, "cap stayed at {base}");
    assert!(grown <= c.budget().ceiling());
    // still in use: nothing is given back
    assert!(!c.relax(std::time::Duration::from_secs(3600)));
    assert_eq!(c.budget().cap(), grown);
    // only a few columns stay in use; the rest go stale
    let keep = std::time::Duration::from_millis(300);
    c.relax(keep); // marks the clock
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let few = &pairs[..3];
    for &(s, col) in few {
        c.get(&st, s, col).unwrap();
    }
    let first = c.relax(keep);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    for &(s, col) in few {
        c.get(&st, s, col).unwrap();
    }
    let second = c.relax(keep);
    assert!(first || second, "stale columns should have been dropped");
    let after = c.budget().cap();
    assert!(after < grown, "limit stayed at {grown}");
    assert!(after >= base && c.budget().used() <= after);
    // nothing read at all for a while: back to where it started
    std::thread::sleep(std::time::Duration::from_millis(1500));
    c.relax(keep);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    c.relax(keep);
    assert_eq!(c.budget().cap(), base);
    assert!(c.budget().used() <= base);
}

#[test]
fn a_pattern_can_be_looked_for_in_every_field() {
    let (_d, st, data) = make(20_000);
    let c = cache();
    let count = |q: &str| {
        filter(&st, &parse(q).unwrap(), &c, &AtomicBool::new(false))
            .unwrap()
            .count()
    };
    let n = |f: &dyn Fn(&Row) -> bool| data.iter().filter(|r| f(r)).count() as u64;
    // capitals matter unless the pattern says otherwise
    assert_eq!(count("~/bob/"), 0);
    assert_eq!(count("~/Bob/"), n(&|r| r.user == "Bob"));
    assert_eq!(count("~/(?i)bob/"), n(&|r| r.user == "Bob"));
    // a field that starts with, ends with, or is exactly the text
    assert_eq!(count("~/^WS-0/"), n(&|r| r.host.starts_with("WS-0")));
    assert_eq!(count("~/(?i)debug$/"), 1);
    assert_eq!(count("~/^alice$/"), n(&|r| r.user == "alice"));
    // a whole word: "netsvcs" yes, "netsvc" no
    assert_eq!(count("~/\\bnetsvcs\\b/"), n(&|r| r.cmd.contains("netsvcs")));
    assert_eq!(count("~/\\bnetsvc\\b/"), 0);
    // joined with other terms like any term
    assert_eq!(
        count("~/^alice$/ EventID=4688"),
        n(&|r| r.user == "alice" && r.ev == 4688)
    );
    // one column, as before
    assert_eq!(count("User~/^ali/"), n(&|r| r.user.starts_with("ali")));
}

#[test]
fn the_text_inside_a_simple_pattern_is_known() {
    use logseek_core::query::exec::plain_text_of;
    assert_eq!(plain_text_of("(?i)^cmd\\.exe$").as_deref(), Some("cmd.exe"));
    assert_eq!(plain_text_of("\\bnetsvcs\\b").as_deref(), Some("netsvcs"));
    assert_eq!(plain_text_of("a\\$").as_deref(), Some("a$"));
    assert_eq!(plain_text_of("a.*b"), None);
    assert_eq!(plain_text_of("\\d+"), None);
    assert_eq!(plain_text_of("^$"), None);
}

#[test]
fn is_not_keeps_the_rows_that_have_no_value() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("u.csv");
    let mut text = String::from("Id,User,Bytes\n");
    for i in 0..3000 {
        let user = ["bob", "", "alice"][i % 3];
        let bytes = if i % 5 == 0 {
            String::new()
        } else {
            (i % 7).to_string()
        };
        text.push_str(&format!("{i},{user},{bytes}\n"));
    }
    std::fs::write(&src, text).unwrap();
    let dir = d.path().join("u.lsidx");
    let job = start_ingest(&src, &dir, Budget::new(256 << 20), IngestOptions::default());
    while matches!(job.state(), JobState::Running) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(job.state(), JobState::Done);
    let st = Store::open(&dir).unwrap();
    let c = cache();
    let count = |q: &str| {
        filter(&st, &parse(q).unwrap(), &c, &AtomicBool::new(false))
            .unwrap()
            .count()
    };
    // "is not bob" and "not (is bob)" are one question with one answer:
    // a row with no user is not bob
    assert_eq!(count("User=bob"), 1000);
    assert_eq!(count("NOT User=bob"), 2000);
    assert_eq!(count("User!=bob"), 2000);
    assert_eq!(count("User<>bob"), 2000);
    // the same on a column of numbers
    let three = (0..3000).filter(|i| i % 5 != 0 && i % 7 == 3).count() as u64;
    assert_eq!(count("Bytes=3"), three);
    assert_eq!(count("Bytes!=3"), 3000 - three);
    assert_eq!(count("Bytes != 3"), 3000 - three);
}
