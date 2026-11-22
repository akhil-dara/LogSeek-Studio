//! Windows event logs: every event becomes one line of JSON.

use logseek_core::ingest::evtx::{evtx_to_jsonl, is_evtx};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[test]
fn only_evtx_files_are_event_logs() {
    assert!(is_evtx(Path::new("C:/x/Security.EVTX")));
    assert!(!is_evtx(Path::new("C:/x/Security.csv")));
}

#[test]
fn a_file_that_is_not_an_event_log_says_so_and_leaves_nothing_behind() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("bad.evtx");
    std::fs::write(&p, b"not an event log").unwrap();
    let out = d.path().join("events");
    let r = evtx_to_jsonl(&p, &out, &AtomicBool::new(false), &AtomicU64::new(0));
    assert!(r.is_err());
    assert!(!out.exists() && !out.with_extension("part").exists());
}

/// Runs on a real log when LOGSEEK_EVTX_SAMPLE names one (no event log is
/// kept in the repository).
#[test]
fn a_real_log_becomes_one_json_object_per_event() {
    let Ok(sample) = std::env::var("LOGSEEK_EVTX_SAMPLE") else {
        return;
    };
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("events");
    let done = AtomicU64::new(0);
    let r = evtx_to_jsonl(Path::new(&sample), &out, &AtomicBool::new(false), &done).unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(r.events > 0);
    assert_eq!(text.lines().count() as u64, r.events);
    assert_eq!(done.load(Ordering::Relaxed), r.events);
    for line in text.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(v["EventID"].is_number() && v["TimeCreated.SystemTime"].is_string());
    }
}
