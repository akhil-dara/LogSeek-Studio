//! Elasticsearch bulk files: an instruction line before every document.
//! One row per document, the instruction's index and id kept as columns.

use logseek_core::budget::Budget;
use logseek_core::ingest::detect::detect_spec;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState, Keep};
use logseek_core::query::exec::{page, ChunkCache, MatchSet};
use logseek_core::store::{SourceSpec, Store};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

fn wait(job: &logseek_core::ingest::pipeline::IngestJob) -> JobState {
    let t = Instant::now();
    loop {
        let s = job.state();
        if !matches!(s, JobState::Running) {
            return s;
        }
        assert!(t.elapsed() < Duration::from_secs(120), "ingest timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn write_bulk(path: &Path, docs: usize) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    for i in 0..docs {
        writeln!(
            f,
            r#"{{"index":{{"_index":"logstash-2015.05.{:02}","_id":"doc{i}"}}}}"#,
            18 + i % 3
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"@timestamp":"2015-05-18T09:{:02}:{:02}.000Z","ip":"10.0.0.{}","response":"{}"}}"#,
            (i / 60) % 60,
            i % 60,
            i % 250,
            [200, 404][i % 2]
        )
        .unwrap();
    }
}

fn import(src: &Path, dir: &Path, keep: Keep) -> Store {
    let o = IngestOptions {
        chunk_bytes: 16 * 1024,
        keep,
        ..Default::default()
    };
    assert_eq!(
        wait(&start_ingest(src, dir, Budget::new(256 << 20), o)),
        JobState::Done
    );
    Store::open(dir).unwrap()
}

#[test]
fn bulk_is_detected() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("logs.jsonl");
    write_bulk(&src, 50);
    let head = std::fs::read(&src).unwrap();
    assert!(matches!(
        detect_spec(&head),
        SourceSpec::Json { bulk: true, .. }
    ));
    let plain = b"{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n{\"a\":4}\n";
    assert!(matches!(
        detect_spec(plain),
        SourceSpec::Json { bulk: false, .. }
    ));
}

#[test]
fn elasticsearch_bulk_is_one_row_per_document() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("logs.jsonl");
    write_bulk(&src, 3000); // many 16 KB chunks: instruction/document pairs cross chunk edges
    let st = import(&src, &d.path().join("x.lsidx"), Keep::All);
    assert_eq!(st.row_count(), 3000);
    let col = |n: &str| {
        st.schema()
            .index_of(n)
            .unwrap_or_else(|| panic!("no column {n}"))
    };
    for n in ["@timestamp", "ip", "_index", "_id"] {
        assert_eq!(st.schema().columns[col(n)].count, 3000, "{n} on every row");
    }
    assert!(
        st.schema().index_of("index").is_none(),
        "instructions are not rows"
    );
    let c = ChunkCache::new(Budget::new(64 << 20));
    let p = page(
        &st,
        &MatchSet::all(&st),
        1234,
        1,
        &[col("_id"), col("_index"), col("ip")],
        &c,
    )
    .unwrap();
    assert_eq!(
        p.cells[0],
        vec![
            Some("doc1234".into()),
            Some("logstash-2015.05.19".into()),
            Some("10.0.0.234".into())
        ]
    );
    // the raw view shows the instruction and the document
    let raw = String::from_utf8(st.raw_line(1234).unwrap()).unwrap();
    assert!(
        raw.contains("\"_id\":\"doc1234\"") && raw.contains("@timestamp"),
        "{raw}"
    );
}

#[test]
fn bulk_rebuild_equals_import() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("logs.jsonl");
    write_bulk(&src, 3000);
    let full = import(&src, &d.path().join("f.lsidx"), Keep::All);
    let inpl = import(
        &src,
        &d.path().join("i.lsidx"),
        Keep::Only(vec!["@timestamp".into()]),
    );
    let cols: Vec<usize> = (0..full.schema().len()).collect();
    let c = ChunkCache::new(Budget::new(64 << 20));
    for start in [0u64, 1023, 2999] {
        let a = page(&full, &MatchSet::all(&full), start, 1, &cols, &c).unwrap();
        let b = page(&inpl, &MatchSet::all(&inpl), start, 1, &cols, &c).unwrap();
        assert_eq!(a.cells, b.cells, "row {start}");
    }
}
