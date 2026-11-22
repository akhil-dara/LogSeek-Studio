//! Packed files: what is inside, and unpacking one of them.

use logseek_core::ingest::packed::{entries, is_packed, unpack, Entry};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

fn text() -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..50_000 {
        writeln!(v, "{{\"n\":{i},\"host\":\"web{}\"}}", i % 7).unwrap();
    }
    v
}

#[test]
fn a_gzip_file_holds_one_file_named_after_it_and_unpacks_whole() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("export.json.gz");
    let mut gz = flate2::write::GzEncoder::new(
        std::fs::File::create(&p).unwrap(),
        flate2::Compression::fast(),
    );
    gz.write_all(&text()).unwrap();
    gz.finish().unwrap();
    assert!(is_packed(&p) && !is_packed(Path::new("a.json")));
    assert_eq!(
        entries(&p).unwrap(),
        [Entry {
            name: "export.json".into(),
            bytes: None
        }]
    );
    let out = d.path().join("out");
    let done = AtomicU64::new(0);
    let n = unpack(&p, "export.json", &out, &AtomicBool::new(false), &done).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), text());
    assert_eq!(n, text().len() as u64);
    assert_eq!(done.load(Ordering::Relaxed), n);
}

#[test]
fn a_zip_lists_every_file_with_its_size_and_unpacks_the_one_asked_for() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("logs.zip");
    let mut z = zip::ZipWriter::new(std::fs::File::create(&p).unwrap());
    let o = zip::write::SimpleFileOptions::default();
    z.add_directory("sub/", o).unwrap();
    z.start_file("a.json", o).unwrap();
    z.write_all(&text()).unwrap();
    z.start_file("sub/b.csv", o).unwrap();
    z.write_all(b"x,y\n1,2\n").unwrap();
    z.finish().unwrap();
    assert_eq!(
        entries(&p).unwrap(),
        [
            Entry {
                name: "a.json".into(),
                bytes: Some(text().len() as u64)
            },
            Entry {
                name: "sub/b.csv".into(),
                bytes: Some(8)
            }
        ]
    );
    let out = d.path().join("b");
    let stop = AtomicBool::new(false);
    unpack(&p, "sub/b.csv", &out, &stop, &AtomicU64::new(0)).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), b"x,y\n1,2\n");
    assert!(unpack(&p, "nope", &out, &stop, &AtomicU64::new(0)).is_err());
}

#[test]
fn stopping_or_a_bad_file_leaves_nothing_behind() {
    let d = tempfile::tempdir().unwrap();
    let bad = d.path().join("bad.gz");
    std::fs::write(&bad, b"plain text").unwrap();
    assert!(entries(&bad).is_err());
    let p = d.path().join("x.gz");
    let mut gz = flate2::write::GzEncoder::new(
        std::fs::File::create(&p).unwrap(),
        flate2::Compression::fast(),
    );
    gz.write_all(&text()).unwrap();
    gz.finish().unwrap();
    let out = d.path().join("out");
    let r = unpack(&p, "x", &out, &AtomicBool::new(true), &AtomicU64::new(0));
    assert_eq!(r, Err("stopped".into()));
    assert!(!out.exists() && !out.with_extension("part").exists());
}
