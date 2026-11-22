//! `cargo run --release --example explore -- <source file> <store_dir>`
//! Opens any file (an .evtx is turned into JSON lines first, as the app does), then runs every
//! finding engine with nothing told about the data, and prints what each one found first.
//! Column choices are made from the file itself: the text column is the longest filled one,
//! repeats and pairs use columns with few different values.
use logseek_core::budget::{Budget, Machine};
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::contrast::{contrast, ContrastOpts};
use logseek_core::query::exec::{page, ChunkCache, FormulaSet, MatchSet};
use logseek_core::query::pairs::rare_pairs;
use logseek_core::query::patterns::{pattern_words, patterns};
use logseek_core::query::rhythm::{regular, timeline_signals};
use logseek_core::query::shape::{column_shapes, shape_words, Shape};
use logseek_core::query::strange::strange_text;
use logseek_core::query::unusual::{unusual_by, Weigh};
use logseek_core::store::Store;
use logseek_core::sysmem::process_memory;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::time::Instant;

fn short(s: &str, n: usize) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    one.chars().take(n).collect()
}
fn val(v: &Option<String>, n: usize) -> String {
    v.as_deref()
        .map(|s| short(s, n))
        .unwrap_or_else(|| "(empty)".into())
}
fn ram() -> String {
    format!(
        "[RAM {:.0} MB]",
        process_memory().private as f64 / 1048576.0
    )
}
fn when(us: i64) -> String {
    logseek_core::types::fmt_time(us)
}

fn open(src: &Path, dir: &Path) -> Store {
    if let Ok(st) = Store::open(dir) {
        return st;
    }
    let t = Instant::now();
    let mut feed = src.to_path_buf();
    if src.extension().map(|e| e.eq_ignore_ascii_case("evtx")) == Some(true) {
        let out: PathBuf = dir.with_extension("jsonl");
        let e = logseek_core::ingest::evtx::evtx_to_jsonl(
            src,
            &out,
            &AtomicBool::new(false),
            &AtomicU64::new(0),
        )
        .unwrap();
        println!("evtx: {} events turned into JSON lines", e.events);
        feed = out;
    }
    let job = start_ingest(&feed, dir, Budget::new(512 << 20), IngestOptions::default());
    while matches!(job.state(), JobState::Running) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    println!(
        "indexed in {:.1} s ({:?})",
        t.elapsed().as_secs_f64(),
        job.state()
    );
    Store::open(dir).unwrap()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (src, dir) = (Path::new(&a[1]), Path::new(&a[2]));
    // a workbook: each sheet turned into rows, as the app does, and read on its own
    if logseek_core::ingest::sheets::is_workbook(src) {
        std::fs::create_dir_all(dir).unwrap();
        for s in logseek_core::ingest::sheets::sheets(src).unwrap() {
            let safe: String = s
                .name
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { '_' })
                .collect();
            let csv = dir.join(format!("{safe}.csv"));
            if !csv.exists() {
                let o = logseek_core::ingest::sheets::sheet_to_csv(
                    src,
                    &s.name,
                    &csv,
                    &AtomicBool::new(false),
                    &AtomicU64::new(0),
                )
                .unwrap();
                if o.rows == 0 {
                    continue;
                }
            }
            println!("\n######## sheet {}", s.name);
            explore(open(&csv, &dir.join(format!("{safe}.lsidx"))));
        }
        return;
    }
    explore(open(src, dir));
}

