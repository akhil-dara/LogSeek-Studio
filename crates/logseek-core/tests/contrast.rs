//! What sets some rows apart from the rest: the values, and pairs of values,
//! far more common in these rows than in the others, with exact counts.

use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::contrast::{contrast, Cond, ContrastOpts};
use logseek_core::query::exec::{filter, ChunkCache, FormulaSet, MatchSet};
use logseek_core::query::parse;
use logseek_core::store::Store;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64};

struct Row {
    hour: u32,
    app: &'static str,
    region: &'static str,
    host: String,
    user: &'static str,
    id: String,
    site: &'static str,
    note: &'static str,
}

/// 24 hours of rows. In hour 13 most rows come from release "v2" in region
/// "ap" together; elsewhere "v2" and "ap" each occur, but rarely together.
fn make() -> (tempfile::TempDir, Store, Vec<Row>) {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("app.csv");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
    writeln!(f, "Time,App,Region,Host,User,Id,Site,Note,Mirror").unwrap();
    let mut data = Vec::new();
    let mut n = 0u64;
    for hour in 0..24u32 {
        let rows = if hour == 13 { 3000 } else { 1000 };
        for i in 0..rows {
            n += 1;
            let (app, region) = if hour == 13 {
                match i % 10 {
                    0..=8 => ("v2", "ap"),
                    _ => ("v1", "eu"),
                }
            } else {
                match i % 50 {
                    0..=4 => ("v2", "eu"),  // v2 alone: 10%
                    5..=19 => ("v1", "ap"), // ap alone: 30%
                    20 => ("v2", "ap"),     // together: 2%
                    _ => ("v1", "eu"),
                }
            };
            let r = Row {
                hour,
                app,
                region,
                host: format!("h{}", i % 7),
                user: ["ann", "bo", "cy"][i as usize % 3],
                id: format!("id-{n}"),
                site: "main",
                // empty in the busy hour far more often than elsewhere
                note: if (hour == 13 && i % 4 != 0) || (hour != 13 && i % 25 == 0) {
                    ""
                } else {
                    "ok"
                },
            };
            writeln!(
                f,
                "2026-11-12 {:02}:{:02}:{:02},{},{},{},{},{},{},{},{}",
                hour,
                i / 60 % 60,
                i % 60,
                r.app,
                r.region,
                r.host,
                r.user,
                r.id,
                r.site,
                r.note,
                // empty in exactly the rows where Note is empty
                if r.note.is_empty() { "" } else { "m" }
            )
            .unwrap();
            data.push(r);
        }
    }
    drop(f);
    let dir = d.path().join("app.lsidx");
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
    (d, Store::open(&dir).unwrap(), data)
}

fn val(r: &Row, col: &str) -> String {
    match col {
        "App" => r.app.into(),
        "Region" => r.region.into(),
        "Host" => r.host.clone(),
        "User" => r.user.into(),
        "Id" => r.id.clone(),
        "Site" => r.site.into(),
        "Note" => r.note.into(),
        "Mirror" => if r.note.is_empty() { "" } else { "m" }.into(),
        _ => unreachable!("{col}"),
    }
}

