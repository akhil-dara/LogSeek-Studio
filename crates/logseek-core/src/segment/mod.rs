//! Immutable columnar segment files (`*.kseg`).
//!
//! Layout: `[column blobs][bloom blobs][header JSON][u64 header_len]["KSEG1END"]`.
//! Opening a segment reads only the footer + header; each column is read and
//! decoded on demand, so a query touching 2 of 120 columns reads 2 blobs.

pub mod bloom;
mod codec;
pub mod format;

use crate::budget::{Budget, Component, Reservation};
use crate::error::{Error, Result};
use crate::types::{ColType, Value};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub use codec::{ColData, DecodedColumn, Encoding};
pub use format::{BoolCase, TimeTemplate};

/// How a typed column was written in the source (for byte-exact rendering).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ColFormat {
    Time(TimeTemplate),
    Bool(BoolCase),
}

const MAGIC_END: &[u8; 8] = b"KSEG1END";
/// Dictionary encoding is used while distinct values stay at or below this.
pub const DICT_MAX: usize = 65_536;

// ───────────────────────────── metadata ─────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ColMeta {
    pub col_id: u32,
    pub ty: ColType,
    pub enc: Encoding,
    pub nulls: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_i: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_i: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_f: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_f: Option<f64>,
    /// Exact distinct count for dictionary-encoded columns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dict_len: Option<u32>,
    /// Values that failed the column type and were kept as text.
    #[serde(default)]
    pub side: u32,
    /// Rows whose source spelling differs from the column format (kept verbatim).
    #[serde(default)]
    pub ex: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt: Option<ColFormat>,
    pub(crate) off: u64,
    pub(crate) len: u64,
    pub(crate) raw_len: u64,
    #[serde(default)]
    pub(crate) bloom_off: u64,
    #[serde(default)]
    pub(crate) bloom_len: u64,
    /// The values are not stored: they are rebuilt from the source on demand
    /// (zone maps, counts and the Bloom filter above are still kept).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_source: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SegmentMeta {
    pub row_start: u64,
    pub rows: u32,
    pub byte_start: u64,
    pub byte_end: u64,
    #[serde(default)]
    pub time_min: Option<i64>,
    #[serde(default)]
    pub time_max: Option<i64>,
    /// Sorted by `col_id`; only columns present in this segment.
    #[serde(default)]
    pub cols: Vec<ColMeta>,
    /// Which file of an index built from several this segment came from
    /// (`Manifest::parts`); 0 for an index of one file.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub source: u32,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

impl ColMeta {
    /// Bytes this column takes in the segment file (values + Bloom filter).
    pub fn stored_len(&self) -> u64 {
        self.len + self.bloom_len
    }
}

impl SegmentMeta {
    pub fn new(row_start: u64, rows: u32, byte_start: u64, byte_end: u64) -> Self {
        SegmentMeta {
            row_start,
            rows,
            byte_start,
            byte_end,
            time_min: None,
            time_max: None,
            cols: Vec::new(),
            source: 0,
        }
    }
    pub fn with_time(mut self, min: Option<i64>, max: Option<i64>) -> Self {
        self.time_min = min;
        self.time_max = max;
        self
    }
    pub fn col(&self, col_id: u32) -> Option<&ColMeta> {
        self.cols
            .binary_search_by_key(&col_id, |c| c.col_id)
            .ok()
            .map(|i| &self.cols[i])
    }
    pub fn has_col(&self, col_id: u32) -> bool {
        self.col(col_id).is_some()
    }
}

// ───────────────────────────── builder ─────────────────────────────

