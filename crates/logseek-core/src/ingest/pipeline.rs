//! The import pipeline: reader → parallel workers → ordered writer.
//!
//! * The reader cuts the source at record boundaries (quote-parity aware for
//!   CSV, newline for NDJSON, a streaming element scanner for JSON arrays and
//!   wrapper documents) and reserves each chunk's worst-case memory *before*
//!   reading it — the budget can never be exceeded and, because reservations
//!   are taken in file order, the ordered writer can always make progress.
//! * Workers parse, type and compress chunks into segment columns.
//! * The writer commits segments in order and atomically updates the manifest,
//!   so rows are queryable while the import runs, a cancelled or failed import
//!   keeps everything committed, and a restart resumes where it stopped.

use super::csv::CsvChunkParser;
use super::detect::detect_spec;
use super::infer::{
    infer_column, infer_types, parse_bool, parse_float, parse_int, pick_time_column,
    pick_time_column_with,
};
use super::json::{
    parse_json_elements, parse_json_records, ArrayElementScanner, JsonOptions, SchemaBuilder,
};
use super::report::ImportReport;
use crate::budget::{Budget, Component, Reservation};
use crate::segment::{ColumnBuilder, EncodedColumn, SegmentMeta, SegmentWriter};
use crate::store::manifest::VERSION;
use crate::store::memindex::{self, MemIndex};
use crate::store::{
    ColumnInfo, Fingerprint, Manifest, SourceSpec, StoreSchema, OFFSET_COL, SEG_DIR,
};
use crate::time::{infer_date_order, parse_time_with, DateOrder};
use crate::types::{ColType, Value};
use crossbeam_channel::{bounded, Receiver, Sender};
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// Which columns the index keeps; the others are read from the source.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Keep {
    #[default]
    All,
    /// Keep these columns (by name); row offsets are always kept.
    Only(Vec<String>),
}

