//! Strange text for a column: values written unlike the rest of the column
//! stand out, measured against the column itself.

use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::exec::{filter, ChunkCache, FormulaSet, MatchSet};
use logseek_core::query::parse;
use logseek_core::query::strange::{share_below, strange_text};
use logseek_core::store::Store;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64};

const ODD: [&str; 3] = [
    "powershell -enc SQBFAFgAIAAoAE4AZQB3AC0ATwBiAGoAZQBjAHQAIABOAGUAdAAuAFcAZQBiAEMAbABpAGUAbgB0ACkA",
    "p^o^w^e^r^s^h^e^l^l -w h^i^d^d^e^n",
    "Ã©Ã¨Ã Ã§Ã´ cafÃ© rÃ©sumÃ©",
];

fn make() -> (tempfile::TempDir, Store, Vec<(u64, String)>) {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("cmd.csv");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
    writeln!(f, "Host,Cmd").unwrap();
    let mut odd_rows = Vec::new();
    for i in 0..30_000u64 {
        let cmd = match i {
            777 | 21_000 => ODD[0].to_string(),
            5_005 => ODD[1].to_string(),
            12_345 => ODD[2].to_string(),
            _ => [
                format!(
                    "C:\\Windows\\system32\\svchost.exe -k netsvcs -p -s {}",
                    ["Schedule", "Winmgmt", "BITS"][i as usize % 3]
                ),
                format!(
                    "\"C:\\Program Files\\Mozilla Firefox\\firefox.exe\" -contentproc {} tab",
                    i % 40
                ),
                "C:\\Windows\\explorer.exe".to_string(),
            ][i as usize % 3]
                .clone(),
        };
        if ODD.contains(&cmd.as_str()) {
            odd_rows.push((i, cmd.clone()));
        }
        writeln!(f, "WS-{:02},\"{}\"", i % 9, cmd.replace('"', "\"\"")).unwrap();
    }
    drop(f);
    let dir = d.path().join("c.lsidx");
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
    (d, Store::open(&dir).unwrap(), odd_rows)
}

#[test]
fn values_written_unlike_the_rest_of_the_column_come_first() {
    let (_d, st, odd_rows) = make();
    let cache = ChunkCache::new(Budget::new(64 << 20));
    let no = AtomicBool::new(false);
    let col = st.schema().index_of("Cmd").unwrap();
    let s = strange_text(
        &st,
        &MatchSet::all(&st),
        col,
        None,
        &FormulaSet::default(),
        &cache,
        &no,
        &AtomicU64::new(0),
    )
    .unwrap();
    // the spread covers every row (none of these is too short to score)
    assert_eq!(s.spread.iter().sum::<u64>() + s.unscored, 30_000);
    // the strangest thousandth: exactly the four odd rows, three values
    let mut texts: Vec<&str> = s.values.iter().map(|v| v.text.as_str()).collect();
    texts.sort_unstable();
    let mut want = ODD.to_vec();
    want.sort_unstable();
    assert_eq!(texts, want);
    assert_eq!(
        s.values.iter().find(|v| v.text == ODD[0]).unwrap().rows,
        2,
        "a value in two rows counts both"
    );
    let ids: Vec<u64> = odd_rows.iter().map(|(i, _)| *i).collect();
    assert_eq!(s.rows, ids);
    // strangest first, and each far from the column's usual text
    for w in s.values.windows(2) {
        assert!(w[0].score >= w[1].score);
    }
    for v in &s.values {
        assert!(
            v.score > s.usual + 1.0,
            "{} {} vs {}",
            v.text,
            v.score,
            s.usual
        );
        assert!(share_below(&s.spread, v.score) > 0.99);
    }
    // a lower line takes in more values, never fewer
    let more = strange_text(
        &st,
        &MatchSet::all(&st),
        col,
        Some(s.usual),
        &FormulaSet::default(),
        &cache,
        &no,
        &AtomicU64::new(0),
    )
    .unwrap();
    assert!(more.values.len() > s.values.len());
    // only the rows shown are read: without the odd rows nothing that odd is left
    let some = filter(&st, &parse("Cmd:svchost").unwrap(), &cache, &no).unwrap();
    let s2 = strange_text(
        &st,
        &some,
        col,
        Some(s.values.last().unwrap().score),
        &FormulaSet::default(),
        &cache,
        &no,
        &AtomicU64::new(0),
    )
    .unwrap();
    assert!(s2.values.is_empty(), "{:?}", s2.values);
    assert_eq!(s2.spread.iter().sum::<u64>(), some.count());
}