/// Accumulates one column of one segment.
pub struct ColumnBuilder {
    pub(crate) col_id: u32,
    pub(crate) ty: ColType,
    pub(crate) rows: u32,
    pub(crate) nulls: Vec<u64>,
    pub(crate) null_count: u32,
    pub(crate) bytes: Vec<u8>,
    pub(crate) offsets: Vec<u32>,
    pub(crate) dict: Option<HashMap<Box<str>, u32>>,
    pub(crate) codes: Vec<u32>,
    pub(crate) ints: Vec<i64>,
    pub(crate) floats: Vec<f64>,
    pub(crate) bools: Vec<u64>,
    pub(crate) side: Vec<(u32, Box<str>)>,
    pub(crate) min_i: Option<i64>,
    pub(crate) max_i: Option<i64>,
    pub(crate) min_f: Option<f64>,
    pub(crate) max_f: Option<f64>,
    pub(crate) date_order: crate::time::DateOrder,
    pub(crate) tmpl: Option<TimeTemplate>,
    pub(crate) tmpl_tried: bool,
    /// Float columns: decimal places per row (255 = written non-canonically).
    pub(crate) decs: Vec<u8>,
    /// Float columns: exact decimal mantissa per row (value = m / 10^decs).
    pub(crate) dec_m: Vec<i64>,
    /// Float rows written non-canonically: their numeric value.
    pub(crate) ex_num: Vec<(u32, f64)>,
    pub(crate) bool_case: Option<BoolCase>,
    /// Rows whose exact source text cannot be re-rendered: kept verbatim.
    pub(crate) raw_ex: Vec<(u32, Box<str>)>,
}

impl ColumnBuilder {
    pub fn new(col_id: u32, ty: ColType) -> Self {
        ColumnBuilder {
            col_id,
            ty,
            rows: 0,
            nulls: Vec::new(),
            null_count: 0,
            bytes: Vec::new(),
            offsets: vec![0],
            dict: if ty == ColType::Text {
                Some(HashMap::new())
            } else {
                None
            },
            codes: Vec::new(),
            ints: Vec::new(),
            floats: Vec::new(),
            bools: Vec::new(),
            side: Vec::new(),
            min_i: None,
            max_i: None,
            min_f: None,
            max_f: None,
            date_order: crate::time::DateOrder::Mdy,
            tmpl: None,
            tmpl_tried: false,
            decs: Vec::new(),
            dec_m: Vec::new(),
            ex_num: Vec::new(),
            bool_case: None,
            raw_ex: Vec::new(),
        }
    }

    pub fn with_date_order(mut self, o: crate::time::DateOrder) -> Self {
        self.date_order = o;
        self
    }

    pub fn col_id(&self) -> u32 {
        self.col_id
    }
    pub fn ty(&self) -> ColType {
        self.ty
    }
    pub fn rows(&self) -> u32 {
        self.rows
    }

    /// Approximate heap bytes held (for budget accounting by callers).
    pub fn heap_bytes(&self) -> usize {
        self.bytes.capacity()
            + self.offsets.capacity() * 4
            + self.codes.capacity() * 4
            + self.ints.capacity() * 8
            + self.floats.capacity() * 8
            + self.bools.capacity() * 8
            + self.nulls.capacity() * 8
            + self.dict.as_ref().map_or(0, |d| d.len() * 48)
            + self.side.iter().map(|(_, s)| s.len() + 24).sum::<usize>()
            + self.decs.capacity()
            + self.raw_ex.iter().map(|(_, s)| s.len() + 24).sum::<usize>()
    }

    fn set_bit(v: &mut Vec<u64>, row: u32) {
        let w = (row / 64) as usize;
        if v.len() <= w {
            v.resize(w + 1, 0);
        }
        v[w] |= 1 << (row % 64);
    }

    fn push_text(&mut self, s: &str) {
        if let Some(d) = self.dict.as_mut() {
            let next = d.len() as u32;
            let code = match d.get(s) {
                Some(&c) => c,
                None => {
                    d.insert(s.into(), next);
                    next
                }
            };
            if d.len() > DICT_MAX {
                self.dict = None;
                self.codes = Vec::new();
            } else {
                self.codes.push(code);
            }
        }
        self.bytes.extend_from_slice(s.as_bytes());
        self.offsets.push(self.bytes.len() as u32);
    }

    fn push_filler(&mut self) {
        match self.ty {
            ColType::Text => {
                if self.dict.is_some() {
                    self.codes.push(u32::MAX);
                }
                self.offsets.push(self.bytes.len() as u32);
            }
            ColType::Int | ColType::Time => {
                let last = *self.ints.last().unwrap_or(&0);
                self.ints.push(last)
            }
            ColType::Float => self.floats.push(0.0),
            ColType::Bool => {}
        }
    }

    fn note_i(&mut self, v: i64) {
        self.min_i = Some(self.min_i.map_or(v, |m| m.min(v)));
        self.max_i = Some(self.max_i.map_or(v, |m| m.max(v)));
    }

