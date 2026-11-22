//! The store manifest: everything needed to reopen, resume or verify an index.

use crate::error::{Error, Result};
use crate::ingest::csv::CsvOptions;
use crate::ingest::report::ImportReport;
use crate::time::DateOrder;
use crate::types::ColType;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use xxhash_rust::xxh3::xxh3_64;

pub const MANIFEST: &str = "manifest.json";
/// Bump whenever the segment or manifest format changes; older indexes are rebuilt.
pub const VERSION: u32 = 4; // 4: time column must be filled on half the rows

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Fingerprint {
    pub size: u64,
    pub mtime_ns: u64,
    pub head_tail: String,
}

impl Fingerprint {
    pub fn of(path: &Path) -> std::io::Result<Fingerprint> {
        let f = std::fs::File::open(path)?;
        let md = f.metadata()?;
        let size = md.len();
        let mtime_ns = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let n = size.min(64 * 1024) as usize;
        let mut head = vec![0u8; n];
        crate::segment::read_at(&f, &mut head, 0)?;
        let mut tail = vec![0u8; n];
        crate::segment::read_at(&f, &mut tail, size - n as u64)?;
        Ok(Fingerprint {
            size,
            mtime_ns,
            head_tail: format!("{:016x}{:016x}", xxh3_64(&head), xxh3_64(&tail)),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SourceSpec {
    Csv {
        opts: CsvOptions,
        header: Vec<String>,
    },
    /// `record_path`: wrapper key holding the records; `array`: top-level array;
    /// `stream`: top-level values back to back (pretty-printed objects, tool output).
    Json {
        record_path: Option<String>,
        array: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        stream: bool,
        /// Elasticsearch bulk file: an instruction line (`{"index":{...}}`)
        /// before each document; folded into the document as `_index`/`_id`.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        bulk: bool,
    },
    /// Lines of text with no header, split into the parts worked out from the
    /// file's own lines; `header` names one column per part, then `_rest`.
    Text {
        parts: Vec<crate::ingest::textlayout::Part>,
        header: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ColumnInfo {
    pub name: String,
    pub ty: ColType,
    /// Non-null values in the whole source (exact once the import is done).
    pub count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_order: Option<DateOrder>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StoreSchema {
    pub columns: Vec<ColumnInfo>,
    pub time_col: Option<usize>,
    /// When the time axis is an integer epoch column: µs per unit (1e6 = seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_unit_us: Option<i64>,
}

impl StoreSchema {
    /// A raw time-column value in epoch µs.
    pub fn to_us(&self, v: i64) -> i64 {
        v.saturating_mul(self.time_unit_us.unwrap_or(1))
    }
}

impl StoreSchema {
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }
    pub fn len(&self) -> usize {
        self.columns.len()
    }
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub version: u32,
    pub source: PathBuf,
    pub fingerprint: Fingerprint,
    pub spec: SourceSpec,
    pub schema: StoreSchema,
    pub rows: u64,
    /// Source bytes fully committed to segments (resume point).
    pub bytes_done: u64,
    pub segments: Vec<String>,
    pub report: ImportReport,
    /// "running" | "done" | "cancelled" | "partial: …" | "failed: …"
    pub state: String,
    /// Several files indexed as one: every file, in the order read (the
    /// first is also `source`); each segment says which one it came from.
    /// Empty for an index of one file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<SourcePart>,
}

/// One of the files of an index built from several.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourcePart {
    pub path: PathBuf,
    pub fingerprint: Fingerprint,
    pub spec: SourceSpec,
    /// What the `_file` column says for its rows.
    pub label: String,
}

impl Manifest {
    pub fn load(dir: &Path) -> Result<Manifest> {
        let p = dir.join(MANIFEST);
        let mut last = None;
        for _ in 0..20 {
            match std::fs::read(&p) {
                Ok(b) => return Ok(serde_json::from_slice(&b)?),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Error::Io(e)),
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        }
        Err(Error::Io(last.unwrap()))
    }

    /// Atomic replace (temp + rename), retried briefly if a reader holds the file.
    pub fn save(&self, dir: &Path) -> Result<()> {
        let tmp = dir.join("manifest.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        let dst = dir.join(MANIFEST);
        let mut last = None;
        for _ in 0..50 {
            match std::fs::rename(&tmp, &dst) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        }
        Err(Error::Io(last.unwrap()))
    }
}
