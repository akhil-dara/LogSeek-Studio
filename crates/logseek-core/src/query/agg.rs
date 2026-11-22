//! Aggregation: `stats … by …`, `top`, `timechart`, and sorted top-K.
//!
//! Group tables are charged to the memory budget. When a pass would exceed it,
//! the work restarts as P hash-partitioned passes (pass k handles keys with
//! `hash % P == k`): exact results with no disk spill, so a tiny disk never makes it refuse.

use super::ast::{Agg, AggFn};
use super::exec::{resolve_column, ChunkCache, MatchSet, SegMatch};
use crate::budget::{Component, Reservation};
use crate::error::{Error, Result};
use crate::segment::DecodedColumn;
use crate::store::Store;
use crate::types::ColType;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use xxhash_rust::xxh3::xxh3_64;

const SEP: char = '\u{1f}';
/// Joins the key columns of one group (for `count_groups` callers).
pub const KEY_SEP: char = SEP;
const MAX_PASSES: u64 = 4096;

/// Rows (within a segment) selected by a segment match.
pub fn match_rows(m: &SegMatch, rows: u32) -> Box<dyn Iterator<Item = usize> + '_> {
    match m {
        SegMatch::None => Box::new(std::iter::empty()),
        SegMatch::All(_) => Box::new(0..rows as usize),
        SegMatch::Bits { bits, .. } => Box::new(bits.iter().enumerate().flat_map(|(wi, &w)| {
            let mut w = w;
            std::iter::from_fn(move || {
                if w == 0 {
                    return None;
                }
                let b = w.trailing_zeros() as usize;
                w &= w - 1;
                Some(wi * 64 + b)
            })
        })),
    }
}

#[derive(Debug, Clone)]
struct St {
    count: u64,
    sum: f64,
    n: u64,
    min: f64,
    max: f64,
    dc: Option<HashSet<u64>>,
}

impl St {
    fn new(f: AggFn) -> St {
        St {
            count: 0,
            sum: 0.0,
            n: 0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            dc: (f == AggFn::Dc).then(HashSet::new),
        }
    }
    fn value(&self, f: AggFn) -> f64 {
        match f {
            AggFn::Count => self.count as f64,
            AggFn::Sum => self.sum,
            AggFn::Avg => {
                if self.n == 0 {
                    f64::NAN
                } else {
                    self.sum / self.n as f64
                }
            }
            AggFn::Min => {
                if self.n == 0 {
                    f64::NAN
                } else {
                    self.min
                }
            }
            AggFn::Max => {
                if self.n == 0 {
                    f64::NAN
                } else {
                    self.max
                }
            }
            AggFn::Dc => self.dc.as_ref().map_or(0, |s| s.len()) as f64,
        }
    }
}

/// A stats result: compact in memory (key bytes + values, charged to the
/// budget) and spilled to an indexed scratch file when it does not fit.
pub struct StatsResult {
    /// Group-by column names followed by one name per aggregate.
    pub columns: Vec<String>,
    pub exact: bool,
    /// Partitioned passes needed to stay inside the memory budget.
    pub passes: u64,
    table: ResultTable,
}

impl StatsResult {
    pub fn len(&self) -> usize {
        self.table.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn spilled(&self) -> bool {
        self.table.spill.is_some()
    }
    /// Group `i`: `(key values, aggregate values)`.
    pub fn get(&self, i: usize) -> Option<(Vec<String>, Vec<f64>)> {
        self.table.get(i)
    }
    pub fn iter(&self) -> impl Iterator<Item = (Vec<String>, Vec<f64>)> + '_ {
        self.table.iter()
    }
}

const SPARSE: usize = 1024;

struct Spill {
    path: std::path::PathBuf,
    file: std::fs::File,
    w: Option<std::io::BufWriter<std::fs::File>>,
    /// Byte offset of every SPARSE-th record.
    sparse: Vec<u64>,
    len: usize,
    bytes: u64,
}

impl Drop for Spill {
    fn drop(&mut self) {
        self.w = None;
        let _ = std::fs::remove_file(&self.path);
    }
}

struct ResultTable {
    nkeys: usize,
    naggs: usize,
    keys: Vec<u8>,
    key_off: Vec<u64>,
    vals: Vec<f64>,
    /// Display order over the in-memory part (sorted by key) when not spilled.
    order: Option<Vec<u32>>,
    spill: Option<Spill>,
    held: Vec<Reservation>,
    reserved: u64,
    used: u64,
}