    fn note_f(&mut self, v: f64) {
        if v.is_finite() {
            self.min_f = Some(self.min_f.map_or(v, |m| m.min(v)));
            self.max_f = Some(self.max_f.map_or(v, |m| m.max(v)));
        }
    }

    pub fn push(&mut self, v: Value) {
        let row = self.rows;
        self.rows += 1;
        match (self.ty, v) {
            (_, Value::Null) => {
                Self::set_bit(&mut self.nulls, row);
                self.null_count += 1;
                self.push_filler();
            }
            (ColType::Text, Value::Text(s)) => self.push_text(s),
            (ColType::Text, other) => {
                let s = render(other);
                self.push_text(&s)
            }
            (ColType::Int, Value::Int(i))
            | (ColType::Time, Value::Time(i))
            | (ColType::Time, Value::Int(i)) => {
                self.ints.push(i);
                self.note_i(i);
            }
            (ColType::Float, Value::Float(f)) => {
                self.floats.push(f);
                self.note_f(f);
            }
            (ColType::Float, Value::Int(i)) => {
                self.floats.push(i as f64);
                self.note_f(i as f64);
            }
            (ColType::Bool, Value::Bool(b)) => {
                if b {
                    Self::set_bit(&mut self.bools, row);
                }
            }
            (_, other) => {
                // Type mismatch: keep the original text, never drop it.
                self.side
                    .push((row, render(other).into_owned().into_boxed_str()));
                self.push_filler();
            }
        }
    }

    /// Push a typed value together with the exact source text it came from.
    /// The text is stored only if some value's canonical rendering differs
    /// (e.g. `14/02/2018 08:31:01` vs `2018-02-14 08:31:01`, `1.50` vs `1.5`),
    /// so free-text search and display always see the original.
    pub fn push_with_raw(&mut self, v: Value, raw: &str) {
        let row = self.rows;
        match (self.ty, v) {
            (ColType::Float, Value::Float(f)) => {
                while self.decs.len() < row as usize {
                    self.decs.push(0);
                    self.dec_m.push(0);
                }
                match format::canonical_decimals(raw).zip(format::mantissa(raw)) {
                    Some((k, m)) => {
                        self.decs.push(k);
                        self.dec_m.push(m);
                    }
                    None => {
                        self.decs.push(255);
                        self.dec_m.push(0);
                        self.raw_ex.push((row, raw.into()));
                        self.ex_num.push((row, f));
                    }
                }
            }
            (ColType::Time, Value::Time(_)) => {
                if !self.tmpl_tried {
                    self.tmpl = TimeTemplate::infer(raw, self.date_order);
                    self.tmpl_tried = true;
                }
                if !self.tmpl.as_ref().is_some_and(|t| t.matches(raw)) {
                    self.raw_ex.push((row, raw.into()));
                }
            }
            (ColType::Bool, Value::Bool(_)) => {
                let c = BoolCase::of(raw);
                if self.bool_case.is_none() && c.is_some() {
                    self.bool_case = c;
                }
                if c.is_none() || c != self.bool_case {
                    self.raw_ex.push((row, raw.into()));
                }
            }
            _ => {}
        }
        self.push(v);
    }

    pub fn finish(self) -> EncodedColumn {
        codec::encode(self)
    }
}

/// Text rendering used for display, search and type-mismatch preservation.
pub fn render<'a>(v: Value<'a>) -> Cow<'a, str> {
    match v {
        Value::Null => Cow::Borrowed(""),
        Value::Text(s) => Cow::Borrowed(s),
        Value::Int(i) => Cow::Owned(i.to_string()),
        Value::Float(f) => Cow::Owned(fmt_float(f)),
        Value::Bool(b) => Cow::Borrowed(if b { "true" } else { "false" }),
        Value::Time(t) => Cow::Owned(crate::types::fmt_time(t)),
    }
}

pub fn fmt_float(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{f:.0}")
    } else {
        let s = format!("{f}");
        s
    }
}

/// A column ready to be written.
#[derive(Debug)]
pub struct EncodedColumn {
    pub(crate) meta: ColMeta,
    pub(crate) blob: Vec<u8>,
    pub(crate) bloom: Option<Vec<u8>>,
}

