//! Sorting rows by a column, for any size: sorted in memory while it fits,
//! otherwise in sorted runs on disk merged into one order. The result is
//! the rows' ids in order, read a page at a time (no limits).

use crate::types::ColType;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// What a value sorts as.
#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    Num(f64),
    Text(String),
    /// No value: always after everything else.
    Null,
}

/// The sort key of a cell of a column of type `ty`.
pub fn key_for(ty: ColType, v: Option<&str>) -> Key {
    let Some(s) = v.map(str::trim).filter(|s| !s.is_empty()) else {
        return Key::Null;
    };
    let num = match ty {
        ColType::Int | ColType::Float => crate::ingest::infer::parse_float(s),
        ColType::Time => crate::time::parse_time(s).map(|t| t as f64),
        ColType::Bool => crate::ingest::infer::parse_bool(s).map(|b| b as u8 as f64),
        ColType::Text => None,
    };
    match num {
        Some(n) => Key::Num(n),
        None => Key::Text(s.to_string()),
    }
}

fn cmp_keys(a: &Key, b: &Key, desc: bool) -> Ordering {
    let o = match (a, b) {
        (Key::Null, Key::Null) => return Ordering::Equal,
        (Key::Null, _) => return Ordering::Greater,
        (_, Key::Null) => return Ordering::Less,
        (Key::Num(x), Key::Num(y)) => x.total_cmp(y),
        (Key::Num(_), Key::Text(_)) => Ordering::Less,
        (Key::Text(_), Key::Num(_)) => Ordering::Greater,
        (Key::Text(x), Key::Text(y)) => x
            .to_lowercase()
            .cmp(&y.to_lowercase())
            .then_with(|| x.cmp(y)),
    };
    if desc {
        o.reverse()
    } else {
        o
    }
}

fn cmp_entries(a: &(Key, u64), b: &(Key, u64), desc: bool) -> Ordering {
    cmp_keys(&a.0, &b.0, desc).then(a.1.cmp(&b.1))
}

fn write_entry(w: &mut impl Write, e: &(Key, u64)) -> std::io::Result<()> {
    match &e.0 {
        Key::Null => w.write_all(&[0])?,
        Key::Num(n) => {
            w.write_all(&[1])?;
            w.write_all(&n.to_le_bytes())?;
        }
        Key::Text(s) => {
            w.write_all(&[2])?;
            w.write_all(&(s.len() as u32).to_le_bytes())?;
            w.write_all(s.as_bytes())?;
        }
    }
    w.write_all(&e.1.to_le_bytes())
}

fn read_entry(r: &mut impl Read) -> Option<(Key, u64)> {
    let mut tag = [0u8; 1];
    r.read_exact(&mut tag).ok()?;
    let mut b8 = [0u8; 8];
    let key = match tag[0] {
        0 => Key::Null,
        1 => {
            r.read_exact(&mut b8).ok()?;
            Key::Num(f64::from_le_bytes(b8))
        }
        _ => {
            let mut b4 = [0u8; 4];
            r.read_exact(&mut b4).ok()?;
            let mut s = vec![0u8; u32::from_le_bytes(b4) as usize];
            r.read_exact(&mut s).ok()?;
            Key::Text(String::from_utf8_lossy(&s).into_owned())
        }
    };
    r.read_exact(&mut b8).ok()?;
    Some((key, u64::from_le_bytes(b8)))
}

/// Collects (key, row) pairs and sorts them, spilling sorted runs to
/// `scratch` whenever more than `run_bytes` are held.
pub struct Sorter {
    scratch: PathBuf,
    run_bytes: usize,
    desc: bool,
    buf: Vec<(Key, u64)>,
    held: usize,
    runs: Vec<PathBuf>,
    id: u64,
}