impl ResultTable {
    fn new(nkeys: usize, naggs: usize) -> Self {
        ResultTable {
            nkeys,
            naggs,
            keys: Vec::new(),
            key_off: vec![0],
            vals: Vec::new(),
            order: None,
            spill: None,
            held: Vec::new(),
            reserved: 0,
            used: 0,
        }
    }
    fn mem_len(&self) -> usize {
        self.key_off.len() - 1
    }
    fn len(&self) -> usize {
        self.mem_len() + self.spill.as_ref().map_or(0, |s| s.len)
    }
    fn charge(&mut self, cache: &ChunkCache, bytes: u64) -> bool {
        // results may hold at most half the budget: the rest is for the passes
        if self.reserved.max(self.used) + bytes > cache.budget().cap() / 2 {
            return false;
        }
        self.used += bytes;
        while self.used > self.reserved {
            match cache.reserve(
                (64u64 << 10).max(self.used - self.reserved),
                Component::Query,
            ) {
                Ok(r) => {
                    self.reserved += r.bytes();
                    self.held.push(r);
                }
                Err(_) => return false,
            }
        }
        true
    }
    fn push(
        &mut self,
        cache: &ChunkCache,
        scratch: &std::path::Path,
        key: &str,
        vals: &[f64],
    ) -> Result<()> {
        if self.spill.is_none() && self.charge(cache, key.len() as u64 + 8 + vals.len() as u64 * 8)
        {
            self.keys.extend_from_slice(key.as_bytes());
            self.key_off.push(self.keys.len() as u64);
            self.vals.extend_from_slice(vals);
            return Ok(());
        }
        use std::io::Write;
        if self.spill.is_none() {
            std::fs::create_dir_all(scratch)?;
            let path = scratch.join(format!(
                "stats-{}-{:x}.tmp",
                std::process::id(),
                xxh3_64(&self.used.to_le_bytes()) ^ (self as *const _ as u64)
            ));
            let f = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .read(true)
                .write(true)
                .open(&path)?;
            let w = std::io::BufWriter::with_capacity(256 * 1024, f.try_clone()?);
            self.spill = Some(Spill {
                path,
                file: f,
                w: Some(w),
                sparse: Vec::new(),
                len: 0,
                bytes: 0,
            });
        }
        let sp = self.spill.as_mut().unwrap();
        if sp.len % SPARSE == 0 {
            sp.sparse.push(sp.bytes);
        }
        let w = sp.w.as_mut().unwrap();
        w.write_all(&(key.len() as u32).to_le_bytes())?;
        w.write_all(key.as_bytes())?;
        for v in vals {
            w.write_all(&v.to_le_bytes())?;
        }
        sp.bytes += 4 + key.len() as u64 + vals.len() as u64 * 8;
        sp.len += 1;
        Ok(())
    }
    fn finish(&mut self, cache: &ChunkCache) -> Result<()> {
        if let Some(sp) = self.spill.as_mut() {
            if let Some(mut w) = sp.w.take() {
                use std::io::Write;
                w.flush()?;
            }
        } else if self.charge(cache, self.mem_len() as u64 * 4) {
            let mut order: Vec<u32> = (0..self.mem_len() as u32).collect();
            order.sort_by(|&a, &b| self.key(a as usize).cmp(self.key(b as usize)));
            self.order = Some(order);
        }
        Ok(())
    }
    fn key(&self, i: usize) -> &[u8] {
        &self.keys[self.key_off[i] as usize..self.key_off[i + 1] as usize]
    }
    fn split(&self, k: &[u8]) -> Vec<String> {
        if self.nkeys == 0 {
            return Vec::new();
        }
        String::from_utf8_lossy(k)
            .split(SEP)
            .map(str::to_string)
            .collect()
    }
    fn mem_get(&self, i: usize) -> (Vec<String>, Vec<f64>) {
        let i = self.order.as_ref().map_or(i, |o| o[i] as usize);
        (
            self.split(self.key(i)),
            self.vals[i * self.naggs..(i + 1) * self.naggs].to_vec(),
        )
    }
    fn read_record(&self, r: &mut impl std::io::Read) -> Option<(Vec<String>, Vec<f64>)> {
        let mut b4 = [0u8; 4];
        r.read_exact(&mut b4).ok()?;
        let mut k = vec![0u8; u32::from_le_bytes(b4) as usize];
        r.read_exact(&mut k).ok()?;
        let mut vals = Vec::with_capacity(self.naggs);
        for _ in 0..self.naggs {
            let mut b8 = [0u8; 8];
            r.read_exact(&mut b8).ok()?;
            vals.push(f64::from_le_bytes(b8));
        }
        Some((self.split(&k), vals))
    }
    fn get(&self, i: usize) -> Option<(Vec<String>, Vec<f64>)> {
        if i < self.mem_len() {
            return Some(self.mem_get(i));
        }
        let sp = self.spill.as_ref()?;
        let j = i - self.mem_len();
        if j >= sp.len {
            return None;
        }
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::io::BufReader::new(sp.file.try_clone().ok()?);
        f.seek(SeekFrom::Start(sp.sparse[j / SPARSE])).ok()?;
        for _ in 0..j % SPARSE {
            let mut b4 = [0u8; 4];
            f.read_exact(&mut b4).ok()?;
            f.seek_relative(u32::from_le_bytes(b4) as i64 + self.naggs as i64 * 8)
                .ok()?;
        }
        self.read_record(&mut f)
    }
    fn iter(&self) -> impl Iterator<Item = (Vec<String>, Vec<f64>)> + '_ {
        let mem = (0..self.mem_len()).map(move |i| self.mem_get(i));
        let mut reader = self.spill.as_ref().and_then(|sp| {
            use std::io::{Seek, SeekFrom};
            let mut f = std::io::BufReader::with_capacity(256 * 1024, sp.file.try_clone().ok()?);
            f.seek(SeekFrom::Start(0)).ok()?;
            Some((f, sp.len))
        });
        let spilled = std::iter::from_fn(move || {
            let (f, left) = reader.as_mut()?;
            if *left == 0 {
                return None;
            }
            *left -= 1;
            self.read_record(f)
        });
        mem.chain(spilled)
    }
}