impl EncodedColumn {
    pub fn encoding(&self) -> Encoding {
        self.meta.enc
    }
    pub fn col_id(&self) -> u32 {
        self.meta.col_id
    }
    pub fn compressed_len(&self) -> usize {
        self.blob.len() + self.bloom.as_ref().map_or(0, |b| b.len())
    }
    pub fn meta(&self) -> &ColMeta {
        &self.meta
    }
    /// Keep the column's metadata and Bloom filter, drop its values: they
    /// are read from the source when needed.
    pub fn into_source(mut self) -> EncodedColumn {
        self.meta.in_source = true;
        self.blob = Vec::new();
        self
    }
    /// `into_source`, also dropping the Bloom filter (an index held in RAM
    /// spends that memory on cheap columns instead; the byte scan still
    /// settles words for this column).
    pub fn into_source_bare(mut self) -> EncodedColumn {
        self.bloom = None;
        self.meta.bloom_len = 0;
        self.into_source()
    }
    /// Decode in memory (a column rebuilt from the source, never written).
    pub fn decode(&self, rows: u32, budget: &Budget) -> Result<DecodedColumn> {
        let need = codec::decoded_estimate(&self.meta, rows);
        let res = budget.try_reserve(need, Component::Cache)?;
        codec::decode(&self.meta, rows, &self.blob, res)
    }
}

// ───────────────────────────── writer ─────────────────────────────

pub struct SegmentWriter;

impl SegmentWriter {
    /// Write atomically (temp file + rename). Returns bytes written.
    pub fn write(path: &Path, cols: &[EncodedColumn], meta: SegmentMeta) -> Result<u64> {
        let tmp = path.with_extension("kseg.tmp");
        let mut w = BufWriter::with_capacity(256 * 1024, File::create(&tmp)?);
        let off = Self::emit(&mut w, cols, meta)?;
        let f = w.into_inner().map_err(|e| Error::Io(e.into_error()))?;
        f.sync_data().ok();
        drop(f);
        std::fs::rename(&tmp, path)?;
        Ok(off)
    }

    /// The same segment, as bytes held in memory (zero-write tier).
    pub fn to_bytes(cols: &[EncodedColumn], meta: SegmentMeta) -> Result<Vec<u8>> {
        let mut v = Vec::new();
        Self::emit(&mut v, cols, meta)?;
        Ok(v)
    }

    fn emit<W: Write>(w: &mut W, cols: &[EncodedColumn], mut meta: SegmentMeta) -> Result<u64> {
        let mut off = 0u64;
        let mut metas: Vec<ColMeta> = Vec::with_capacity(cols.len());
        for c in cols {
            let mut m = c.meta.clone();
            w.write_all(&c.blob)?;
            m.off = off;
            m.len = c.blob.len() as u64;
            off += m.len;
            metas.push(m);
        }
        for (c, m) in cols.iter().zip(metas.iter_mut()) {
            if let Some(b) = &c.bloom {
                w.write_all(b)?;
                m.bloom_off = off;
                m.bloom_len = b.len() as u64;
                off += m.bloom_len;
            }
        }
        metas.sort_by_key(|m| m.col_id);
        meta.cols = metas;
        let header = serde_json::to_vec(&meta)?;
        w.write_all(&header)?;
        w.write_all(&(header.len() as u64).to_le_bytes())?;
        w.write_all(MAGIC_END)?;
        off += header.len() as u64 + 16;
        Ok(off)
    }
}

enum Data {
    File(File),
    Mem(std::sync::Arc<[u8]>),
}

impl Data {
    fn read(&self, buf: &mut [u8], off: u64) -> std::io::Result<()> {
        match self {
            Data::File(f) => read_at(f, buf, off),
            Data::Mem(b) => {
                let a = off as usize;
                let src = b
                    .get(a..a + buf.len())
                    .ok_or(std::io::ErrorKind::UnexpectedEof)?;
                buf.copy_from_slice(src);
                Ok(())
            }
        }
    }
    fn len(&self) -> std::io::Result<u64> {
        match self {
            Data::File(f) => Ok(f.metadata()?.len()),
            Data::Mem(b) => Ok(b.len() as u64),
        }
    }
}

// ───────────────────────────── reader ─────────────────────────────

pub struct SegmentReader {
    path: PathBuf,
    file: Data,
    meta: SegmentMeta,
    decodes: AtomicU64,
}