static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Sorter {
    pub fn new(scratch: &Path, run_bytes: usize, desc: bool) -> Sorter {
        Sorter {
            scratch: scratch.to_path_buf(),
            run_bytes: run_bytes.max(4 << 10),
            desc,
            buf: Vec::new(),
            held: 0,
            runs: Vec::new(),
            id: NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        }
    }

    pub fn push(&mut self, key: Key, row: u64) -> std::io::Result<()> {
        self.held += 40
            + match &key {
                Key::Text(s) => s.len(),
                _ => 0,
            };
        self.buf.push((key, row));
        if self.held >= self.run_bytes {
            self.spill()?;
        }
        Ok(())
    }

    fn spill(&mut self) -> std::io::Result<()> {
        let desc = self.desc;
        self.buf.sort_unstable_by(|a, b| cmp_entries(a, b, desc));
        std::fs::create_dir_all(&self.scratch)?;
        let p = self.scratch.join(format!(
            "sort-{}-{}-{}.run",
            std::process::id(),
            self.id,
            self.runs.len()
        ));
        let mut w = BufWriter::with_capacity(256 << 10, File::create(&p)?);
        for e in &self.buf {
            write_entry(&mut w, e)?;
        }
        w.flush()?;
        self.runs.push(p);
        self.buf.clear();
        self.held = 0;
        Ok(())
    }

    /// Finish: the rows in order.
    pub fn finish(mut self) -> std::io::Result<SortedOrder> {
        let desc = self.desc;
        if self.runs.is_empty() {
            self.buf.sort_unstable_by(|a, b| cmp_entries(a, b, desc));
            let mem: Vec<u64> = self.buf.iter().map(|e| e.1).collect();
            return Ok(SortedOrder {
                len: mem.len() as u64,
                mem,
                file: None,
            });
        }
        if !self.buf.is_empty() {
            self.spill()?;
        }
        // merge the runs into one file of row ids
        struct Item {
            e: (Key, u64),
            run: usize,
            desc: bool,
        }
        impl PartialEq for Item {
            fn eq(&self, o: &Self) -> bool {
                self.cmp(o) == Ordering::Equal
            }
        }
        impl Eq for Item {}
        impl PartialOrd for Item {
            fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
                Some(self.cmp(o))
            }
        }
        impl Ord for Item {
            // BinaryHeap pops the greatest: the smallest entry must compare greatest
            fn cmp(&self, o: &Self) -> Ordering {
                cmp_entries(&o.e, &self.e, self.desc)
            }
        }
        let mut readers: Vec<BufReader<File>> = self
            .runs
            .iter()
            .map(|p| File::open(p).map(|f| BufReader::with_capacity(64 << 10, f)))
            .collect::<std::io::Result<_>>()?;
        let mut heap = BinaryHeap::new();
        for (i, r) in readers.iter_mut().enumerate() {
            if let Some(e) = read_entry(r) {
                heap.push(Item { e, run: i, desc });
            }
        }
        let out_path = self
            .scratch
            .join(format!("sort-{}-{}.order", std::process::id(), self.id));
        let mut w = BufWriter::with_capacity(256 << 10, File::create(&out_path)?);
        let mut n = 0u64;
        while let Some(it) = heap.pop() {
            w.write_all(&it.e.1.to_le_bytes())?;
            n += 1;
            if let Some(e) = read_entry(&mut readers[it.run]) {
                heap.push(Item {
                    e,
                    run: it.run,
                    desc,
                });
            }
        }
        w.flush()?;
        drop(w);
        drop(readers);
        for p in &self.runs {
            let _ = std::fs::remove_file(p);
        }
        Ok(SortedOrder {
            len: n,
            mem: Vec::new(),
            file: Some((File::open(&out_path)?, out_path)),
        })
    }
}

impl Drop for Sorter {
    fn drop(&mut self) {
        for p in &self.runs {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Row ids in sorted order, in memory or in a scratch file.
pub struct SortedOrder {
    len: u64,
    mem: Vec<u64>,
    file: Option<(File, PathBuf)>,
}

impl SortedOrder {
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether the order lives on disk.
    pub fn on_disk(&self) -> bool {
        self.file.is_some()
    }

    /// Row ids at places `start..start+len`.
    pub fn page(&self, start: u64, len: usize) -> Vec<u64> {
        let s = start.min(self.len);
        let e = (s + len as u64).min(self.len);
        match &self.file {
            None => self.mem[s as usize..e as usize].to_vec(),
            Some((f, _)) => {
                let Ok(mut f) = f.try_clone() else {
                    return Vec::new();
                };
                let mut buf = vec![0u8; ((e - s) * 8) as usize];
                if f.seek(SeekFrom::Start(s * 8)).is_err() || f.read_exact(&mut buf).is_err() {
                    return Vec::new();
                }
                buf.chunks_exact(8)
                    .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
                    .collect()
            }
        }
    }
}

impl Drop for SortedOrder {
    fn drop(&mut self) {
        if let Some((f, p)) = self.file.take() {
            drop(f);
            let _ = std::fs::remove_file(p);
        }
    }
}