impl Keep {
    fn keeps(&self, id: u32, schema: &StoreSchema) -> bool {
        match self {
            Keep::All => true,
            Keep::Only(names) => {
                id == OFFSET_COL
                    || schema
                        .columns
                        .get(id as usize)
                        .is_some_and(|c| names.contains(&c.name))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct IngestOptions {
    /// Target bytes of source per chunk.
    pub chunk_bytes: usize,
    /// Parser threads (0 = automatic).
    pub workers: usize,
    /// Fault injection for tests: fail the write after this many segments.
    pub fail_after_segments: Option<usize>,
    /// Bytes this run may add to the index (`None` = no limit). The writer
    /// stops before a segment that would cross it, leaving a usable index.
    pub disk_budget: Option<u64>,
    /// Fault injection for tests: the parser panics on this chunk index.
    pub panic_on_chunk: Option<u64>,
    /// Columns kept in the index (the rest are read from the source). With
    /// `Keep::All` and a disk budget, columns that do not fit are read from
    /// the source instead of stopping the import.
    pub keep: Keep,
    /// Fault injection for tests: after this many segments, only this many
    /// more bytes may be written (another program filled the disk).
    pub shrink_budget_after: Option<(usize, u64)>,
    /// Write nothing to disk: the index lives in memory for the session.
    /// Chosen automatically when the disk budget is below `MIN_DISK`, and for
    /// the rest of the file when the disk fills during the import.
    pub zero_write: bool,
    /// Memory an index held in RAM may use (default: a quarter of the budget).
    pub ram_index_cap: Option<u64>,
    /// Which field the timeline uses (the user's choice from "Check before opening").
    pub time: TimeChoice,
    /// Names for a file with no row of names, by place (empty: keep the
    /// numbered name), from "Check before opening". A name already taken is
    /// numbered (`client_2`). Ignored for a file that names its own columns.
    pub names: Vec<String>,
}

/// The field the timeline uses.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum TimeChoice {
    /// The import picks (a well-filled time field, else an epoch number).
    #[default]
    Auto,
    /// This field, by name; a number is read as epoch time in the unit that fits.
    Column(String),
    /// No timeline.
    Off,
}

/// Below this disk budget an index is not written at all (zero-write); above
/// it the import writes what fits and moves to memory if the disk fills.
pub const MIN_DISK: u64 = 64 << 10;

impl Default for IngestOptions {
    fn default() -> Self {
        IngestOptions {
            chunk_bytes: 8 << 20,
            workers: 0,
            fail_after_segments: None,
            disk_budget: None,
            panic_on_chunk: None,
            keep: Keep::All,
            shrink_budget_after: None,
            zero_write: false,
            ram_index_cap: None,
            time: TimeChoice::Auto,
            names: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum JobState {
    Running,
    Done,
    Cancelled,
    Partial(String),
    Failed(String),
}

#[derive(Debug)]
pub struct Progress {
    bytes_total: AtomicU64,
    bytes_done: AtomicU64,
    rows: AtomicU64,
    segments: AtomicU64,
    state: Mutex<JobState>,
}

impl Progress {
    pub fn bytes_total(&self) -> u64 {
        self.bytes_total.load(Ordering::Relaxed)
    }
    pub fn bytes_done(&self) -> u64 {
        self.bytes_done.load(Ordering::Relaxed)
    }
    pub fn rows(&self) -> u64 {
        self.rows.load(Ordering::Relaxed)
    }
    pub fn segments(&self) -> u64 {
        self.segments.load(Ordering::Relaxed)
    }
    pub fn state(&self) -> JobState {
        self.state.lock().unwrap().clone()
    }
    pub fn fraction(&self) -> f64 {
        let t = self.bytes_total();
        if t == 0 {
            0.0
        } else {
            self.bytes_done() as f64 / t as f64
        }
    }
}

pub struct IngestJob {
    progress: Arc<Progress>,
    cancel: Arc<AtomicBool>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl IngestJob {
    pub fn state(&self) -> JobState {
        self.progress.state()
    }
    pub fn progress(&self) -> &Progress {
        &self.progress
    }
    pub fn progress_arc(&self) -> Arc<Progress> {
        self.progress.clone()
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
    pub fn join(&self) {
        if let Some(h) = self.handle.lock().unwrap().take() {
            let _ = h.join();
        }
    }
}

pub fn start_ingest(src: &Path, dir: &Path, budget: Budget, opts: IngestOptions) -> IngestJob {
    start_ingest_many(&[src.to_path_buf()], dir, budget, opts)
}

/// Several files indexed as one, read one after another: columns matched by
/// name, a `_file` column naming each row's file, each segment remembering
/// its file (nothing is copied). One file: exactly `start_ingest`.
pub fn start_ingest_many(
    srcs: &[PathBuf],
    dir: &Path,
    budget: Budget,
    opts: IngestOptions,
) -> IngestJob {
    let progress = Arc::new(Progress {
        bytes_total: AtomicU64::new(0),
        bytes_done: AtomicU64::new(0),
        rows: AtomicU64::new(0),
        segments: AtomicU64::new(0),
        state: Mutex::new(JobState::Running),
    });
    let cancel = Arc::new(AtomicBool::new(false));
    let (p, c) = (progress.clone(), cancel.clone());
    let (srcs, dir) = (srcs.to_vec(), dir.to_path_buf());
    let h = std::thread::Builder::new()
        .name("ingest".into())
        .spawn(move || {
            let st = match run(&srcs, &dir, budget, opts, &p, &c) {
                Ok(s) => s,
                Err(e) => JobState::Failed(e.to_string()),
            };
            *p.state.lock().unwrap() = st;
        })
        .expect("spawn ingest");
    IngestJob {
        progress,
        cancel,
        handle: Mutex::new(Some(h)),
    }
}

// ───────────────────────── shared schema ─────────────────────────

pub(crate) struct GlobalSchema {
    cols: Vec<ColumnInfo>,
    index: HashMap<String, usize>,
}

impl GlobalSchema {
    pub(crate) fn from(s: &StoreSchema) -> Self {
        let index = s
            .columns
            .iter()
            .enumerate()
            .map(|(i, c)| (c.name.clone(), i))
            .collect();
        GlobalSchema {
            cols: s.columns.clone(),
            index,
        }
    }
    fn get_or_add(
        &mut self,
        name: &str,
        infer: impl FnOnce() -> (ColType, Option<DateOrder>),
    ) -> usize {
        if let Some(&i) = self.index.get(name) {
            return i;
        }
        let (ty, date_order) = infer();
        let i = self.cols.len();
        self.cols.push(ColumnInfo {
            name: name.to_string(),
            ty,
            count: 0,
            date_order,
        });
        self.index.insert(name.to_string(), i);
        i
    }
}

// ───────────────────────── chunks ─────────────────────────

pub(crate) enum Payload {
    Text {
        bytes: Vec<u8>,
        first: bool,
    },
    Elements {
        buf: Vec<u8>,
        elems: Vec<(u64, usize, usize)>,
        end: u64,
    },
}

struct Chunk {
    /// What the reader noticed about this chunk (merged into the chunk's report).
    rep: ImportReport,
    idx: u64,
    /// Which file (an index built from several); 0 otherwise.
    src: u32,
    base: u64,
    end: u64,
    payload: Payload,
    res: Reservation,
}

struct Built {
    idx: u64,
    src: u32,
    end: u64,
    segs: Vec<(Vec<EncodedColumn>, SegmentMeta)>,
    rep: ImportReport,
    error: Option<String>,
    /// The chunk's memory; a segment held in memory takes its share from it.
    res: Reservation,
}

/// A panic contained in one worker must not poison the shared schema for the
/// others: take the guard even if a previous holder panicked.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn reserve_wait(budget: &Budget, bytes: u64, cancel: &AtomicBool) -> Option<Reservation> {
    loop {
        if let Ok(r) = budget.try_reserve(bytes, Component::Ingest) {
            return Some(r);
        }
        if cancel.load(Ordering::SeqCst) {
            return None;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

// ───────────────────────── controller ─────────────────────────

fn run(
    srcs: &[PathBuf],
    dir: &Path,
    budget: Budget,
    opts: IngestOptions,
    progress: &Progress,
    cancel: &Arc<AtomicBool>,
) -> crate::Result<JobState> {
    let src = srcs
        .first()
        .ok_or_else(|| crate::error::Error::Invalid("no file to index".into()))?;
    let merged = srcs.len() > 1;
    let fps: Vec<Fingerprint> = srcs
        .iter()
        .map(|p| Fingerprint::of(p))
        .collect::<std::io::Result<_>>()?;
    let fp = fps[0].clone();
    let total: u64 = fps.iter().map(|f| f.size).sum();
    // each file's first byte, counted over the files read before it
    let before: Vec<u64> = fps
        .iter()
        .scan(0u64, |acc, f| {
            let at = *acc;
            *acc += f.size;
            Some(at)
        })
        .collect();
    // the index is for exactly these files, unchanged (one file: no parts)
    let same_parts = |m: &Manifest| -> bool {
        if !merged {
            return m.parts.is_empty();
        }
        m.parts.len() == srcs.len()
            && m.parts
                .iter()
                .zip(srcs)
                .zip(&fps)
                .all(|((p, s), f)| p.path == canonical(s) && &p.fingerprint == f)
    };
    progress.bytes_total.store(total, Ordering::Relaxed);
    let zero = opts.zero_write || opts.disk_budget.is_some_and(|b| b < MIN_DISK);
    // a finished index on disk for this very file wins, whatever the space now
    if let Ok(m) = Manifest::load(dir) {
        if m.fingerprint == fp && same_parts(&m) && m.version == VERSION && m.state == "done" {
            memindex::remove(dir);
            progress.rows.store(m.rows, Ordering::Relaxed);
            progress.bytes_done.store(total, Ordering::Relaxed);
            progress
                .segments
                .store(m.segments.len() as u64, Ordering::Relaxed);
            return Ok(JobState::Done);
        }
    }
    // an index held in memory this session: carry on from it only while the
    // disk still has no room; otherwise (stale, or space came back) forget it
    // and resume on disk from where the disk manifest stopped
    let mut mem: Option<Arc<MemIndex>> = None;
    // (an index of several files resumes only once it is whole: part way, it
    // starts again from the first file)
    if let Some(m) = memindex::get(dir) {
        let x = m.manifest();
        if zero
            && x.fingerprint == fp
            && same_parts(&x)
            && x.version == VERSION
            && (!merged || x.state == "done")
        {
            mem = Some(m);
        } else {
            memindex::remove(dir);
        }
    }
    let mut manifest = if let Some(m) = &mem {
        m.manifest()
    } else if zero {
        let m = fresh_manifest_for(srcs, &fps, &opts.names)?;
        let x = MemIndex::new(m.clone());
        memindex::put(dir, x.clone());
        mem = Some(x);
        m
    } else {
        std::fs::create_dir_all(dir.join(SEG_DIR))?;
        match Manifest::load(dir) {
            Ok(m)
                if m.fingerprint == fp
                    && same_parts(&m)
                    && m.version == VERSION
                    && (!merged || m.state == "done") =>
            {
                m
            }
            _ => {
                for e in std::fs::read_dir(dir.join(SEG_DIR))?.flatten() {
                    let _ = std::fs::remove_file(e.path());
                }
                fresh_manifest_for(srcs, &fps, &opts.names)?
            }
        }
    };
    // where the manifest goes: memory once the index lives there, else the disk
    let save = |m: &Manifest, mem: &Option<Arc<MemIndex>>| -> crate::Result<()> {
        match mem {
            Some(x) => {
                x.set_manifest(m);
                Ok(())
            }
            None => m.save(dir),
        }
    };
    if manifest.state == "done" {
        progress.rows.store(manifest.rows, Ordering::Relaxed);
        progress.bytes_done.store(total, Ordering::Relaxed);
        progress
            .segments
            .store(manifest.segments.len() as u64, Ordering::Relaxed);
        return Ok(JobState::Done);
    }
    manifest.state = "running".into();
    save(&manifest, &mem)?;
    progress.rows.store(manifest.rows, Ordering::Relaxed);
    progress
        .bytes_done
        .store(manifest.bytes_done, Ordering::Relaxed);

    let cap = budget.cap();
    let chunk_bytes = opts.chunk_bytes.min((cap / 12) as usize).max(16 * 1024);
    let workers = if opts.workers > 0 {
        opts.workers
    } else {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        crate::budget::Plan::for_resources(cap, cores).workers
    };

    let global = Arc::new(Mutex::new(GlobalSchema::from(&manifest.schema)));
    let (chunk_tx, chunk_rx) = bounded::<Chunk>(workers * 2);
    let (built_tx, built_rx) = bounded::<Built>(workers * 2);

    // each file with its own way of reading it (and, for several files, its
    // label for the `_file` column)
    let files: Vec<(PathBuf, SourceSpec, Option<String>)> = if merged {
        manifest
            .parts
            .iter()
            .map(|p| (p.path.clone(), p.spec.clone(), Some(p.label.clone())))
            .collect()
    } else {
        vec![(src.to_path_buf(), manifest.spec.clone(), None)]
    };
    let reader = {
        let (files, budget, cancel) = (files.clone(), budget.clone(), cancel.clone());
        let start = if merged { 0 } else { manifest.bytes_done };
        std::thread::Builder::new()
            .name("ingest-read".into())
            .spawn(move || {
                let mut idx = 0u64;
                for (i, (path, spec, _)) in files.iter().enumerate() {
                    let from = if i == 0 { start } else { 0 };
                    read_source(
                        path,
                        spec,
                        from,
                        chunk_bytes,
                        &budget,
                        &cancel,
                        &chunk_tx,
                        i as u32,
                        &mut idx,
                    )?;
                    if cancel.load(Ordering::SeqCst) {
                        break;
                    }
                }
                Ok(())
            })
            .expect("spawn reader")
    };
    let parts: Arc<Vec<(SourceSpec, Option<String>)>> =
        Arc::new(files.into_iter().map(|(_, s, l)| (s, l)).collect());
    let mut handles = Vec::new();
    let panic_on = opts.panic_on_chunk;
    for w in 0..workers {
        let (rx, tx, parts, global) = (
            chunk_rx.clone(),
            built_tx.clone(),
            parts.clone(),
            global.clone(),
        );
        handles.push(
            std::thread::Builder::new()
                .name(format!("ingest-w{w}"))
                .spawn(move || work(rx, tx, &parts, &global, panic_on))
                .expect("spawn worker"),
        );
    }
    drop(chunk_rx);
    drop(built_tx);

    // ── ordered writer ──
    let mut counts: Vec<u64> = manifest.schema.columns.iter().map(|c| c.count).collect();
    let mut pending: BTreeMap<u64, Built> = BTreeMap::new();
    let mut next = 0u64;
    let mut outcome: Option<JobState> = None;
    // bytes this run has added to the index (checked against the disk budget)
    let mut written: u64 = 0;
    let mut cap = opts.disk_budget;
    // free space as last measured on the drive, and what was written since
    let mut free_room: Option<(u64, u64)> = None;
    let source_size = total;
    let ram_cap = opts.ram_index_cap.unwrap_or(budget.cap() / 4);
    // each whole-number column's range block by block, in file order: values
    // that never go backwards from one block to the next look like time
    let mut int_spans: std::collections::HashMap<usize, Vec<(i64, i64)>> = Default::default();
    'recv: for b in built_rx.iter() {
        pending.insert(b.idx, b);
        while let Some(mut b) = pending.remove(&next) {
            next += 1;
            if let Some(e) = &b.error {
                outcome = Some(JobState::Failed(e.clone()));
                break 'recv;
            }
            let snapshot: Vec<ColumnInfo> = lock(&global).cols.clone();
            manifest.schema.columns = snapshot;
            match &opts.time {
                TimeChoice::Auto => {}
                TimeChoice::Off => {
                    manifest.schema.time_col = None;
                    manifest.schema.time_unit_us = None;
                }
                TimeChoice::Column(name) => {
                    let id = manifest.schema.columns.iter().position(|c| &c.name == name);
                    if let Some(id) = id.filter(|&id| manifest.schema.time_col != Some(id)) {
                        let unit = if manifest.schema.columns[id].ty == ColType::Int {
                            let (mut lo, mut hi) = (i64::MAX, i64::MIN);
                            for (cols, _) in &b.segs {
                                if let Some(c) = cols.iter().find(|c| c.col_id() == id as u32) {
                                    lo = lo.min(c.meta.min_i.unwrap_or(i64::MAX));
                                    hi = hi.max(c.meta.max_i.unwrap_or(i64::MIN));
                                }
                            }
                            super::infer::epoch_unit_of(lo, hi)
                        } else {
                            None
                        };
                        manifest.schema.time_col = Some(id);
                        manifest.schema.time_unit_us = unit;
                    }
                }
            }
            for (cols, _) in &b.segs {
                for c in cols {
                    if let (Some(lo), Some(hi)) = (c.meta.min_i, c.meta.max_i) {
                        int_spans
                            .entry(c.col_id() as usize)
                            .or_default()
                            .push((lo, hi));
                    }
                }
            }
            if opts.time == TimeChoice::Auto && manifest.schema.time_col.is_none() {
                let names: Vec<String> = manifest
                    .schema
                    .columns
                    .iter()
                    .map(|c| c.name.clone())
                    .collect();
                let types: Vec<ColType> = manifest.schema.columns.iter().map(|c| c.ty).collect();
                // rows filled so far, this batch included (the running totals are added
                // after this choice, so on the first batch they are still nothing)
                let mut counts: Vec<u64> =
                    manifest.schema.columns.iter().map(|c| c.count).collect();
                for (cols, meta) in &b.segs {
                    for c in cols {
                        if let Some(n) = counts.get_mut(c.col_id() as usize) {
                            *n += (meta.rows - c.meta.nulls) as u64;
                        }
                    }
                }
                // a named time column must be filled on at least half the rows;
                // otherwise a well-filled epoch column (e.g. `timestamp`) wins
                let typed = pick_time_column_with(&names, &types, &counts);
                let filled = |id: usize| -> bool {
                    let (mut n, mut r) = (0u64, 0u64);
                    for (cols, meta) in &b.segs {
                        r += meta.rows as u64;
                        if let Some(c) = cols.iter().find(|c| c.col_id() == id as u32) {
                            n += (meta.rows - c.meta.nulls) as u64;
                        }
                    }
                    n * 2 >= r
                };
                manifest.schema.time_col = typed.filter(|&t| filled(t));
                if manifest.schema.time_col.is_none() {
                    // no time-typed column well filled: a whole-number column of epoch
                    // times in ms or µs, or in seconds when its blocks run in time
                    // order (seconds alone also fit byte counts and ids), the fullest first
                    let mut best: Option<(usize, u64, i64)> = None;
                    for (cols, _) in &b.segs {
                        for c in cols {
                            let id = c.col_id() as usize;
                            let (Some(info), Some(lo), Some(hi)) =
                                (manifest.schema.columns.get(id), c.meta.min_i, c.meta.max_i)
                            else {
                                continue;
                            };
                            if info.ty != ColType::Int {
                                continue;
                            }
                            // blocks in order: each one starts at or after the one before ends
                            let spans = int_spans.get(&id).map(Vec::as_slice).unwrap_or(&[]);
                            let in_order =
                                spans.len() >= 2 && spans.windows(2).all(|w| w[0].1 <= w[1].0);
                            if let Some(unit) = super::infer::epoch_unit_of(lo, hi)
                                .filter(|&u| u < 1_000_000 || in_order)
                            {
                                let n = counts.get(id).copied().unwrap_or(0);
                                if best.is_none_or(|b| n > b.1) {
                                    best = Some((id, n, unit));
                                }
                            }
                        }
                    }
                    if let Some((id, _, unit)) = best.filter(|b| filled(b.0)) {
                        manifest.schema.time_col = Some(id);
                        manifest.schema.time_unit_us = Some(unit);
                    }
                    if manifest.schema.time_col.is_none() {
                        manifest.schema.time_col = typed;
                    }
                }
            }
            counts.resize(manifest.schema.columns.len(), 0);
            for (cols, mut meta) in std::mem::take(&mut b.segs) {
                if let Some((n, more)) = opts.shrink_budget_after {
                    if manifest.segments.len() == n {
                        cap = Some(written + more);
                    }
                }
                // re-measure the drive now and then (other programs write too)
                if mem.is_none() && manifest.segments.len() % 16 == 0 {
                    free_room = crate::sysmem::disk_free(dir)
                        .map(|f| (f.saturating_sub(crate::sysmem::DISK_SAFETY), written));
                }
                let always: Vec<u32> = std::iter::once(OFFSET_COL)
                    .chain(manifest.schema.time_col.map(|t| t as u32))
                    .collect();
                // costs: on disk every column keeps its Bloom filter; in memory a
                // column read from the source drops it (see into_source_bare)
                let bloom = |c: &EncodedColumn| c.bloom.as_ref().map_or(0, |b| b.len()) as u64;
                let fixed_for = |in_mem: bool| -> u64 {
                    4096 + cols
                        .iter()
                        .map(|c| 64 + if in_mem { 0 } else { bloom(c) })
                        .sum::<u64>()
                };
                let cost_for = |c: &EncodedColumn, in_mem: bool| -> u64 {
                    c.blob.len() as u64 + if in_mem { bloom(c) } else { 0 }
                };
                let est_for = |ids: &[u32], in_mem: bool| -> u64 {
                    fixed_for(in_mem)
                        + cols
                            .iter()
                            .filter(|c| ids.contains(&c.col_id()))
                            .map(|c| cost_for(c, in_mem))
                            .sum::<u64>()
                };
                let all_ids: Vec<u32> = cols.iter().map(|c| c.col_id()).collect();
                // a row with an unbalanced quote was read line by line here; cut
                // at a window edge it would parse differently, so this segment
                // keeps every column (rare, and exact by construction)
                let bad_quotes = b
                    .rep
                    .bad_quote_spans
                    .iter()
                    .any(|&(x, y)| x < meta.byte_end && y > meta.byte_start);
                // pick for the disk; when nothing fits there, for memory; else stop
                let mut chosen: Option<Vec<u32>> = None;
                loop {
                    let room = if let Some(m) = &mem {
                        Some(ram_cap.saturating_sub(m.bytes()))
                    } else {
                        match (cap, free_room) {
                            (Some(c), Some((f, at))) => Some(
                                c.saturating_sub(written)
                                    .min(f.saturating_sub(written - at)),
                            ),
                            (Some(c), None) => Some(c.saturating_sub(written)),
                            (None, Some((f, at))) => Some(f.saturating_sub(written - at)),
                            (None, None) => None,
                        }
                    };
                    let mut ids: Vec<u32> = match (&opts.keep, room) {
                        _ if bad_quotes => all_ids.clone(),
                        (Keep::All, Some(room)) => {
                            let list: Vec<(u32, u64)> = cols
                                .iter()
                                .map(|c| (c.col_id(), cost_for(c, mem.is_some())))
                                .collect();
                            // aim a little under the room: later segments may cost more
                            super::tiering::choose_kept(
                                &list,
                                &always,
                                fixed_for(mem.is_some()),
                                meta.byte_end.saturating_sub(meta.byte_start),
                                source_size
                                    .saturating_sub(before[b.src as usize] + meta.byte_start),
                                room / 10 * 9,
                            )
                        }
                        (Keep::All, None) => all_ids.clone(),
                        (Keep::Only(_), _) => all_ids
                            .iter()
                            .copied()
                            .filter(|&id| opts.keep.keeps(id, &manifest.schema))
                            .collect(),
                    };
                    if !bad_quotes && room.is_some_and(|r| est_for(&ids, mem.is_some()) > r) {
                        // last resort: keep only offsets and time here
                        ids.retain(|id| always.contains(id));
                    }
                    if room.is_none_or(|r| est_for(&ids, mem.is_some()) <= r) {
                        chosen = Some(ids);
                        break;
                    }
                    if mem.is_none() && ram_cap > 0 {
                        // the disk is full: the rest of the index lives in memory;
                        // the manifest on disk stays where it was, so a later
                        // session resumes from there when space is back
                        let x = MemIndex::new(manifest.clone());
                        memindex::put(dir, x.clone());
                        mem = Some(x);
                        continue;
                    }
                    break;
                }
                let Some(ids) = chosen else {
                    outcome = Some(JobState::Partial(if mem.is_some() {
                        format!(
                            "there is no room left on the disk or in memory ({} rows are ready); free some disk space, then open the file again to continue",
                            manifest.rows
                        )
                    } else {
                        format!(
                            "the disk is nearly full ({} MB indexed, {} MB kept free for the system); free some space, then open the file again to continue",
                            written >> 20,
                            crate::sysmem::DISK_SAFETY >> 20
                        )
                    }));
                    break 'recv;
                };
                let est = est_for(&ids, mem.is_some());
                let cols: Vec<EncodedColumn> = cols
                    .into_iter()
                    .map(|c| {
                        if ids.contains(&c.col_id()) {
                            c
                        } else if mem.is_some() {
                            c.into_source_bare()
                        } else {
                            c.into_source()
                        }
                    })
                    .collect();
                if opts.fail_after_segments == Some(manifest.segments.len()) {
                    outcome = Some(JobState::Partial(
                        "could not write to the index location (disk full?)".into(),
                    ));
                    break 'recv;
                }
                meta.row_start = manifest.rows;
                meta.source = b.src;
                if let Some(tc) = manifest.schema.time_col {
                    if let Some(c) = cols.iter().find(|c| c.col_id() == tc as u32) {
                        let m = &c.meta;
                        meta.time_min = m.min_i.map(|v| manifest.schema.to_us(v));
                        meta.time_max = m.max_i.map(|v| manifest.schema.to_us(v));
                    }
                }
                for c in &cols {
                    let id = c.col_id();
                    if id != OFFSET_COL && (id as usize) < counts.len() {
                        counts[id as usize] += (meta.rows - c.meta.nulls) as u64;
                    }
                }
                let rows = meta.rows as u64;
                let name = if let Some(m) = &mem {
                    let name = format!(
                        "{}{:08}.kseg",
                        memindex::MEM_PREFIX,
                        manifest.segments.len()
                    );
                    let bytes = SegmentWriter::to_bytes(&cols, meta)?;
                    // never wait for memory here: the reader may be holding it
                    // all, waiting for this chunk to be released. Take free room,
                    // else this chunk's own share (the encoded segment is far
                    // smaller than the source bytes it came from).
                    let need = bytes.len() as u64;
                    let res = match budget.try_reserve(need, Component::Cache) {
                        Ok(r) => r,
                        Err(_) => match b.res.split(need) {
                            Some(r) => r,
                            None => {
                                outcome = Some(JobState::Partial(format!(
                                    "there is no room left in memory ({} rows are ready); free some disk space, then open the file again to continue",
                                    manifest.rows
                                )));
                                break 'recv;
                            }
                        },
                    };
                    m.add(name.clone(), bytes, Some(res));
                    name
                } else {
                    let name = format!("{:08}.kseg", manifest.segments.len());
                    let path = dir.join(SEG_DIR).join(&name);
                    if let Err(e) = SegmentWriter::write(&path, &cols, meta) {
                        outcome = Some(JobState::Partial(format!(
                            "could not write to the index location: {e}"
                        )));
                        break 'recv;
                    }
                    written += std::fs::metadata(&path).map_or(est, |m| m.len());
                    name
                };
                manifest.segments.push(name);
                manifest.rows += rows;
                progress.segments.fetch_add(1, Ordering::Relaxed);
            }
            manifest.report.merge(&b.rep);
            // one file: the resume point in it; several: how far through them all
            manifest.bytes_done = before[b.src as usize] + b.end;
            for (c, n) in manifest.schema.columns.iter_mut().zip(&counts) {
                c.count = *n;
            }
            save(&manifest, &mem)?;
            progress.rows.store(manifest.rows, Ordering::Relaxed);
            progress
                .bytes_done
                .store(manifest.bytes_done, Ordering::Relaxed);
        }
        if cancel.load(Ordering::SeqCst) {
            break;
        }
    }
    let stopped_early = outcome.is_some() || cancel.load(Ordering::SeqCst);
    if stopped_early {
        cancel.store(true, Ordering::SeqCst);
    }
    drop(built_rx);
    drop(pending);
    let read_result = reader
        .join()
        .unwrap_or_else(|_| Err("reader panicked".into()));
    for h in handles {
        let _ = h.join();
    }
    let state = match (outcome, read_result) {
        (Some(s), _) => s,
        (None, Err(e)) => JobState::Failed(e),
        (None, Ok(())) if cancel.load(Ordering::SeqCst) => JobState::Cancelled,
        (None, Ok(())) => JobState::Done,
    };
    manifest.schema.columns = lock(&global).cols.clone();
    for (c, n) in manifest.schema.columns.iter_mut().zip(&counts) {
        c.count = *n;
    }
    manifest.state = match &state {
        JobState::Done => "done".into(),
        JobState::Cancelled => "cancelled".into(),
        JobState::Partial(m) => format!("partial: {m}"),
        JobState::Failed(m) => format!("failed: {m}"),
        JobState::Running => "running".into(),
    };
    save(&manifest, &mem)?;
    progress.rows.store(manifest.rows, Ordering::Relaxed);
    Ok(state)
}

/// A new manifest for these files: the first file's reading and columns,
/// and for several files every file with its own reading and its label.
fn fresh_manifest_for(
    srcs: &[PathBuf],
    fps: &[Fingerprint],
    names: &[String],
) -> crate::Result<Manifest> {
    let mut m = fresh_manifest(&srcs[0], fps[0].clone())?;
    if srcs.len() == 1 {
        give_names(&mut m, names);
    }
    if srcs.len() > 1 {
        let labels = file_labels(srcs);
        let mut parts = Vec::with_capacity(srcs.len());
        for ((p, f), label) in srcs.iter().zip(fps).zip(labels) {
            let mut head = vec![0u8; (1 << 20).min(f.size as usize)];
            std::fs::File::open(p)?.read_exact(&mut head)?;
            parts.push(crate::store::SourcePart {
                path: canonical(p),
                fingerprint: f.clone(),
                spec: detect_spec(&head),
                label,
            });
        }
        m.parts = parts;
    }
    Ok(m)
}

/// The user's names for a file with no row of names (blank: keep the
/// numbered name); a name already taken is numbered (`client_2`).
fn give_names(m: &mut Manifest, names: &[String]) {
    if names.iter().all(|n| n.trim().is_empty()) {
        return;
    }
    let header = match &mut m.spec {
        SourceSpec::Csv { opts, header } if !opts.has_header => header,
        SourceSpec::Text { header, .. } => header,
        _ => return,
    };
    let mut out: Vec<String> = Vec::with_capacity(header.len());
    for (i, old) in header.iter().enumerate() {
        let want = names.get(i).map(|n| n.trim()).filter(|n| !n.is_empty());
        let Some(want) = want else {
            out.push(old.clone());
            continue;
        };
        let taken = |n: &str, out: &[String]| {
            out.iter().any(|o| o == n) || header.iter().enumerate().any(|(j, h)| j > i && h == n)
        };
        let mut name = want.to_string();
        let mut k = 2;
        while taken(&name, &out) {
            name = format!("{want}_{k}");
            k += 1;
        }
        out.push(name);
    }
    for (c, n) in m.schema.columns.iter_mut().zip(&out) {
        c.name = n.clone();
    }
    *header = out;
}

/// What each file's rows say in the `_file` column: its name, or where two
/// names are the same, its path from the folder the files share.
pub fn file_labels(srcs: &[PathBuf]) -> Vec<String> {
    let name = |p: &PathBuf| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.display().to_string())
    };
    let names: Vec<String> = srcs.iter().map(name).collect();
    let unique = {
        let mut s = names.clone();
        s.sort();
        s.dedup();
        s.len() == names.len()
    };
    if unique {
        return names;
    }
    let full: Vec<PathBuf> = srcs.iter().map(|p| canonical(p)).collect();
    // the deepest folder every file is in
    let mut common: PathBuf = full[0].parent().map(Path::to_path_buf).unwrap_or_default();
    while !full.iter().all(|p| p.starts_with(&common)) {
        if !common.pop() {
            break;
        }
    }
    full.iter()
        .zip(names)
        .map(|(p, n)| {
            p.strip_prefix(&common)
                .map(|r| r.display().to_string())
                .unwrap_or(n)
        })
        .collect()
}

fn fresh_manifest(src: &Path, fp: Fingerprint) -> crate::Result<Manifest> {
    let mut head = vec![0u8; (1 << 20).min(fp.size as usize)];
    std::fs::File::open(src)?.read_exact(&mut head)?;
    let spec = detect_spec(&head);
    let mut schema = StoreSchema::default();
    // the head may end mid-record: parse only up to its last newline
    let cut = head
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(head.len(), |i| i + 1);
    let mut rep = ImportReport::default();
    let fixed = match &spec {
        SourceSpec::Csv { opts, header } => {
            let p = CsvChunkParser::new(*opts, header.len());
            Some((
                header.clone(),
                p.parse(
                    &head[..cut],
                    0,
                    opts.has_header,
                    Some(header.as_slice()),
                    &mut rep,
                ),
            ))
        }
        SourceSpec::Text { parts, header } => Some((
            header.clone(),
            super::textlayout::parse_chunk(&head[..cut], 0, parts, header.len(), &mut rep),
        )),
        SourceSpec::Json { .. } => None,
    };
    if let Some((header, batches)) = &fixed {
        let types = batches
            .first()
            .map(infer_types)
            .unwrap_or_else(|| vec![ColType::Text; header.len()]);
        for (i, name) in header.iter().enumerate() {
            let ty = types.get(i).copied().unwrap_or(ColType::Text);
            let date_order = (ty == ColType::Time).then(|| {
                let b = &batches[0];
                let v: Vec<&str> = (0..b.rows().min(2000))
                    .filter_map(|r| b.field(r, i))
                    .collect();
                infer_date_order(&v)
            });
            schema.columns.push(ColumnInfo {
                name: name.clone(),
                ty,
                count: 0,
                date_order,
            });
        }
        let names: Vec<String> = schema.columns.iter().map(|c| c.name.clone()).collect();
        let tys: Vec<ColType> = schema.columns.iter().map(|c| c.ty).collect();
        schema.time_col = pick_time_column(&names, &tys);
    }
    Ok(Manifest {
        version: VERSION,
        source: canonical(src),
        fingerprint: fp,
        spec,
        schema,
        rows: 0,
        bytes_done: 0,
        segments: Vec::new(),
        report: ImportReport::default(),
        state: "running".into(),
        parts: Vec::new(),
    })
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

// ───────────────────────── reader ─────────────────────────

#[allow(clippy::too_many_arguments)]
fn read_source(
    src: &Path,
    spec: &SourceSpec,
    start: u64,
    chunk_bytes: usize,
    budget: &Budget,
    cancel: &AtomicBool,
    tx: &Sender<Chunk>,
    part: u32,
    idx: &mut u64,
) -> Result<(), String> {
    let mut f = std::fs::File::open(src).map_err(|e| e.to_string())?;
    match spec {
        SourceSpec::Json {
            record_path,
            array,
            stream,
            ..
        } if record_path.is_some() || *array || *stream => {
            let sc = if *stream {
                ArrayElementScanner::stream()
            } else {
                ArrayElementScanner::new(record_path.clone())
            };
            read_elements(
                &mut f,
                sc,
                start,
                chunk_bytes,
                budget,
                cancel,
                tx,
                part,
                idx,
            )
        }
        _ => {
            let csv = match spec {
                SourceSpec::Csv { opts, .. } => Some((opts.delimiter, opts.quote)),
                _ => None,
            };
            let bulk = matches!(spec, SourceSpec::Json { bulk: true, .. });
            // one JSON record a line, where a record may still be written over
            // several lines: a chunk ends where a record starts
            let json_lines = matches!(spec, SourceSpec::Json { .. }) && !bulk;
            f.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
            let mut base = start;
            let mut carry: Vec<u8> = Vec::new();
            let mut eof = false;
            // records left out because one alone is longer than can be held:
            // said with the next chunk that goes out
            let mut left_out: Vec<(u64, u64)> = Vec::new();
            while !eof || !carry.is_empty() || !left_out.is_empty() {
                if cancel.load(Ordering::SeqCst) {
                    return Ok(());
                }
                let Some(mut res) = reserve_wait(budget, chunk_bytes as u64 * 6, cancel) else {
                    return Ok(());
                };
                let mut buf = std::mem::take(&mut carry);
                // set when the edge is a cut through a record too long to hold whole
                let mut cut_overlong = false;
                let boundary = loop {
                    if !eof {
                        // past a chunk's size the buffer grows by half each time,
                        // so a long record is looked through a few times, not
                        // once for every small read
                        let want = if buf.len() >= chunk_bytes {
                            (buf.len() / 2).max(64 * 1024)
                        } else {
                            chunk_bytes - buf.len()
                        };
                        let old = buf.len();
                        buf.resize(old + want, 0);
                        let n = read_full(&mut f, &mut buf[old..]).map_err(|e| e.to_string())?;
                        buf.truncate(old + n);
                        if n < want {
                            eof = true;
                        }
                    }
                    if eof {
                        break buf.len();
                    }
                    if let Some(b) = last_boundary(&buf, csv) {
                        let b = if bulk {
                            bulk_boundary(&buf, b)
                        } else if json_lines {
                            record_boundary(&buf, b)
                        } else {
                            b
                        };
                        if b > 0 {
                            break b;
                        }
                    }
                    // one record longer than the buffer: read more (budget permitting)
                    let need = buf.len() as u64 * 6;
                    if need > budget.ceiling() {
                        // it can never fit, however long we wait: cut it at its
                        // last line end (the CSV reader then reads that part line
                        // by line, as it does for an unbalanced quote), say so in
                        // the report, and go on; with no line end at all there is
                        // nothing to cut at, so stop and say where
                        match buf.iter().rposition(|&c| c == b'\n') {
                            Some(i) => {
                                cut_overlong = true;
                                break i + 1;
                            }
                            None => {
                                // one record with no line end in all that can
                                // be held: it is left out. The file is read on
                                // to the end of that line holding nothing, the
                                // place is noted for the report, and what
                                // follows is read as usual.
                                let mut gone = buf.len() as u64;
                                buf = Vec::new();
                                let mut block = vec![0u8; 1 << 20];
                                while !eof {
                                    if cancel.load(Ordering::SeqCst) {
                                        return Ok(());
                                    }
                                    let n = read_full(&mut f, &mut block).map_err(|e| e.to_string())?;
                                    if n < block.len() {
                                        eof = true;
                                    }
                                    match block[..n].iter().position(|&c| c == b'\n') {
                                        Some(i) => {
                                            gone += i as u64 + 1;
                                            buf = block[i + 1..n].to_vec();
                                            break;
                                        }
                                        None => gone += n as u64,
                                    }
                                }
                                left_out.push((base, base + gone));
                                base += gone;
                                if eof {
                                    break buf.len();
                                }
                                continue;
                            }
                        }
                    }
                    if need > res.bytes() {
                        let extra = need - res.bytes();
                        while res.grow(extra).is_err() {
                            if cancel.load(Ordering::SeqCst) {
                                return Ok(());
                            }
                            std::thread::sleep(Duration::from_millis(2));
                        }
                    }
                };
                if boundary == 0 && buf.is_empty() && left_out.is_empty() {
                    break;
                }
                carry = buf.split_off(boundary);
                let end = base + boundary as u64;
                let mut rep = ImportReport::default();
                rep.unreadable_spans.append(&mut left_out);
                if cut_overlong {
                    rep.unreadable_spans.push((base, end));
                }
                let chunk = Chunk {
                    rep,
                    idx: *idx,
                    src: part,
                    base,
                    end,
                    payload: Payload::Text {
                        bytes: buf,
                        first: base == 0,
                    },
                    res,
                };
                if tx.send(chunk).is_err() {
                    return Ok(());
                }
                *idx += 1;
                base = end;
                if eof && carry.is_empty() {
                    break;
                }
            }
            Ok(())
        }
    }
}

/// A chunk edge at or before `b` (just after a line end) where a JSON record
/// starts: a line that opens with a bracket at its very start. A record
/// written over several lines has its inner lines indented and ends with a
/// closing bracket, so neither is taken for a start. Lines are looked at
/// from the end backwards, over the last 4 MiB at most; when none of them
/// starts a record, the edge stays where it was.
fn record_boundary(buf: &[u8], b: usize) -> usize {
    let floor = b.saturating_sub(4 << 20);
    let mut at = b;
    // at the very end of what is held the next line is not there to look at:
    // the line before says it instead. A record on one line (opens and closes
    // at the line's edges), or the closing bracket of a longer one at the
    // start of its line, ends a record, so the edge is good as it is. This
    // keeps a file of long one-line records from being held two at a time.
    if b == buf.len() && b > 0 {
        let line_start = buf[..b - 1]
            .iter()
            .rposition(|&c| c == b'\n')
            .map_or(0, |i| i + 1);
        let line = buf[line_start..b].trim_ascii_end();
        let whole = matches!(line.first(), Some(b'{' | b'['))
            && matches!(line.last(), Some(b'}' | b']'));
        let closes = matches!(line.first(), Some(b'}' | b']'));
        if whole || closes {
            return b;
        }
    }
    loop {
        // the line that starts at `at`: known to start a record?
        if at < buf.len() && matches!(buf[at], b'{' | b'[') {
            return at;
        }
        if at <= floor.max(1) {
            return b;
        }
        // the start of the line before it
        match buf[..at - 1].iter().rposition(|&c| c == b'\n') {
            Some(i) => at = i + 1,
            None => return b,
        }
    }
}

/// A chunk edge at `b` (just after a line end) that leaves no bulk
/// instruction at the end of the chunk without its document.
fn bulk_boundary(buf: &[u8], b: usize) -> usize {
    let body = &buf[..b];
    let end = body.len().saturating_sub(1); // the '\n' at b - 1
    let start = body[..end]
        .iter()
        .rposition(|&c| c == b'\n')
        .map_or(0, |i| i + 1);
    if super::json::bulk_action(&body[start..end]).is_some() {
        start
    } else {
        b
    }
}

fn read_full(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Position just after the last record terminator. For CSV (`Some((delimiter,
/// quote))`) a newline only ends a record outside a quoted field, found the way
/// csv-core finds it: a quote opens a quoted field only at the start of a
/// field; inside one, a doubled quote is a literal quote and a single quote
/// closes it; anywhere else a quote is ordinary text.
fn last_boundary(buf: &[u8], csv: Option<(u8, u8)>) -> Option<usize> {
    #[derive(Clone, Copy, PartialEq)]
    enum S {
        /// at the start of a field
        Start,
        /// inside an unquoted field (or after a closing quote and more text)
        Field,
        /// inside a quoted field
        Quoted,
        /// a quote seen inside a quoted field: closes it, or doubles when
        /// another follows
        QuoteSeen,
    }
    let Some((delim, quote)) = csv else {
        return buf.iter().rposition(|&b| b == b'\n').map(|i| i + 1);
    };
    let mut s = S::Start;
    let mut last = None;
    for (i, &b) in buf.iter().enumerate() {
        s = match s {
            S::Quoted => {
                if b == quote {
                    S::QuoteSeen
                } else {
                    S::Quoted
                }
            }
            S::QuoteSeen if b == quote => S::Quoted,
            S::Start | S::Field | S::QuoteSeen => {
                if b == b'\n' {
                    last = Some(i + 1);
                    S::Start
                } else if b == b'\r' || b == delim {
                    S::Start
                } else if s == S::Start && b == quote {
                    S::Quoted
                } else {
                    S::Field
                }
            }
        };
    }
    last
}

#[allow(clippy::too_many_arguments)]
fn read_elements(
    f: &mut std::fs::File,
    mut sc: ArrayElementScanner,
    resume_from: u64,
    chunk_bytes: usize,
    budget: &Budget,
    cancel: &AtomicBool,
    tx: &Sender<Chunk>,
    part: u32,
    idx: &mut u64,
) -> Result<(), String> {
    let mut scan_buf = Vec::new();
    let mut block = vec![0u8; 1 << 20];
    // one element may be as long as one line may be (a sixth of the memory
    // there is for the import): a longer one is left out and noted
    sc.set_max_element((budget.ceiling() / 6).max(64 * 1024) as usize);
    let mut left_out: Vec<(u64, u64)> = Vec::new();
    let mut cur: Option<(Vec<u8>, Vec<(u64, usize, usize)>, Reservation)> = None;
    let mut last_end = resume_from;
    let mut chunk_base = resume_from;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Ok(());
        }
        let n = read_full(f, &mut block).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        let mut ready: Vec<(u64, Vec<u8>)> = Vec::new();
        sc.feed_with_offsets(&block[..n], &mut scan_buf, |off, el| {
            if off >= resume_from {
                ready.push((off, el.to_vec()));
            }
        });
        left_out.extend(sc.take_skipped().into_iter().filter(|(a, _)| *a >= resume_from));
        for (off, el) in ready {
            if cur.is_none() {
                let Some(res) = reserve_wait(budget, chunk_bytes as u64 * 6, cancel) else {
                    return Ok(());
                };
                cur = Some((Vec::with_capacity(chunk_bytes), Vec::new(), res));
                chunk_base = off;
            }
            let (buf, elems, _) = cur.as_mut().unwrap();
            let a = buf.len();
            buf.extend_from_slice(&el);
            elems.push((off, a, buf.len()));
            last_end = off + el.len() as u64;
            if buf.len() >= chunk_bytes {
                let (buf, elems, res) = cur.take().unwrap();
                let c = Chunk {
                    rep: ImportReport::default(),
                    idx: *idx,
                    src: part,
                    base: chunk_base,
                    end: last_end,
                    payload: Payload::Elements {
                        buf,
                        elems,
                        end: last_end,
                    },
                    res,
                };
                if tx.send(c).is_err() {
                    return Ok(());
                }
                *idx += 1;
            }
        }
        // after the array of records closes the rest of the file is still read:
        // the scanner counts what it holds, so the report can say what was skipped
    }
    if let Some((buf, elems, res)) = cur.take() {
        let c = Chunk {
            rep: ImportReport::default(),
            idx: *idx,
            src: part,
            base: chunk_base,
            end: last_end,
            payload: Payload::Elements {
                buf,
                elems,
                end: last_end,
            },
            res,
        };
        if tx.send(c).is_err() {
            return Ok(());
        }
        *idx += 1;
    }
    // what was not read: bytes after the array of records, a last record the
    // file ends in the middle of
    let mut rep = ImportReport {
        unread_after_records: sc.unread_after_array(),
        unreadable_spans: left_out,
        ..ImportReport::default()
    };
    if let Some((a, b)) = sc.unfinished() {
        rep.unfinished_record_bytes = b - a;
    }
    if !rep.is_clean() {
        let Some(res) = reserve_wait(budget, 4096, cancel) else {
            return Ok(());
        };
        let c = Chunk {
            rep,
            idx: *idx,
            src: part,
            base: last_end,
            end: last_end,
            payload: Payload::Elements {
                buf: Vec::new(),
                elems: Vec::new(),
                end: last_end,
            },
            res,
        };
        if tx.send(c).is_err() {
            return Ok(());
        }
        *idx += 1;
    }
    Ok(())
}

// ───────────────────────── workers ─────────────────────────

fn work(
    rx: Receiver<Chunk>,
    tx: Sender<Built>,
    parts: &[(SourceSpec, Option<String>)],
    global: &Mutex<GlobalSchema>,
    panic_on: Option<u64>,
) {
    for chunk in rx.iter() {
        // A parser bug on hostile input must cost one chunk, not the import:
        // contain the panic, list the byte range, keep going.
        let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if panic_on == Some(chunk.idx) {
                panic!("injected parser fault on chunk {}", chunk.idx);
            }
            let (spec, file) = &parts[chunk.src as usize];
            parse_chunk(&chunk, spec, global, file.as_deref())
        }));
        let (segs, mut rep) = match parsed {
            Ok(v) => v,
            Err(_) => {
                let rep = ImportReport {
                    unreadable_spans: vec![(chunk.base, chunk.end)],
                    ..ImportReport::default()
                };
                (Vec::new(), rep)
            }
        };
        rep.merge(&chunk.rep);
        let built = Built {
            idx: chunk.idx,
            src: chunk.src,
            end: chunk.end,
            segs,
            rep,
            error: None,
            res: chunk.res,
        };
        if tx.send(built).is_err() {
            return;
        }
    }
}

fn parse_chunk(
    chunk: &Chunk,
    spec: &SourceSpec,
    global: &Mutex<GlobalSchema>,
    file: Option<&str>,
) -> (Vec<(Vec<EncodedColumn>, SegmentMeta)>, ImportReport) {
    parse_payload(&chunk.payload, chunk.base, spec, global, file)
}

/// Parse source bytes into segments (the import, and rebuilding a segment
/// from the source, share this so both give identical columns).
pub(crate) fn parse_payload(
    payload: &Payload,
    base: u64,
    spec: &SourceSpec,
    global: &Mutex<GlobalSchema>,
    file: Option<&str>,
) -> (Vec<(Vec<EncodedColumn>, SegmentMeta)>, ImportReport) {
    let mut rep = ImportReport::default();
    let mut local = SchemaBuilder::default();
    let batches = match (payload, spec) {
        (Payload::Text { bytes, first }, SourceSpec::Csv { opts, header }) => {
            let p = CsvChunkParser::new(*opts, header.len());
            for h in header {
                local.column_index(h);
            }
            p.parse(
                bytes,
                base,
                *first && opts.has_header,
                Some(header.as_slice()),
                &mut rep,
            )
        }
        (Payload::Text { bytes, .. }, SourceSpec::Text { parts, header }) => {
            for h in header {
                local.column_index(h);
            }
            super::textlayout::parse_chunk(bytes, base, parts, header.len(), &mut rep)
        }
        (Payload::Text { bytes, .. }, SourceSpec::Json { bulk, .. }) => {
            // these chunks are cut at line ends: one record per line, whatever a
            // line starts with
            let o = JsonOptions {
                bulk: *bulk,
                line_oriented: true,
                ..JsonOptions::default()
            };
            parse_json_records(bytes, base, &mut local, &o, &mut rep)
        }
        (Payload::Elements { buf, elems, end }, _) => {
            parse_json_elements(buf, elems, *end, &mut local, &mut rep)
        }
    };
    let segs = batches
        .iter()
        .map(|b| build_segment(b, &local, global, file))
        .collect();
    (segs, rep)
}

fn typed<'a>(ty: ColType, order: DateOrder, s: &'a str) -> Value<'a> {
    match ty {
        ColType::Text => Value::Text(s),
        ColType::Int => parse_int(s).map(Value::Int).unwrap_or(Value::Text(s)),
        ColType::Float => parse_float(s).map(Value::Float).unwrap_or(Value::Text(s)),
        ColType::Bool => parse_bool(s).map(Value::Bool).unwrap_or(Value::Text(s)),
        ColType::Time => parse_time_with(s, order)
            .map(Value::Time)
            .unwrap_or(Value::Text(s)),
    }
}

/// The column naming each row's file, in an index built from several files.
pub const FILE_COLUMN: &str = "_file";
/// The column holding the fields past a row's header width.
pub const OVERFLOW_COLUMN: &str = "_overflow";
/// Column names the app uses itself; a field of the source with one of these
/// names is renamed (`_file_2`) so the two never share a column.
pub const RESERVED_COLUMNS: [&str; 2] = [FILE_COLUMN, OVERFLOW_COLUMN];

fn build_segment(
    b: &super::batch::RecordBatch,
    local: &SchemaBuilder,
    global: &Mutex<GlobalSchema>,
    file: Option<&str>,
) -> (Vec<EncodedColumn>, SegmentMeta) {
    let rows = b.rows();
    // local column → (global id, type, date order)
    let mut map: Vec<Option<(u32, ColType, DateOrder)>> = vec![None; local.len()];
    let mut overflow: Option<(u32, ColType, DateOrder)> = None;
    let mut file_col: Option<u32> = None;
    {
        let mut g = lock(global);
        if file.is_some() {
            file_col = Some(g.get_or_add(FILE_COLUMN, || (ColType::Text, None)) as u32);
        }
        for (li, name) in local.names().iter().enumerate() {
            let gi = g.get_or_add(name, || {
                let v: Vec<&str> = (0..rows.min(2000)).filter_map(|r| b.field(r, li)).collect();
                let ty = infer_column(&v);
                let order = (ty == ColType::Time).then(|| infer_date_order(&v));
                (ty, order)
            });
            let c = &g.cols[gi];
            map[li] = Some((gi as u32, c.ty, c.date_order.unwrap_or_default()));
        }
        if b.has_overflow() {
            let gi = g.get_or_add(OVERFLOW_COLUMN, || (ColType::Text, None));
            overflow = Some((gi as u32, ColType::Text, DateOrder::Mdy));
        }
    }
    let mut builders: Vec<Option<ColumnBuilder>> = (0..local.len()).map(|_| None).collect();
    let mut filled: Vec<usize> = vec![0; local.len()];
    let mut ov = overflow.map(|(id, ty, _)| ColumnBuilder::new(id, ty));
    let mut offs = ColumnBuilder::new(OFFSET_COL, ColType::Int);
    for r in 0..rows {
        offs.push(Value::Int(b.offset(r) as i64));
        for (c, text) in b.row_cells(r) {
            let Some((gid, ty, order)) = map.get(c).copied().flatten() else {
                continue;
            };
            let bl = builders[c]
                .get_or_insert_with(|| ColumnBuilder::new(gid, ty).with_date_order(order));
            while filled[c] < r {
                bl.push(Value::Null);
                filled[c] += 1;
            }
            bl.push_with_raw(typed(ty, order, text), text);
            filled[c] += 1;
        }
        if let Some(o) = ov.as_mut() {
            match b.overflow(r) {
                Some(t) => o.push(Value::Text(t)),
                None => o.push(Value::Null),
            }
        }
    }
    let mut cols = Vec::with_capacity(local.len() + 2);
    for (c, bl) in builders.into_iter().enumerate() {
        if let Some(mut bl) = bl {
            while filled[c] < rows {
                bl.push(Value::Null);
                filled[c] += 1;
            }
            cols.push(bl.finish());
        }
    }
    if let Some(o) = ov {
        cols.push(o.finish());
    }
    if let (Some(id), Some(f)) = (file_col, file) {
        let mut bl = ColumnBuilder::new(id, ColType::Text);
        for _ in 0..rows {
            bl.push(Value::Text(f));
        }
        cols.push(bl.finish());
    }
    cols.push(offs.finish());
    let start = if rows > 0 {
        b.offset(0)
    } else {
        b.end_offset()
    };
    (
        cols,
        SegmentMeta::new(0, rows as u32, start, b.end_offset()),
    )
}