struct Charge<'a> {
    cache: &'a ChunkCache,
    held: Vec<Reservation>,
    reserved: u64,
    used: u64,
}

impl<'a> Charge<'a> {
    fn add(&mut self, bytes: u64) -> std::result::Result<(), ()> {
        self.used += bytes;
        while self.used > self.reserved {
            let step = (64u64 << 10).max(self.used - self.reserved);
            match self.cache.reserve(step, Component::Query) {
                Ok(r) => {
                    self.reserved += r.bytes();
                    self.held.push(r);
                }
                Err(_) => return Err(()),
            }
        }
        Ok(())
    }
}

enum PassErr {
    Overflow,
    Fail(Error),
}

fn agg_label(a: &Agg) -> String {
    match &a.col {
        Some(c) => format!("{}({c})", a.func.name()),
        None => a.func.name().to_string(),
    }
}

pub fn run_stats(
    st: &Store,
    m: &MatchSet,
    aggs: &[Agg],
    by: &[String],
    cache: &ChunkCache,
    cancel: &AtomicBool,
) -> Result<StatsResult> {
    let by_ids: Vec<u32> = by
        .iter()
        .map(|b| resolve_column(st, b).map(|i| i as u32))
        .collect::<Result<_>>()?;
    let agg_ids: Vec<Option<u32>> = aggs
        .iter()
        .map(|a| {
            a.col
                .as_ref()
                .map(|c| resolve_column(st, c).map(|i| i as u32))
                .transpose()
        })
        .collect::<Result<_>>()?;
    let scratch = st.dir().join("_tmp");
    let mut passes = 1u64;
    loop {
        let mut table = ResultTable::new(by.len(), aggs.len());
        let mut overflow = false;
        for k in 0..passes {
            match stats_pass(st, m, aggs, &by_ids, &agg_ids, cache, cancel, passes, k) {
                Ok(g) => {
                    for (key, vals) in g {
                        table.push(cache, &scratch, &key, &vals)?;
                    }
                }
                Err(PassErr::Overflow) => {
                    overflow = true;
                    break;
                }
                Err(PassErr::Fail(e)) => return Err(e),
            }
        }
        if overflow {
            if passes >= MAX_PASSES {
                return Err(Error::Invalid(
                    "group-by exceeds the memory budget even in 4096 passes".into(),
                ));
            }
            passes *= 4;
            continue;
        }
        table.finish(cache)?;
        let mut columns: Vec<String> = by.to_vec();
        columns.extend(aggs.iter().map(agg_label));
        return Ok(StatsResult {
            columns,
            exact: true,
            passes,
            table,
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn stats_pass(
    st: &Store,
    m: &MatchSet,
    aggs: &[Agg],
    by_ids: &[u32],
    agg_ids: &[Option<u32>],
    cache: &ChunkCache,
    cancel: &AtomicBool,
    parts: u64,
    part: u64,
) -> std::result::Result<Vec<(String, Vec<f64>)>, PassErr> {
    let mut groups: HashMap<String, Vec<St>> = HashMap::new();
    let mut charge = Charge {
        cache,
        held: Vec::new(),
        reserved: 0,
        used: 0,
    };
    let mut key = String::new();
    for (si, sm) in m.segs.iter().enumerate() {
        if matches!(sm, SegMatch::None) {
            continue;
        }
        if cancel.load(Ordering::SeqCst) {
            return Err(PassErr::Fail(Error::Cancelled));
        }
        let rows = st.segments()[si].meta().rows;
        let get = |c: u32| cache.get(st, si, c).map_err(PassErr::Fail);
        let bcols: Vec<Option<Arc<DecodedColumn>>> = by_ids
            .iter()
            .map(|&c| get(c))
            .collect::<std::result::Result<_, _>>()?;
        let acols: Vec<Option<Arc<DecodedColumn>>> = agg_ids
            .iter()
            .map(|c| match c {
                Some(c) => get(*c),
                None => Ok(None),
            })
            .collect::<std::result::Result<_, _>>()?;
        for r in match_rows(sm, rows) {
            key.clear();
            for (i, c) in bcols.iter().enumerate() {
                if i > 0 {
                    key.push(SEP);
                }
                if let Some(t) = c.as_ref().and_then(|c| c.text(r)) {
                    key.push_str(&t);
                }
            }
            if parts > 1 && xxh3_64(key.as_bytes()) % parts != part {
                continue;
            }
            let entry = match groups.get_mut(key.as_str()) {
                Some(e) => e,
                None => {
                    if charge
                        .add(key.len() as u64 + 64 + aggs.len() as u64 * 72)
                        .is_err()
                    {
                        return Err(PassErr::Overflow);
                    }
                    groups
                        .entry(key.clone())
                        .or_insert_with(|| aggs.iter().map(|a| St::new(a.func)).collect())
                }
            };
            for (j, a) in aggs.iter().enumerate() {
                let s = &mut entry[j];
                match a.func {
                    AggFn::Count => match &acols[j] {
                        None if agg_ids[j].is_none() => s.count += 1,
                        Some(c) if !c.is_null(r) => s.count += 1,
                        _ => {}
                    },
                    AggFn::Dc => {
                        if let Some(t) = acols[j].as_ref().and_then(|c| c.text(r)) {
                            if s.dc.as_mut().unwrap().insert(xxh3_64(t.as_bytes()))
                                && charge.add(16).is_err()
                            {
                                return Err(PassErr::Overflow);
                            }
                        }
                    }
                    _ => {
                        if let Some(v) = acols[j].as_ref().and_then(|c| c.float(r)) {
                            s.n += 1;
                            s.sum += v;
                            s.min = s.min.min(v);
                            s.max = s.max.max(v);
                        }
                    }
                }
            }
        }
    }
    let _ = by_ids;
    Ok(groups
        .into_iter()
        .map(|(k, sts)| {
            let vals = sts.iter().zip(aggs).map(|(s, a)| s.value(a.func)).collect();
            (k, vals)
        })
        .collect())
}

/// Groups of a source that can be read again (time buckets, made columns):
/// `read(each)` calls `each(key, value)` for every row, the key's columns
/// joined with `KEY_SEP`. Each group gets its row count and, with `func`, that
/// measure of `value`. Exact, in budget-sized hash partitions; no limit.
pub fn count_groups<F>(
    nkeys: usize,
    func: Option<AggFn>,
    scratch: &std::path::Path,
    cache: &ChunkCache,
    cancel: &AtomicBool,
    mut read: F,
) -> Result<StatsResult>
where
    F: FnMut(&mut dyn FnMut(&str, Option<&str>)) -> Result<()>,
{
    let naggs = 1 + usize::from(func.is_some());
    let mut passes = 1u64;
    loop {
        let mut table = ResultTable::new(nkeys, naggs);
        let mut overflow = false;
        for part in 0..passes {
            if cancel.load(Ordering::SeqCst) {
                return Err(Error::Cancelled);
            }
            let mut groups: HashMap<String, (St, Option<St>)> = HashMap::new();
            let mut charge = Charge {
                cache,
                held: Vec::new(),
                reserved: 0,
                used: 0,
            };
            let mut over = false;
            read(&mut |k: &str, v: Option<&str>| {
                if over {
                    return;
                }
                if passes > 1 && xxh3_64(k.as_bytes()) % passes != part {
                    return;
                }
                if !groups.contains_key(k) {
                    if charge.add(k.len() as u64 + 64 + naggs as u64 * 72).is_err() {
                        over = true;
                        return;
                    }
                    groups.insert(k.to_string(), (St::new(AggFn::Count), func.map(St::new)));
                }
                let (c, m) = groups.get_mut(k).unwrap();
                c.count += 1;
                if let (Some(m), Some(f)) = (m.as_mut(), func) {
                    match f {
                        AggFn::Count => {
                            if v.is_some_and(|v| !v.is_empty()) {
                                m.count += 1;
                            }
                        }
                        AggFn::Dc => {
                            if let Some(t) = v.filter(|t| !t.is_empty()) {
                                if m.dc.as_mut().unwrap().insert(xxh3_64(t.as_bytes()))
                                    && charge.add(16).is_err()
                                {
                                    over = true;
                                }
                            }
                        }
                        _ => {
                            if let Some(x) = v.and_then(crate::ingest::infer::parse_float) {
                                m.n += 1;
                                m.sum += x;
                                m.min = m.min.min(x);
                                m.max = m.max.max(x);
                            }
                        }
                    }
                }
            })?;
            if over {
                overflow = true;
                break;
            }
            for (k, (c, m)) in groups {
                let mut vals = vec![c.value(AggFn::Count)];
                if let (Some(m), Some(f)) = (m, func) {
                    vals.push(m.value(f));
                }
                table.push(cache, scratch, &k, &vals)?;
            }
        }
        if overflow {
            if passes >= MAX_PASSES {
                return Err(Error::Invalid(
                    "grouping exceeds the memory budget even in 4096 passes".into(),
                ));
            }
            passes *= 4;
            continue;
        }
        table.finish(cache)?;
        let mut columns: Vec<String> = (0..nkeys).map(|i| format!("key{i}")).collect();
        columns.push("count".into());
        if let Some(f) = func {
            columns.push(f.name().to_string());
        }
        return Ok(StatsResult {
            columns,
            exact: true,
            passes,
            table,
        });
    }
}

/// Every distinct value of a source that can be read again (a made column):
/// exact counts in budget-sized hash partitions, the table spilled to disk
/// when large. No limit: more passes, never fewer values. `read(each)` must
/// call `each(value)` for every row, the same way every time it is called.
pub fn count_values<F>(
    label: &str,
    scratch: &std::path::Path,
    cache: &ChunkCache,
    cancel: &AtomicBool,
    mut read: F,
) -> Result<StatsResult>
where
    F: FnMut(&mut dyn FnMut(Option<&str>)) -> Result<()>,
{
    let mut passes = 1u64;
    loop {
        let mut table = ResultTable::new(1, 1);
        let mut overflow = false;
        for part in 0..passes {
            if cancel.load(Ordering::SeqCst) {
                return Err(Error::Cancelled);
            }
            let mut groups: HashMap<String, u64> = HashMap::new();
            let mut charge = Charge {
                cache,
                held: Vec::new(),
                reserved: 0,
                used: 0,
            };
            let mut over = false;
            read(&mut |v: Option<&str>| {
                if over {
                    return;
                }
                let k = v.unwrap_or("");
                if passes > 1 && xxh3_64(k.as_bytes()) % passes != part {
                    return;
                }
                if let Some(c) = groups.get_mut(k) {
                    *c += 1;
                    return;
                }
                if charge.add(k.len() as u64 + 64 + 72).is_err() {
                    over = true;
                    return;
                }
                groups.insert(k.to_string(), 1);
            })?;
            if over {
                overflow = true;
                break;
            }
            for (k, c) in groups {
                table.push(cache, scratch, &k, &[c as f64])?;
            }
        }
        if overflow {
            if passes >= MAX_PASSES {
                return Err(Error::Invalid(
                    "counting values exceeds the memory budget even in 4096 passes".into(),
                ));
            }
            passes *= 4;
            continue;
        }
        table.finish(cache)?;
        return Ok(StatsResult {
            columns: vec![label.to_string(), "count".to_string()],
            exact: true,
            passes,
            table,
        });
    }
}

/// `top n col`: most frequent values, count descending.
pub fn top(
    st: &Store,
    m: &MatchSet,
    n: usize,
    col: &str,
    cache: &ChunkCache,
    cancel: &AtomicBool,
) -> Result<StatsResult> {
    let r = run_stats(
        st,
        m,
        &[Agg {
            func: AggFn::Count,
            col: None,
        }],
        &[col.to_string()],
        cache,
        cancel,
    )?;
    // keep the n best while streaming through (works for spilled results too)
    let mut best: Vec<(Vec<String>, Vec<f64>)> = Vec::with_capacity(n * 2 + 1);
    let cmp = |a: &(Vec<String>, Vec<f64>), b: &(Vec<String>, Vec<f64>)| {
        b.1[0].partial_cmp(&a.1[0]).unwrap().then(a.0.cmp(&b.0))
    };
    for g in r.iter() {
        best.push(g);
        if best.len() > n * 2 + 32 {
            best.select_nth_unstable_by(n, cmp);
            best.truncate(n);
        }
    }
    best.sort_by(cmp);
    best.truncate(n);
    let mut table = ResultTable::new(1, 1);
    let scratch = st.dir().join("_tmp");
    for (k, v) in &best {
        table.push(cache, &scratch, &k.join("\u{1f}"), v)?;
    }
    Ok(StatsResult {
        columns: r.columns.clone(),
        exact: true,
        passes: r.passes,
        table,
    })
}

#[derive(Debug, Clone, Default)]
pub struct Series {
    pub span_us: i64,
    /// `(bucket start µs, by-value, aggregate)` sorted by bucket then by-value.
    pub points: Vec<(i64, Option<String>, f64)>,
}

pub fn timechart(
    st: &Store,
    m: &MatchSet,
    span_us: i64,
    agg: &Agg,
    by: Option<&str>,
    cache: &ChunkCache,
    cancel: &AtomicBool,
) -> Result<Series> {
    let tc =
        st.schema()
            .time_col
            .ok_or_else(|| Error::Invalid("this source has no time column".into()))? as u32;
    let by_id = by
        .map(|b| resolve_column(st, b).map(|i| i as u32))
        .transpose()?;
    let agg_id = agg
        .col
        .as_ref()
        .map(|c| resolve_column(st, c).map(|i| i as u32))
        .transpose()?;
    let mut map: HashMap<(i64, Option<String>), St> = HashMap::new();
    let mut charge = Charge {
        cache,
        held: Vec::new(),
        reserved: 0,
        used: 0,
    };
    for (si, sm) in m.segs.iter().enumerate() {
        if matches!(sm, SegMatch::None) {
            continue;
        }
        if cancel.load(Ordering::SeqCst) {
            return Err(Error::Cancelled);
        }
        let rows = st.segments()[si].meta().rows;
        let Some(tcol) = cache.get(st, si, tc)? else {
            continue;
        };
        let bcol = by_id.map(|b| cache.get(st, si, b)).transpose()?.flatten();
        let acol = agg_id.map(|a| cache.get(st, si, a)).transpose()?.flatten();
        let unit = st.schema().time_unit_us.unwrap_or(1);
        for r in match_rows(sm, rows) {
            let Some(t) = tcol.int(r) else { continue };
            let t = t.saturating_mul(unit);
            let bucket = t.div_euclid(span_us) * span_us;
            let bv = bcol
                .as_ref()
                .and_then(|c| c.text(r))
                .map(|s| s.into_owned());
            let k = (bucket, bv);
            if !map.contains_key(&k) {
                charge
                    .add(96 + k.1.as_ref().map_or(0, |s| s.len() as u64))
                    .map_err(|_| Error::Invalid("timechart has too many series for the memory budget; use a larger span".into()))?;
            }
            let s = map.entry(k).or_insert_with(|| St::new(agg.func));
            match agg.func {
                AggFn::Count => s.count += 1,
                AggFn::Dc => {
                    if let Some(t) = acol.as_ref().and_then(|c| c.text(r)) {
                        s.dc.as_mut().unwrap().insert(xxh3_64(t.as_bytes()));
                    }
                }
                _ => {
                    if let Some(v) = acol.as_ref().and_then(|c| c.float(r)) {
                        s.n += 1;
                        s.sum += v;
                        s.min = s.min.min(v);
                        s.max = s.max.max(v);
                    }
                }
            }
        }
    }
    let mut points: Vec<(i64, Option<String>, f64)> = map
        .into_iter()
        .map(|((b, k), s)| (b, k, s.value(agg.func)))
        .collect();
    points.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    Ok(Series { span_us, points })
}

#[derive(Debug, Clone, PartialEq)]
enum SV {
    Num(f64),
    Text(String),
    Null,
}

fn cmp_sv(a: &SV, b: &SV, desc: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering as O;
    let o = match (a, b) {
        (SV::Null, SV::Null) => return O::Equal,
        (SV::Null, _) => return O::Greater, // nulls last either way
        (_, SV::Null) => return O::Less,
        (SV::Num(x), SV::Num(y)) => x.partial_cmp(y).unwrap_or(O::Equal),
        (SV::Text(x), SV::Text(y)) => x.cmp(y),
        (SV::Num(_), SV::Text(_)) => O::Less,
        (SV::Text(_), SV::Num(_)) => O::Greater,
    };
    if desc {
        o.reverse()
    } else {
        o
    }
}

/// The first `k` matching rows in the given sort order — one streaming pass,
/// memory O(k), no full sort of the data.
pub fn top_k_sorted(
    st: &Store,
    m: &MatchSet,
    keys: &[(String, bool)],
    k: usize,
    cache: &ChunkCache,
    cancel: &AtomicBool,
) -> Result<Vec<u64>> {
    let ids: Vec<(u32, bool, bool)> = keys
        .iter()
        .map(|(c, d)| {
            let i = resolve_column(st, c)?;
            let numeric = matches!(
                st.schema().columns[i].ty,
                ColType::Int | ColType::Float | ColType::Time
            );
            Ok((i as u32, *d, numeric))
        })
        .collect::<Result<_>>()?;
    let cmp = |a: &(Vec<SV>, u64), b: &(Vec<SV>, u64)| {
        for (i, (_, desc, _)) in ids.iter().enumerate() {
            let o = cmp_sv(&a.0[i], &b.0[i], *desc);
            if o != std::cmp::Ordering::Equal {
                return o;
            }
        }
        a.1.cmp(&b.1)
    };
    let mut cand: Vec<(Vec<SV>, u64)> = Vec::with_capacity(k * 2 + 1);
    for (si, sm) in m.segs.iter().enumerate() {
        if matches!(sm, SegMatch::None) {
            continue;
        }
        if cancel.load(Ordering::SeqCst) {
            return Err(Error::Cancelled);
        }
        let meta = st.segments()[si].meta();
        let cols: Vec<Option<Arc<DecodedColumn>>> = ids
            .iter()
            .map(|(c, _, _)| cache.get(st, si, *c))
            .collect::<Result<_>>()?;
        for r in match_rows(sm, meta.rows) {
            let key: Vec<SV> = cols
                .iter()
                .zip(&ids)
                .map(|(c, (_, _, numeric))| match c {
                    None => SV::Null,
                    Some(c) if *numeric => {
                        match c.float(r).or_else(|| c.int(r).map(|v| v as f64)) {
                            Some(v) => SV::Num(v),
                            None => c.text(r).map_or(SV::Null, |t| SV::Text(t.into_owned())),
                        }
                    }
                    Some(c) => c.text(r).map_or(SV::Null, |t| SV::Text(t.into_owned())),
                })
                .collect();
            cand.push((key, meta.row_start + r as u64));
            if cand.len() > k * 2 + 64 {
                cand.select_nth_unstable_by(k, cmp);
                cand.truncate(k);
            }
        }
    }
    cand.sort_by(cmp);
    cand.truncate(k);
    Ok(cand.into_iter().map(|(_, r)| r).collect())
}
