//! Rebuild a segment's columns from the source file: the same parser and the
//! final schema give the same columns the import stored, so a column that was
//! never written (in-place and zero-write tiers) reads exactly like one that was.

use super::pipeline::{parse_payload, GlobalSchema, Payload};
use crate::error::{Error, Result};
use crate::segment::{EncodedColumn, SegmentMeta};
use crate::store::{Fingerprint, Manifest, SourceSpec, OFFSET_COL};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct Rebuilder {
    path: PathBuf,
    file: std::fs::File,
    spec: SourceSpec,
    global: Mutex<GlobalSchema>,
    size: u64,
    mtime_ns: u64,
    /// For one of several files: what its rows say in the `_file` column.
    label: Option<String>,
}

fn changed(path: &std::path::Path) -> Error {
    Error::Invalid(format!(
        "the file {} changed since it was indexed; open it again to index the new contents",
        path.display()
    ))
}

impl Rebuilder {
    pub fn new(m: &Manifest) -> Result<Rebuilder> {
        Self::for_part(m, 0)
    }

    /// The rebuilder for file `part` of an index built from several (0, the
    /// only file, otherwise).
    pub fn for_part(m: &Manifest, part: u32) -> Result<Rebuilder> {
        let (path, want, spec, label) = match m.parts.get(part as usize) {
            Some(p) => (&p.path, &p.fingerprint, &p.spec, Some(p.label.clone())),
            None => (&m.source, &m.fingerprint, &m.spec, None),
        };
        let fp = Fingerprint::of(path)
            .map_err(|_| Error::Invalid(format!("the file {} cannot be read", path.display())))?;
        if &fp != want {
            return Err(changed(path));
        }
        Ok(Rebuilder {
            path: path.clone(),
            file: std::fs::File::open(path)?,
            spec: spec.clone(),
            global: Mutex::new(GlobalSchema::from(&m.schema)),
            size: fp.size,
            mtime_ns: fp.mtime_ns,
            label,
        })
    }

    /// Size and modification time still match the indexed file (cheap: one stat).
    fn check(&self) -> Result<()> {
        let md = self.file.metadata()?;
        let mt = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        if md.len() != self.size || mt != self.mtime_ns {
            return Err(changed(&self.path));
        }
        Ok(())
    }

    /// Columns for `rows` of the segment `seg`, whose row byte offsets are
    /// `offsets`. Row ranges come from the offsets, never from scanning for line
    /// ends, so quoted newlines and JSON separators are cut exactly as at import.
    pub fn rows(
        &self,
        seg: &SegmentMeta,
        offsets: &[i64],
        rows: Range<usize>,
    ) -> Result<Vec<EncodedColumn>> {
        self.check()?;
        let n = seg.rows as usize;
        if rows.start >= rows.end || rows.end > n || offsets.len() != n {
            return Err(Error::Invalid(format!(
                "rows {}..{} are not in this segment of {n} rows",
                rows.start, rows.end
            )));
        }
        let a = offsets[rows.start] as u64;
        let b = if rows.end < n {
            offsets[rows.end] as u64
        } else {
            seg.byte_end
        };
        let mut buf = vec![0u8; b.saturating_sub(a) as usize];
        crate::segment::read_at(&self.file, &mut buf, a)?;
        let elements = matches!(
            &self.spec,
            SourceSpec::Json { record_path, array, stream, .. } if record_path.is_some() || *array || *stream
        );
        let payload = if elements {
            // each element ends where its value ends: read forward from its
            // start, tracking depth and strings, as the import's scanner did
            let mut elems = Vec::with_capacity(rows.len());
            for r in rows.clone() {
                let s = (offsets[r] as u64 - a) as usize;
                let limit = if r + 1 < rows.end {
                    (offsets[r + 1] as u64 - a) as usize
                } else {
                    buf.len()
                };
                let e = value_end(&buf[..limit], s);
                elems.push((offsets[r] as u64, s, e));
            }
            Payload::Elements { buf, elems, end: b }
        } else {
            Payload::Text {
                bytes: buf,
                first: false,
            }
        };
        let (mut segs, _) =
            parse_payload(&payload, a, &self.spec, &self.global, self.label.as_deref());
        if segs.len() != 1 {
            return Err(changed(&self.path));
        }
        let (cols, meta) = segs.pop().unwrap();
        if meta.rows as usize != rows.len() {
            return Err(changed(&self.path));
        }
        // the row offsets must line up with the ones indexed
        if let Some(o) = cols.iter().find(|c| c.col_id() == OFFSET_COL) {
            let d = o.decode(meta.rows, &crate::budget::Budget::new(64 << 20))?;
            if (0..rows.len()).any(|k| d.int(k) != Some(offsets[rows.start + k])) {
                return Err(changed(&self.path));
            }
        }
        Ok(cols)
    }
}

/// Where the JSON value starting at `s` (after any white space) ends.
fn value_end(buf: &[u8], s: usize) -> usize {
    let mut i = s;
    while i < buf.len() && buf[i].is_ascii_whitespace() {
        i += 1;
    }
    match buf.get(i) {
        Some(b'{') | Some(b'[') => {
            let (mut depth, mut in_str, mut esc) = (0i64, false, false);
            while i < buf.len() {
                let c = buf[i];
                i += 1;
                if in_str {
                    match (esc, c) {
                        (true, _) => esc = false,
                        (false, b'\\') => esc = true,
                        (false, b'"') => in_str = false,
                        _ => {}
                    }
                    continue;
                }
                match c {
                    b'"' => in_str = true,
                    b'[' | b'{' => depth += 1,
                    b']' | b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            return i;
                        }
                    }
                    _ => {}
                }
            }
            buf.len()
        }
        Some(b'"') => {
            let mut esc = false;
            i += 1;
            while i < buf.len() {
                let c = buf[i];
                i += 1;
                match (esc, c) {
                    (true, _) => esc = false,
                    (false, b'\\') => esc = true,
                    (false, b'"') => return i,
                    _ => {}
                }
            }
            buf.len()
        }
        Some(_) => {
            // a number, true, false or null
            while i < buf.len()
                && !matches!(buf[i], b',' | b']' | b'}' | b' ' | b'\t' | b'\r' | b'\n')
            {
                i += 1;
            }
            i
        }
        None => buf.len(),
    }
}
