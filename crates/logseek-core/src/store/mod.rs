//! An opened index: manifest + segment readers, row access, raw lines.

pub mod manifest;
pub mod memindex;

pub use manifest::{ColumnInfo, Fingerprint, Manifest, SourcePart, SourceSpec, StoreSchema};

use crate::budget::Budget;
use crate::error::{Error, Result};
use crate::ingest::rebuild::Rebuilder;
use crate::segment::{DecodedColumn, EncodedColumn, SegmentMeta, SegmentReader};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// Hidden per-segment column holding each row's source byte offset.
pub const OFFSET_COL: u32 = u32::MAX - 1;
pub const SEG_DIR: &str = "seg";

pub struct Store {
    dir: PathBuf,
    manifest: Manifest,
    segs: Vec<SegmentReader>,
    starts: Vec<u64>,
    budget: Budget,
    /// Each source file, opened once (one entry per file of the index).
    sources: Vec<OnceLock<Option<std::fs::File>>>,
    rebuilders: Vec<OnceLock<std::result::Result<Rebuilder, String>>>,
    rebuilds: AtomicU64,
    rows_rebuilt: AtomicU64,
    mem: Option<std::sync::Arc<memindex::MemIndex>>,
}

impl Store {
    pub fn open(dir: &Path) -> Result<Store> {
        Self::open_with_budget(dir, Budget::new(64 << 20))
    }

    pub fn open_with_budget(dir: &Path, budget: Budget) -> Result<Store> {
        let mem = memindex::get(dir);
        let manifest = match &mem {
            Some(m) => m.manifest(),
            None => Manifest::load(dir)?,
        };
        if manifest.version != manifest::VERSION {
            return Err(Error::Invalid(format!(
                "this index was built by a different LogSeek version (format {} vs {}); open the source again to rebuild it",
                manifest.version,
                manifest::VERSION
            )));
        }
        let mut st = Store {
            dir: dir.to_path_buf(),
            manifest,
            segs: Vec::new(),
            starts: Vec::new(),
            budget,
            sources: Vec::new(),
            rebuilders: Vec::new(),
            rebuilds: AtomicU64::new(0),
            rows_rebuilt: AtomicU64::new(0),
            mem,
        };
        st.reset_sources();
        st.load_segments()?;
        Ok(st)
    }

    /// One slot per source file for its handle and its rebuilder.
    fn reset_sources(&mut self) {
        let n = self.manifest.parts.len().max(1);
        self.sources = (0..n).map(|_| OnceLock::new()).collect();
        self.rebuilders = (0..n).map(|_| OnceLock::new()).collect();
    }

    /// How many files this index was built from.
    pub fn file_count(&self) -> usize {
        self.manifest.parts.len().max(1)
    }

    /// Source file `part`: its path and how it is read.
    pub fn part(&self, part: u32) -> (&Path, &SourceSpec) {
        match self.manifest.parts.get(part as usize) {
            Some(p) => (&p.path, &p.spec),
            None => (&self.manifest.source, &self.manifest.spec),
        }
    }

    fn load_segments(&mut self) -> Result<()> {
        for name in self.manifest.segments.iter().skip(self.segs.len()) {
            let r = if name.starts_with(memindex::MEM_PREFIX) {
                let bytes = self
                    .mem
                    .as_ref()
                    .and_then(|m| m.segment(name))
                    .ok_or_else(|| {
                        Error::Corrupt(format!("segment {name} is no longer in memory"))
                    })?;
                SegmentReader::from_bytes(name, bytes)?
            } else {
                // a segment is a plain file in the index's own folder: a name
                // with a folder, a drive or ".." in it is not one of ours
                let plain = !name.is_empty()
                    && name != "."
                    && name != ".."
                    && !name.contains(['/', '\\', ':']);
                if !plain {
                    return Err(Error::Corrupt(format!(
                        "the index names a segment outside its folder: {name}"
                    )));
                }
                SegmentReader::open(&self.dir.join(SEG_DIR).join(name))?
            };
            self.starts.push(r.meta().row_start);
            self.segs.push(r);
        }
        Ok(())
    }