fn explore(st: Store) {
    let m = Machine::detect();
    let cache = ChunkCache::new(Budget::elastic(256 << 20, m.elastic_ceiling()));
    let no = AtomicBool::new(false);
    let fs = FormulaSet::default();
    let all = MatchSet::all(&st);
    let names: Vec<String> = st.schema().columns.iter().map(|c| c.name.clone()).collect();
    let tc = st.schema().time_col;
    let cols: Vec<usize> = (0..names.len()).collect();
    println!(
        "\n== {} rows x {} columns; time column {:?}",
        st.row_count(),
        names.len(),
        tc.map(|c| names[c].as_str())
    );

    // A look at up to 5,000 rows spread over the file decides which columns to read.
    let total = st.row_count();
    let step = (total / 50).max(1);
    let mut seen: Vec<HashSet<String>> = vec![HashSet::new(); names.len()];
    let mut filled = vec![0u64; names.len()];
    let mut len = vec![0u64; names.len()];
    let mut looked = 0u64;
    let mut start = 0u64;
    while start < total {
        let p = page(&st, &all, start, 100, &cols, &cache).unwrap();
        for row in &p.cells {
            looked += 1;
            for (c, v) in row.iter().enumerate() {
                if let Some(v) = v.as_deref().filter(|v| !v.is_empty()) {
                    filled[c] += 1;
                    len[c] += v.len() as u64;
                    seen[c].insert(v.to_string());
                }
            }
        }
        start += step;
    }
    let half = |c: usize| filled[c] * 2 >= looked;
    let text = (0..names.len())
        .filter(|&c| Some(c) != tc && half(c))
        .max_by_key(|&c| len[c] / filled[c].max(1));
    let few: Vec<usize> = (0..names.len())
        .filter(|&c| Some(c) != tc && half(c) && (2..=60).contains(&seen[c].len()))
        .collect();
    let many = (0..names.len())
        .filter(|&c| {
            Some(c) != tc
                && half(c)
                && Some(c) != text
                && seen[c].len() > 60
                && (seen[c].len() as u64) * 5 <= filled[c]
        })
        .max_by_key(|&c| seen[c].len());

    // Unusual rows
    let t = Instant::now();
    let u = unusual_by(
        &st,
        &all,
        &cols,
        None,
        Weigh::All,
        &fs,
        &cache,
        &no,
        &AtomicU64::new(0),
    )
    .unwrap();
    println!(
        "\n-- {} Unusual rows: {:.1} s; {} rows stand out (usual score {:.1}, line {:.1})",
        ram(),
        t.elapsed().as_secs_f64(),
        u.rows.len(),
        u.usual,
        u.line
    );
    for &r in u.rows.iter().take(10) {
        let p = page(&st, &all, r, 1, &cols, &cache).unwrap();
        let why = u.model.why(&p.cells[0]);
        let s: Vec<String> = why
            .iter()
            .take(3)
            .map(|x| {
                format!(
                    "{} = {} ({} of {})",
                    names[x.col],
                    val(&x.value, 60),
                    x.rows,
                    x.of
                )
            })
            .collect();
        let at = tc.map(|c| val(&p.cells[0][c], 30)).unwrap_or_default();
        println!("  row {r} {at}: {}", s.join(" | "));
    }

    // What sets the unusual rows apart
    if !u.rows.is_empty() {
        let here = all.restrict(&st, &u.rows);
        let rest = all.minus(&here);
        let ccols: Vec<usize> = cols.iter().copied().filter(|&c| Some(c) != tc).collect();
        let t = Instant::now();
        let c = contrast(
            &st,
            &here,
            &rest,
            &ccols,
            &fs,
            &cache,
            &ContrastOpts::default(),
            &no,
            &AtomicU64::new(0),
        )
        .unwrap();
        println!(
            "\n-- {} What sets the unusual rows apart: {:.1} s; {} findings",
            ram(),
            t.elapsed().as_secs_f64(),
            c.findings.len()
        );
        for f in c.findings.iter().take(8) {
            let w: Vec<String> = f
                .conds
                .iter()
                .map(|k| format!("{} is {}", names[k.col], val(&k.value, 60)))
                .collect();
            println!(
                "  {:>7.1}x  here {} of {}  rest {} of {}  {}",
                f.ratio(),
                f.here,
                f.here_total,
                f.rest,
                f.rest_total,
                w.join(" and ")
            );
        }
    }

    // Timeline
    if tc.is_some() {
        let t = Instant::now();
        let s = timeline_signals(&st, &all, &cache, &no, &AtomicU64::new(0)).unwrap();
        println!(
            "\n-- {} Timeline: {:.1} s; {} to {}; usual pause {} s; {} silent gaps, {} steps",
            ram(),
            t.elapsed().as_secs_f64(),
            when(s.first),
            when(s.last),
            s.usual_pause / 1_000_000,
            s.gaps.len(),
            s.steps.len()
        );
        for g in s.gaps.iter().take(6) {
            println!(
                "  silent {} min: {} to {}",
                (g.to - g.from) / 60_000_000,
                when(g.from),
                when(g.to)
            );
        }
        for x in s.steps.iter().take(6) {
            println!(
                "  step at {}: {:.1} -> {:.1} rows/hour",
                when(x.at),
                x.before * 3600.0,
                x.after * 3600.0
            );
        }
        for &c in &few {
            let r = regular(&st, &all, c, &fs, &cache, &no, &AtomicU64::new(0)).unwrap();
            for v in r.values.iter().filter(|v| v.score >= 0.8).take(3) {
                println!(
                    "  regular: {} = {} every {} s +/- {} s ({} rows, score {:.2})",
                    names[c],
                    val(&v.value, 60),
                    v.every / 1_000_000,
                    v.give_or_take / 1_000_000,
                    v.rows,
                    v.score
                );
            }
        }
    }

    // The text column
    if let Some(c) = text {
        println!("\n-- {} Text column: {}", ram(), names[c]);
        let t = Instant::now();
        let (shapes, _) = column_shapes(
            &st,
            &all,
            c,
            Shape::AnyLength,
            &fs,
            &cache,
            &no,
            &AtomicU64::new(0),
        )
        .unwrap();
        println!(
            "  {} shapes: {:.1} s, {} shapes; rarest:",
            ram(),
            t.elapsed().as_secs_f64(),
            shapes.len()
        );
        for s in shapes.iter().rev().take(4) {
            println!(
                "    {:>7} rows  {}  e.g. {}",
                s.rows,
                shape_words(&s.pattern).unwrap_or_else(|| "(plain text)".into()),
                short(&s.example, 80)
            );
        }
        let t = Instant::now();
        let p = patterns(
            &st,
            &all,
            c,
            &Default::default(),
            &fs,
            &cache,
            &no,
            &AtomicU64::new(0),
        )
        .unwrap();
        println!(
            "  {} patterns: {:.1} s, {} patterns; most and fewest rows:",
            ram(),
            t.elapsed().as_secs_f64(),
            p.patterns.len()
        );
        for x in p.patterns.iter().take(5) {
            println!("    {:>7} rows  {}", x.rows, short(&pattern_words(x), 140));
        }
        for x in p.patterns.iter().rev().take(5) {
            println!("    {:>7} rows  {}", x.rows, short(&pattern_words(x), 140));
        }
        let t = Instant::now();
        let s = strange_text(&st, &all, c, None, &fs, &cache, &no, &AtomicU64::new(0)).unwrap();
        println!(
            "  {} strange text: {:.1} s, {} values in {} rows:",
            ram(),
            t.elapsed().as_secs_f64(),
            s.values.len(),
            s.rows.len()
        );
        for v in s.values.iter().take(5) {
            println!(
                "    {:.1} bits  {} rows  {}",
                v.score,
                v.rows,
                short(&v.text, 140)
            );
        }
    }

    // Rare pairs
    if let Some(b) = many {
        for &a in few.iter().take(4) {
            let t = Instant::now();
            let p = rare_pairs(&st, &all, a, b, &fs, &cache, &no, &AtomicU64::new(0)).unwrap();
            println!(
                "\n-- {} Rare pairs {} with {}: {:.1} s",
                ram(),
                names[a],
                names[b],
                t.elapsed().as_secs_f64()
            );
            for l in p.unusual.iter().take(4) {
                println!(
                    "  {} = {} with {} = {}: {} rows of {}, {} partners",
                    names[a],
                    val(&l.a, 40),
                    names[b],
                    val(&l.b, 70),
                    l.rows,
                    l.a_rows,
                    l.partners
                );
            }
        }
    }
    println!(
        "\nRAM now {:.0} MB; after letting the cache go: {}",
        process_memory().private as f64 / 1048576.0,
        {
            cache.clear();
            ram()
        }
    );
}