#[test]
fn the_values_and_pairs_that_set_a_period_apart_are_found_with_exact_counts() {
    let (_d, st, data) = make();
    let cache = ChunkCache::new(Budget::new(64 << 20));
    let no = AtomicBool::new(false);
    let here = filter(
        &st,
        &parse("@2026-11-12T13:00:00..2026-11-12T14:00:00").unwrap(),
        &cache,
        &no,
    )
    .unwrap();
    let all = MatchSet::all(&st);
    let rest = all.minus(&here);
    assert_eq!(here.count(), 3000);
    assert_eq!(rest.count(), 23_000);
    let names: Vec<String> = st.schema().columns.iter().map(|c| c.name.clone()).collect();
    let cols: Vec<usize> = (1..names.len()).collect(); // every column but the time
    let c = contrast(
        &st,
        &here,
        &rest,
        &cols,
        &FormulaSet::default(),
        &cache,
        &ContrastOpts::default(),
        &no,
        &AtomicU64::new(0),
    )
    .unwrap();
    assert_eq!((c.here_total, c.rest_total), (3000, 23_000));

    // every finding's counts are the true counts
    let holds = |r: &Row, conds: &[Cond]| {
        conds
            .iter()
            .all(|k| val(r, &names[k.col]) == k.value.clone().unwrap_or_default())
    };
    for f in &c.findings {
        let h = data
            .iter()
            .filter(|r| r.hour == 13 && holds(r, &f.conds))
            .count() as u64;
        let o = data
            .iter()
            .filter(|r| r.hour != 13 && holds(r, &f.conds))
            .count() as u64;
        assert_eq!((f.here, f.rest), (h, o), "{:?}", f.conds);
        // nothing is offered that is not far commoner here
        assert!(f.share_here() >= 0.05, "{:?}", f.conds);
        assert!(f.ratio() >= 2.0, "{:?}", f.conds);
    }

    // strongest first
    for w in c.findings.windows(2) {
        assert!(w[0].ratio() >= w[1].ratio());
    }
    // the release and the region together explain far more than either alone
    let words = |f: &logseek_core::query::contrast::Finding| -> Vec<(String, Option<String>)> {
        f.conds
            .iter()
            .map(|k| (names[k.col].clone(), k.value.clone()))
            .collect()
    };
    let at = |w: Vec<(&str, &str)>| {
        let w: Vec<(String, Option<String>)> = w
            .into_iter()
            .map(|(a, b)| (a.to_string(), Some(b.to_string())))
            .collect();
        c.findings
            .iter()
            .position(|f| words(f) == w)
            .unwrap_or_else(|| panic!("{w:?} not found"))
    };
    let pair = at(vec![("App", "v2"), ("Region", "ap")]);
    assert_eq!((c.findings[pair].here, c.findings[pair].rest), (2700, 460));
    assert!(pair < at(vec![("App", "v2")]));
    assert!(pair < at(vec![("Region", "ap")]));

    // an empty value can set rows apart too; two columns empty in exactly
    // the same rows are said once, the other named as holding in the same rows
    let empty_lines: Vec<_> = c
        .findings
        .iter()
        .filter(|f| {
            f.conds.len() == 1
                && f.conds[0].value.is_none()
                && ["Note", "Mirror"].contains(&names[f.conds[0].col].as_str())
        })
        .collect();
    assert_eq!(empty_lines.len(), 1, "{empty_lines:?}");
    let e = empty_lines[0];
    assert_eq!(e.same.len(), 1);
    let other: Vec<&str> = e.same[0].iter().map(|k| names[k.col].as_str()).collect();
    assert_eq!(other.len(), 1);
    assert!(other[0] != names[e.conds[0].col] && ["Note", "Mirror"].contains(&other[0]));
    // and no line pairs a condition with one that holds in the same rows,
    // nor repeats a pair with the other column in its place
    for f in c.findings.iter().filter(|f| f.conds.len() == 2) {
        let cs: Vec<&str> = f.conds.iter().map(|k| names[k.col].as_str()).collect();
        assert!(!(cs.contains(&"Note") && cs.contains(&"Mirror")), "{cs:?}");
        if f.conds.iter().any(|k| k.value.is_none()) {
            assert!(
                !cs.contains(&"Mirror")
                    || !c.findings.iter().any(|g| {
                        g.conds.len() == 2
                            && g.conds
                                .iter()
                                .any(|k| names[k.col] == "Note" && k.value.is_none())
                            && g.conds
                                .iter()
                                .any(|k| f.conds.contains(k) && names[k.col] != "Mirror")
                    }),
                "{cs:?} said twice"
            );
        }
    }

    // a column that names every row, and one with the same value everywhere,
    // never explain anything
    for f in &c.findings {
        for k in &f.conds {
            assert!(
                names[k.col] != "Id" && names[k.col] != "Site",
                "{:?}",
                f.conds
            );
        }
    }
    // values spread evenly over the day explain nothing either
    for f in &c.findings {
        for k in &f.conds {
            assert!(
                names[k.col] != "Host" && names[k.col] != "User",
                "{:?}",
                f.conds
            );
        }
    }
    // the pair adds something only when it is stronger than each value alone;
    // a pair no stronger than its parts is not offered
    for f in c.findings.iter().filter(|f| f.conds.len() == 2) {
        for k in &f.conds {
            if let Some(single) = c
                .findings
                .iter()
                .find(|g| g.conds.len() == 1 && g.conds[0] == *k)
            {
                assert!(f.ratio() > single.ratio(), "{:?}", f.conds);
            }
        }
    }
}