impl SegmentReader {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_data(path, Data::File(File::open(path)?))
    }

    /// A segment held in memory (zero-write tier); `name` stands in for a path.
    pub fn from_bytes(name: &str, bytes: std::sync::Arc<[u8]>) -> Result<Self> {
        Self::open_data(Path::new(name), Data::Mem(bytes))
    }

    /// Bytes held in memory (0 for a segment file).
    pub fn memory_bytes(&self) -> u64 {
        match &self.file {
            Data::Mem(b) => b.len() as u64,
            Data::File(_) => 0,
        }
    }

    fn open_data(path: &Path, file: Data) -> Result<Self> {
        let len = file.len()?;
        if len < 16 {
            return Err(Error::Corrupt(format!("{}: too short", path.display())));
        }
        let mut foot = [0u8; 16];
        file.read(&mut foot, len - 16)?;
        if &foot[8..] != MAGIC_END {
            return Err(Error::Corrupt(format!("{}: bad magic", path.display())));
        }
        let hlen = u64::from_le_bytes(foot[..8].try_into().unwrap());
        if hlen + 16 > len {
            return Err(Error::Corrupt(format!(
                "{}: bad header length",
                path.display()
            )));
        }
        let mut hdr = vec![0u8; hlen as usize];
        file.read(&mut hdr, len - 16 - hlen)?;
        let meta: SegmentMeta = serde_json::from_slice(&hdr)?;
        Ok(SegmentReader {
            path: path.to_path_buf(),
            file,
            meta,
            decodes: AtomicU64::new(0),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn meta(&self) -> &SegmentMeta {
        &self.meta
    }
    /// Number of column decodes performed by this reader (diagnostics/tests).
    pub fn decodes(&self) -> u64 {
        self.decodes.load(Ordering::Relaxed)
    }

    /// Bytes the decoded column will need (for budget planning).
    pub fn decoded_size(&self, col_id: u32) -> Option<u64> {
        self.meta
            .col(col_id)
            .map(|m| codec::decoded_estimate(m, self.meta.rows))
    }

    /// True when the column's values are read from the source, not this file.
    pub fn in_source(&self, col_id: u32) -> bool {
        self.meta.col(col_id).is_some_and(|m| m.in_source)
    }

    /// Decode one stored column. `Ok(None)` when the column is absent (all
    /// null) here. Columns read from the source go through `Store::column`.
    pub fn column(&self, col_id: u32, budget: &Budget) -> Result<Option<DecodedColumn>> {
        let Some(m) = self.meta.col(col_id) else {
            return Ok(None);
        };
        if m.in_source {
            return Err(Error::Invalid(format!(
                "column {col_id} is read from the source; use Store::column"
            )));
        }
        let need = codec::decoded_estimate(m, self.meta.rows) + m.len;
        let res: Reservation = budget.try_reserve(need, Component::Cache)?;
        let mut blob = vec![0u8; m.len as usize];
        self.file.read(&mut blob, m.off)?;
        self.decodes.fetch_add(1, Ordering::Relaxed);
        let col = codec::decode(m, self.meta.rows, &blob, res)?;
        Ok(Some(col))
    }

    /// False only when `needle_lower` (ASCII-lowercased) certainly does not occur
    /// as a substring of any value of the column in this segment.
    pub fn may_contain_substring(&self, col_id: u32, needle_lower: &str) -> bool {
        let Some(m) = self.meta.col(col_id) else {
            return false;
        };
        if m.bloom_len == 0 {
            return true;
        }
        let mut b = vec![0u8; m.bloom_len as usize];
        if self.file.read(&mut b, m.bloom_off).is_err() {
            return true;
        }
        bloom::may_contain(&b, needle_lower.as_bytes())
    }

    /// Raw bloom bytes, for callers that cache them.
    pub fn bloom_bytes(&self, col_id: u32) -> Option<Vec<u8>> {
        let m = self.meta.col(col_id)?;
        if m.bloom_len == 0 {
            return None;
        }
        let mut b = vec![0u8; m.bloom_len as usize];
        self.file.read(&mut b, m.bloom_off).ok()?;
        Some(b)
    }
}

pub(crate) fn read_at(f: &File, buf: &mut [u8], mut off: u64) -> std::io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        #[cfg(windows)]
        let n = std::os::windows::fs::FileExt::seek_read(f, &mut buf[done..], off)?;
        #[cfg(unix)]
        let n = std::os::unix::fs::FileExt::read_at(f, &mut buf[done..], off)?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        done += n;
        off += n as u64;
    }
    Ok(())
}
