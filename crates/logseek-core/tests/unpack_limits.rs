//! Unpacking never fills the drive: it stops with a plain message, and the
//! partial copy is removed. (The free space is faked with LOGSEEK_FAKE_FREE_MB,
//! read in debug builds; one test sets it, so the tests here run one at a time.)

use logseek_core::ingest::packed::unpack;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Mutex;

static ENV: Mutex<()> = Mutex::new(());

fn zeros_gz(p: &std::path::Path, mib: usize) {
    let mut gz = flate2::write::GzEncoder::new(
        std::fs::File::create(p).unwrap(),
        flate2::Compression::fast(),
    );
    let chunk = vec![b'a'; 1 << 20];
    for _ in 0..mib {
        gz.write_all(&chunk).unwrap();
    }
    gz.finish().unwrap();
}

#[test]
fn a_gzip_that_would_leave_the_drive_nearly_full_stops_with_a_message() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("bomb.gz");
    zeros_gz(&p, 24);
    let out = d.path().join("bomb");

    // the drive is nearly full (1 MiB free; 200 MiB are always kept)
    std::env::set_var("LOGSEEK_FAKE_FREE_MB", "1");
    let done = AtomicU64::new(0);
    let r = unpack(&p, "bomb", &out, &AtomicBool::new(false), &done);
    std::env::remove_var("LOGSEEK_FAKE_FREE_MB");
    let e = r.expect_err("should have stopped");
    assert!(e.starts_with("not enough room to unpack"), "{e}");
    assert!(!out.exists() && !out.with_extension("part").exists());
    // it stopped early, not after writing everything
    assert!(done.load(std::sync::atomic::Ordering::Relaxed) < 24 << 20);

    // plenty of room: the same file unpacks whole
    std::env::set_var("LOGSEEK_FAKE_FREE_MB", "100000");
    let r = unpack(&p, "bomb", &out, &AtomicBool::new(false), &AtomicU64::new(0));
    std::env::remove_var("LOGSEEK_FAKE_FREE_MB");
    assert_eq!(r, Ok(24 << 20));
    assert_eq!(std::fs::metadata(&out).unwrap().len(), 24 << 20);
}