#[test]
fn a_stricter_share_or_ratio_offers_fewer_and_nothing_is_offered_when_rows_are_alike() {
    let (_d, st, _) = make();
    let cache = ChunkCache::new(Budget::new(64 << 20));
    let no = AtomicBool::new(false);
    let here = filter(
        &st,
        &parse("@2026-11-12T13:00:00..2026-11-12T14:00:00").unwrap(),
        &cache,
        &no,
    )
    .unwrap();
    let rest = MatchSet::all(&st).minus(&here);
    let cols: Vec<usize> = (1..st.schema().len()).collect();
    let run = |here: &MatchSet, rest: &MatchSet, o: ContrastOpts| {
        contrast(
            &st,
            here,
            rest,
            &cols,
            &FormulaSet::default(),
            &cache,
            &o,
            &no,
            &AtomicU64::new(0),
        )
        .unwrap()
    };
    let loose = run(&here, &rest, ContrastOpts::default());
    let strict = run(
        &here,
        &rest,
        ContrastOpts {
            min_share: 0.5,
            min_ratio: 5.0,
        },
    );
    assert!(!strict.findings.is_empty());
    assert!(strict.findings.len() < loose.findings.len());
    // two quiet hours against the other quiet hours: nothing stands out
    let a = filter(
        &st,
        &parse("@2026-11-12T02:00:00..2026-11-12T04:00:00").unwrap(),
        &cache,
        &no,
    )
    .unwrap();
    let quiet = MatchSet::all(&st).minus(&here).minus(&a);
    assert!(run(&a, &quiet, ContrastOpts::default()).findings.is_empty());
    // no rows on one side: nothing to compare, and no error
    let none = MatchSet::all(&st).minus(&MatchSet::all(&st));
    assert_eq!(none.count(), 0);
    assert!(run(&none, &rest, ContrastOpts::default())
        .findings
        .is_empty());
}

#[test]
fn a_comparison_can_be_stopped() {
    let (_d, st, _) = make();
    let cache = ChunkCache::new(Budget::new(64 << 20));
    let all = MatchSet::all(&st);
    let cols: Vec<usize> = (1..st.schema().len()).collect();
    let r = contrast(
        &st,
        &all,
        &all,
        &cols,
        &FormulaSet::default(),
        &cache,
        &ContrastOpts::default(),
        &AtomicBool::new(true),
        &AtomicU64::new(0),
    );
    assert!(r.is_err());
}

#[test]
fn pairs_with_the_same_counts_around_one_condition_are_said_once() {
    use logseek_core::query::contrast::{fold_alternatives, Finding};
    let c = |col: usize, v: Option<&str>| Cond {
        col,
        value: v.map(|s| s.to_string()),
    };
    let pair = |a: Cond, b: Cond, here: u64, rest: u64| Finding {
        conds: vec![a, b],
        same: vec![Vec::new(), Vec::new()],
        instead: None,
        here,
        here_total: 1000,
        rest,
        rest_total: 5000,
    };
    let il = c(1, Some("16384"));
    let f = vec![
        pair(il.clone(), c(2, None), 800, 0),
        pair(il.clone(), c(3, None), 800, 0),
        pair(c(4, Some("x")), c(5, Some("y")), 700, 3),
        pair(il.clone(), c(6, None), 800, 0),
        // the same shared condition with other counts: a line of its own
        pair(il.clone(), c(7, None), 650, 0),
    ];
    let out = fold_alternatives(f);
    assert_eq!(out.len(), 3);
    assert_eq!(out[0].conds, vec![il.clone(), c(2, None)]);
    assert_eq!(out[0].instead, Some((1, vec![c(3, None), c(6, None)])));
    assert_eq!(out[1].conds, vec![c(4, Some("x")), c(5, Some("y"))]);
    assert_eq!(out[1].instead, None);
    assert_eq!(out[2].conds, vec![il, c(7, None)]);
    assert_eq!(out[2].instead, None);
}
