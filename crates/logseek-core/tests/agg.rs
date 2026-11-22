//! Aggregation: counts, top values, stats and time charts over an index.
use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::agg::{run_stats, timechart, top_k_sorted};
use logseek_core::query::ast::{Agg, AggFn};
use logseek_core::query::exec::{filter, ChunkCache, MatchSet};
use logseek_core::query::parse;
use logseek_core::store::Store;
use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::AtomicBool;

struct R {
    t: i64,
    host: String,
    user: String,
    bytes: f64,
    id: u64,
}

fn make(n: usize) -> (tempfile::TempDir, Store, Vec<R>) {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("a.csv");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
    writeln!(f, "Time,Host,User,Bytes,Id").unwrap();
    let mut v = Vec::new();
    for i in 0..n {
        let t = 1_794_564_000 + (i as i64) * 7;
        let r = R {
            t,
            host: format!("ws-{:02}", (i * 7919) % 23),
            user: ["alice", "bob", "carol"][i % 3].into(),
            bytes: ((i * 37) % 1000) as f64 + 0.25,
            id: (i as u64) * 2_654_435_761 % 1_000_003,
        };
        writeln!(f, "{},{},{},{:.2},{}", t, r.host, r.user, r.bytes, r.id).unwrap();
        v.push(r);
    }
    drop(f);
    let dir = d.path().join("a.lsidx");
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
    (d, Store::open(&dir).unwrap(), v)
}

fn all(st: &Store) -> MatchSet {
    MatchSet::all(st)
}
fn c() -> ChunkCache {
    ChunkCache::new(Budget::new(64 << 20))
}
fn no() -> AtomicBool {
    AtomicBool::new(false)
}

#[test]
fn stats_count_by_matches_bruteforce() {
    let (_d, st, v) = make(60_000);
    let r = run_stats(
        &st,
        &all(&st),
        &[Agg {
            func: AggFn::Count,
            col: None,
        }],
        &["Host".into()],
        &c(),
        &no(),
    )
    .unwrap();
    let mut want: HashMap<String, u64> = HashMap::new();
    for x in &v {
        *want.entry(x.host.clone()).or_default() += 1;
    }
    assert_eq!(r.len(), want.len());
    for (k, vals) in r.iter() {
        assert_eq!(vals[0] as u64, want[&k[0]], "{k:?}");
    }
    assert!(r.exact);
}

#[test]
fn stats_numeric_aggregates() {
    let (_d, st, v) = make(30_000);
    let m = filter(&st, &parse("User=bob").unwrap(), &c(), &no()).unwrap();
    let aggs = [
        Agg {
            func: AggFn::Count,
            col: None,
        },
        Agg {
            func: AggFn::Sum,
            col: Some("Bytes".into()),
        },
        Agg {
            func: AggFn::Avg,
            col: Some("Bytes".into()),
        },
        Agg {
            func: AggFn::Min,
            col: Some("Bytes".into()),
        },
        Agg {
            func: AggFn::Max,
            col: Some("Bytes".into()),
        },
        Agg {
            func: AggFn::Dc,
            col: Some("Host".into()),
        },
    ];
    let r = run_stats(&st, &m, &aggs, &[], &c(), &no()).unwrap();
    let bobs: Vec<&R> = v.iter().filter(|x| x.user == "bob").collect();
    let sum: f64 = bobs.iter().map(|x| x.bytes).sum();
    let g = &r.get(0).unwrap().1;
    assert_eq!(g[0] as usize, bobs.len());
    assert!((g[1] - sum).abs() < 1e-6);
    assert!((g[2] - sum / bobs.len() as f64).abs() < 1e-9);
    assert_eq!(g[3], bobs.iter().map(|x| x.bytes).fold(f64::MAX, f64::min));
    assert_eq!(g[4], bobs.iter().map(|x| x.bytes).fold(f64::MIN, f64::max));
    let hosts: std::collections::HashSet<&String> = bobs.iter().map(|x| &x.host).collect();
    assert_eq!(g[5] as usize, hosts.len());
}

#[test]
fn stats_on_unique_column_multipass_under_budget() {
    let (_d, st, v) = make(120_000);
    let cache = ChunkCache::new(Budget::new(3 << 20));
    let r = run_stats(
        &st,
        &all(&st),
        &[Agg {
            func: AggFn::Count,
            col: None,
        }],
        &["Id".into()],
        &cache,
        &no(),
    )
    .unwrap();
    let mut want: HashMap<u64, u64> = HashMap::new();
    for x in &v {
        *want.entry(x.id).or_default() += 1;
    }
    assert_eq!(r.len(), want.len());
    assert_eq!(r.iter().map(|g| g.1[0] as u64).sum::<u64>(), v.len() as u64);
    for (k, vals) in r.iter().take(500) {
        assert_eq!(vals[0] as u64, want[&k[0].parse::<u64>().unwrap()]);
    }
    assert!(
        r.passes > 1,
        "expected partitioned passes, got {}",
        r.passes
    );
    assert!(
        r.spilled(),
        "a result larger than the budget must spill, not stay in RAM"
    );
    assert!(cache.budget().peak() <= cache.budget().cap());
    assert!(r.exact);
}

#[test]
fn top_k_equals_full_sort_prefix() {
    let (_d, st, v) = make(40_000);
    let rows = top_k_sorted(
        &st,
        &all(&st),
        &[("Bytes".into(), true), ("Id".into(), false)],
        50,
        &c(),
        &no(),
    )
    .unwrap();
    let mut idx: Vec<usize> = (0..v.len()).collect();
    idx.sort_by(|&a, &b| {
        v[b].bytes
            .partial_cmp(&v[a].bytes)
            .unwrap()
            .then(v[a].id.cmp(&v[b].id))
    });
    let want: Vec<u64> = idx[..50].iter().map(|&i| i as u64).collect();
    assert_eq!(rows, want);
    let rows = top_k_sorted(
        &st,
        &all(&st),
        &[("Host".into(), false), ("Id".into(), false)],
        10,
        &c(),
        &no(),
    )
    .unwrap();
    let mut idx: Vec<usize> = (0..v.len()).collect();
    idx.sort_by(|&a, &b| v[a].host.cmp(&v[b].host).then(v[a].id.cmp(&v[b].id)));
    assert_eq!(
        rows,
        idx[..10].iter().map(|&i| i as u64).collect::<Vec<_>>()
    );
}

#[test]
fn timechart_buckets_sum_to_count() {
    let (_d, st, v) = make(30_000);
    let span = 3_600_000_000i64;
    let s = timechart(
        &st,
        &all(&st),
        span,
        &Agg {
            func: AggFn::Count,
            col: None,
        },
        None,
        &c(),
        &no(),
    )
    .unwrap();
    assert_eq!(
        s.points.iter().map(|p| p.2 as u64).sum::<u64>(),
        v.len() as u64
    );
    let first = (v[0].t * 1_000_000).div_euclid(span) * span;
    assert_eq!(s.points[0].0, first);
    assert!(s.points.windows(2).all(|w| w[0].0 <= w[1].0));
    let by = timechart(
        &st,
        &all(&st),
        span,
        &Agg {
            func: AggFn::Count,
            col: None,
        },
        Some("User"),
        &c(),
        &no(),
    )
    .unwrap();
    assert_eq!(
        by.points.iter().map(|p| p.2 as u64).sum::<u64>(),
        v.len() as u64
    );
    assert!(by.points.iter().any(|p| p.1.as_deref() == Some("carol")));
}
