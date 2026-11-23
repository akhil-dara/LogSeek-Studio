//! End-to-end import benchmark:
//! `cargo run --release --example ingest -- <source> <store_dir> [budget_mb]`
use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::store::Store;
use logseek_core::sysmem::process_memory;
use std::time::{Duration, Instant};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (src, dir) = (
        std::path::PathBuf::from(&a[1]),
        std::path::PathBuf::from(&a[2]),
    );
    let mb: u64 = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(512);
    let _ = std::fs::remove_dir_all(&dir);
    let budget = Budget::new(mb << 20);
    let size = std::fs::metadata(&src).unwrap().len();
    let t = Instant::now();
    let job = start_ingest(&src, &dir, budget.clone(), IngestOptions::default());
    let mut peak_private = 0u64;
    let mut first_rows: Option<f64> = None;
    loop {
        let m = process_memory();
        peak_private = peak_private.max(m.private);
        if first_rows.is_none() && job.progress().rows() > 0 {
            first_rows = Some(t.elapsed().as_secs_f64());
        }
        if !matches!(job.state(), JobState::Running) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let secs = t.elapsed().as_secs_f64();
    let st = Store::open(&dir).unwrap();
    let m = process_memory();
    println!(
        "source        {} ({:.2} GB)",
        src.display(),
        size as f64 / 1e9
    );
    println!("state         {:?}", job.state());
    println!(
        "rows          {}   columns {}",
        st.row_count(),
        st.schema().len()
    );
    println!(
        "time          {secs:.1} s   → {:.0} MB/s, {:.0} rows/s",
        size as f64 / 1e6 / secs,
        st.row_count() as f64 / secs
    );
    println!(
        "first rows    {:.2} s after start",
        first_rows.unwrap_or(secs)
    );
    println!(
        "index size    {:.2} GB = {:.2}× source",
        st.disk_bytes() as f64 / 1e9,
        st.disk_bytes() as f64 / size as f64
    );
    println!(
        "budget        {} MB cap, {:.0} MB peak reserved",
        mb,
        budget.peak() as f64 / 1048576.0
    );
    let mach = logseek_core::budget::Machine::detect();
    let auto = logseek_core::budget::Plan::for_machine(&mach, None);
    let this = logseek_core::budget::Plan::for_resources(mb << 20, mach.cores);
    println!("mode          {} at this budget; automatic would be {} ({} MB, {} cores, {:.1} GB RAM free)",
        this.mode.label(), auto.mode.label(), auto.cap >> 20, mach.cores, mach.available_ram as f64 / 1e9);
    println!(
        "process RAM   peak private {:.0} MB, peak working set {:.0} MB",
        peak_private as f64 / 1048576.0,
        m.peak_working_set as f64 / 1048576.0
    );
    let r = &st.manifest().report;
    println!("report        ragged {} overflow {} bad-quotes {} header-repeats {} decode-fallbacks {} invalid-json {}",
        r.ragged_rows, r.overflow_fields, r.bad_quote_spans.len(), r.header_repeats, r.decode_errors, r.invalid_json);
    if let Some(tc) = st.schema().time_col {
        println!("time column   {}", st.schema().columns[tc].name);
    }
}