    /// Pick up segments committed since open (live import).
    pub fn refresh(&mut self) -> Result<bool> {
        // a live import may have moved to memory since this store opened
        if let Some(m) = memindex::get(&self.dir) {
            self.mem = Some(m);
        }
        let m = match &self.mem {
            Some(x) => x.manifest(),
            None => Manifest::load(&self.dir)?,
        };
        let grew = m.segments.len() > self.segs.len();
        let new_cols = m.schema.columns.len() != self.manifest.schema.columns.len();
        self.manifest = m;
        if new_cols || self.sources.len() != self.file_count() {
            // new columns appeared: rebuild with the schema that names them
            self.reset_sources();
        }
        self.load_segments()?;
        Ok(grew)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn schema(&self) -> &StoreSchema {
        &self.manifest.schema
    }
    pub fn budget(&self) -> &Budget {
        &self.budget
    }
    pub fn row_count(&self) -> u64 {
        self.segs.iter().map(|s| s.meta().rows as u64).sum()
    }
    pub fn segments(&self) -> &[SegmentReader] {
        &self.segs
    }
    pub fn segment_metas(&self) -> impl Iterator<Item = &SegmentMeta> {
        self.segs.iter().map(|s| s.meta())
    }
    pub fn is_done(&self) -> bool {
        self.manifest.state == "done"
    }

    /// Bytes used by the index on disk.
    pub fn disk_bytes(&self) -> u64 {
        let mut n = std::fs::metadata(self.dir.join(manifest::MANIFEST))
            .map(|m| m.len())
            .unwrap_or(0);
        for s in self.segs.iter().filter(|s| s.memory_bytes() == 0) {
            n += std::fs::metadata(s.path()).map(|m| m.len()).unwrap_or(0);
        }
        n
    }

    /// Bytes of the index held in memory (zero-write tier).
    pub fn memory_bytes(&self) -> u64 {
        self.segs.iter().map(|s| s.memory_bytes()).sum()
    }

    /// True when the source (any of its files) no longer matches what was indexed.
    pub fn is_stale(&self) -> bool {
        let changed = |p: &Path, want: &Fingerprint| match Fingerprint::of(p) {
            Ok(fp) => &fp != want,
            Err(_) => true,
        };
        if self.manifest.parts.is_empty() {
            return changed(&self.manifest.source, &self.manifest.fingerprint);
        }
        self.manifest
            .parts
            .iter()
            .any(|p| changed(&p.path, &p.fingerprint))
    }

    /// `(segment index, row within segment)` for a global row.
    pub fn locate(&self, row: u64) -> Option<(usize, usize)> {
        let i = match self.starts.binary_search(&row) {
            Ok(i) => i,
            Err(0) => return None,
            Err(i) => i - 1,
        };
        let m = self.segs.get(i)?.meta();
        (row < m.row_start + m.rows as u64).then(|| (i, (row - m.row_start) as usize))
    }

    /// Display text of one cell (decodes one column chunk; the query layer caches).
    pub fn cell_text(&self, row: u64, col: usize) -> Result<Option<String>> {
        let (si, r) = self
            .locate(row)
            .ok_or_else(|| Error::Invalid(format!("row {row} out of range")))?;
        let Some(c) = self.column(si, col as u32, &self.budget)? else {
            return Ok(None);
        };
        Ok(c.text(r).map(|s| s.into_owned()))
    }

    /// One column of one segment, stored or read from the source.
    pub fn column(&self, si: usize, col: u32, budget: &Budget) -> Result<Option<DecodedColumn>> {
        let seg = &self.segs[si];
        let Some(m) = seg.meta().col(col) else {
            return Ok(None);
        };
        if !m.in_source {
            return seg.column(col, budget);
        }
        let rows = seg.meta().rows;
        let cols = self.rebuild(si, 0..rows as usize)?;
        match cols.iter().find(|c| c.col_id() == col) {
            Some(c) => Ok(Some(c.decode(rows, budget)?)),
            None => Ok(None),
        }
    }

    fn rebuilder(&self, part: u32) -> Result<&Rebuilder> {
        let slot = self
            .rebuilders
            .get(part as usize)
            .ok_or_else(|| Error::Corrupt(format!("no source file {part}")))?;
        slot.get_or_init(|| Rebuilder::for_part(&self.manifest, part).map_err(|e| e.to_string()))
            .as_ref()
            .map_err(|e| Error::Invalid(e.clone()))
    }

    /// The columns of segment `si` that are read from the source, rebuilt for
    /// `rows` (row offsets are always stored, so any window can be cut exactly).
    pub fn rebuild(&self, si: usize, rows: Range<usize>) -> Result<Vec<EncodedColumn>> {
        self.rebuild_reserving(si, rows, |_| Ok(()))
    }

    /// `rebuild`, first asking `reserve` for the working memory the parse
    /// needs: four times the source bytes of exactly these rows (the bytes,
    /// the parsed cells and the column builders), held until it is done.
    pub fn rebuild_reserving<R>(
        &self,
        si: usize,
        rows: Range<usize>,
        reserve: impl FnOnce(u64) -> Result<R>,
    ) -> Result<Vec<EncodedColumn>> {
        let seg = &self.segs[si];
        let offs = seg
            .column(OFFSET_COL, &self.budget)?
            .ok_or_else(|| Error::Corrupt("segment without offsets".into()))?;
        let offsets: Vec<i64> = (0..seg.meta().rows as usize)
            .map(|r| offs.int(r).unwrap_or(0))
            .collect();
        drop(offs);
        let n = rows.len() as u64;
        let a = offsets.get(rows.start).copied().unwrap_or(0) as u64;
        let b = offsets
            .get(rows.end)
            .map_or(seg.meta().byte_end, |&o| o as u64);
        let _work = reserve(b.saturating_sub(a) * 4 + (1 << 20))?;
        let cols = self
            .rebuilder(seg.meta().source)?
            .rows(seg.meta(), &offsets, rows)?;
        self.rebuilds.fetch_add(1, Ordering::Relaxed);
        self.rows_rebuilt.fetch_add(n, Ordering::Relaxed);
        Ok(cols
            .into_iter()
            .filter(|c| seg.in_source(c.col_id()))
            .collect())
    }

    /// Segment rebuilds from the source so far (diagnostics and tests).
    pub fn rebuilds(&self) -> u64 {
        self.rebuilds.load(Ordering::Relaxed)
    }

    /// Rows re-parsed from the source so far (diagnostics and tests).
    pub fn rows_rebuilt(&self) -> u64 {
        self.rows_rebuilt.load(Ordering::Relaxed)
    }

    /// Columns whose values are read from the source in at least one segment.
    pub fn source_columns(&self) -> Vec<usize> {
        let mut out: Vec<usize> = self
            .segs
            .iter()
            .flat_map(|s| s.meta().cols.iter())
            .filter(|c| c.in_source && c.col_id != OFFSET_COL)
            .map(|c| c.col_id as usize)
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The evidence file, opened once, read-only (the first file of several).
    pub fn source_file(&self) -> Result<&std::fs::File> {
        self.source_file_of(0)
    }

    /// Source file `part`, opened once, read-only.
    pub fn source_file_of(&self, part: u32) -> Result<&std::fs::File> {
        let (path, _) = self.part(part);
        self.sources
            .get(part as usize)
            .ok_or_else(|| Error::Corrupt(format!("no source file {part}")))?
            .get_or_init(|| std::fs::File::open(path).ok())
            .as_ref()
            .ok_or_else(|| Error::Invalid(format!("source offline: {}", path.display())))
    }

    /// Source byte range of a row.
    pub fn row_span(&self, row: u64) -> Result<(u64, u64)> {
        let (si, r) = self
            .locate(row)
            .ok_or_else(|| Error::Invalid(format!("row {row} out of range")))?;
        let seg = &self.segs[si];
        let offs = seg
            .column(OFFSET_COL, &self.budget)?
            .ok_or_else(|| Error::Corrupt("segment without offsets".into()))?;
        let start = offs.int(r).unwrap_or(0) as u64;
        let end = if r + 1 < seg.meta().rows as usize {
            offs.int(r + 1).unwrap_or(0) as u64
        } else {
            seg.meta().byte_end
        };
        Ok((start, end.max(start)))
    }

    /// The exact source bytes of a row (read-only access to the evidence).
    pub fn raw_line(&self, row: u64) -> Result<Vec<u8>> {
        let (a, b) = self.row_span(row)?;
        let len = (b - a).min(16 * 1024 * 1024) as usize;
        let part = self
            .locate(row)
            .map_or(0, |(si, _)| self.segs[si].meta().source);
        let (path, spec) = self.part(part);
        let f = std::fs::File::open(path)?;
        let mut buf = vec![0u8; len];
        crate::segment::read_at(&f, &mut buf, a)?;
        // the span runs to the next record: drop trailing separators (",\n" between array elements)
        while buf
            .last()
            .is_some_and(|b| matches!(b, b'\n' | b'\r' | b',' | b' ' | b']'))
            && buf.len() > 1
        {
            if matches!(spec, SourceSpec::Csv { .. } | SourceSpec::Text { .. })
                && !matches!(buf.last(), Some(b'\n' | b'\r'))
            {
                break;
            }
            buf.pop();
        }
        Ok(buf)
    }
}
