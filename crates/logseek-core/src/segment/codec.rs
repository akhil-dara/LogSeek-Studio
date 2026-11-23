//! Column payload encoding/decoding. Every payload is one zstd frame of:
//! `u32 rows · u8 has_nulls · [null bitmap] · <encoding body> · side values`.

use super::bloom::Bloom;
use super::format::{mk_value, render_decimals, render_mk};
use super::{fmt_float, ColFormat, ColMeta, ColumnBuilder, EncodedColumn};
use crate::budget::Reservation;
use crate::error::{Error, Result};
use crate::types::{fmt_time, ColType};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Encoding {
    Dict,
    Plain,
    Int,
    Float,
    /// Exact decimals: integer mantissa + decimal places per row.
    Decimal,
    Bool,
}

const ZSTD_LEVEL: i32 = 1;

// ─────────────── byte helpers ───────────────

struct W(Vec<u8>);
impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v)
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes())
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes())
    }
    fn bytes(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b)
    }
    fn varint(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.0.push((v as u8) | 0x80);
            v >>= 7;
        }
        self.0.push(v as u8);
    }
    fn words(&mut self, w: &[u64], n: usize) {
        for i in 0..n {
            self.u64(*w.get(i).unwrap_or(&0));
        }
    }
}

struct R<'a> {
    b: &'a [u8],
    p: usize,
}
impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.p + n > self.b.len() {
            return Err(Error::Corrupt("column payload truncated".into()));
        }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn varint(&mut self) -> Result<u64> {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let b = self.u8()?;
            v |= ((b & 0x7f) as u64) << shift;
            if b < 0x80 {
                return Ok(v);
            }
            shift += 7;
            if shift > 63 {
                return Err(Error::Corrupt("varint overflow".into()));
            }
        }
    }
    fn words(&mut self, n: usize) -> Result<Vec<u64>> {
        (0..n).map(|_| self.u64()).collect()
    }
    fn u32s(&mut self, n: usize) -> Result<Vec<u32>> {
        let raw = self.take(n * 4)?;
        Ok(raw
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect())
    }
}

#[inline]
fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}
#[inline]
fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

fn words_for(rows: u32) -> usize {
    (rows as usize).div_ceil(64)
}

// ─────────────── encode ───────────────

pub(super) fn encode(b: ColumnBuilder) -> EncodedColumn {
    let rows = b.rows;
    let mut w = W(Vec::with_capacity(b.bytes.len() + b.ints.len() * 2 + 64));
    w.u32(rows);
    if b.null_count > 0 {
        w.u8(1);
        w.words(&b.nulls, words_for(rows));
    } else {
        w.u8(0);
    }

    let mut dict_len = None;
    let mut bloom = None;
    let enc = match b.ty {
        ColType::Text => {
            let use_dict = match &b.dict {
                Some(d) => d.len() <= super::DICT_MAX && (d.len() as u64) * 2 <= rows.max(1) as u64,
                None => false,
            };
            if use_dict {
                let d = b.dict.as_ref().unwrap();
                let mut entries: Vec<&str> = vec![""; d.len()];
                for (k, &c) in d {
                    entries[c as usize] = k;
                }
                w.u32(entries.len() as u32);
                let mut off = 0u32;
                w.u32(0);
                for e in &entries {
                    off += e.len() as u32;
                    w.u32(off);
                }
                for e in &entries {
                    w.bytes(e.as_bytes());
                }
                let width: u8 = if entries.len() <= 256 { 1 } else { 2 };
                w.u8(width);
                for &c in &b.codes {
                    let c = if c == u32::MAX { 0 } else { c };
                    if width == 1 {
                        w.u8(c as u8)
                    } else {
                        w.bytes(&(c as u16).to_le_bytes())
                    }
                }
                dict_len = Some(entries.len() as u32);
                let side_iter = b.side.iter().map(|(_, s)| s.as_bytes());
                bloom = Some(
                    Bloom::build(entries.iter().map(|s| s.as_bytes()).chain(side_iter)).to_bytes(),
                );
                Encoding::Dict
            } else {
                for &o in &b.offsets {
                    w.u32(o);
                }
                w.bytes(&b.bytes);
                let offs = &b.offsets;
                let bytes = &b.bytes;
                let it =
                    (0..rows as usize).map(move |i| &bytes[offs[i] as usize..offs[i + 1] as usize]);
                bloom = Some(Bloom::build(it).to_bytes());
                Encoding::Plain
            }
        }
        ColType::Int | ColType::Time => {
            let mut prev = 0i64;
            for &v in &b.ints {
                w.varint(zigzag(v.wrapping_sub(prev)));
                prev = v;
            }
            if b.ty == ColType::Int && b.null_count < rows {
                let strs: Vec<String> = b.ints.iter().map(|v| v.to_string()).collect();
                let it = strs
                    .iter()
                    .map(|s| s.as_bytes())
                    .chain(b.side.iter().map(|(_, s)| s.as_bytes()));
                bloom = Some(Bloom::build(it).to_bytes());
            }
            Encoding::Int
        }
        ColType::Float if !b.decs.is_empty() => {
            // exact decimals: mantissa varints + decimal places; odd rows keep their f64
            for i in 0..rows as usize {
                w.varint(zigzag(*b.dec_m.get(i).unwrap_or(&0)));
            }
            for i in 0..rows as usize {
                w.u8(*b.decs.get(i).unwrap_or(&0));
            }
            w.u32(b.ex_num.len() as u32);
            for (r, f) in &b.ex_num {
                w.u32(*r);
                w.bytes(&f.to_le_bytes());
            }
            Encoding::Decimal
        }
        ColType::Float => {
            for &f in &b.floats {
                w.bytes(&f.to_le_bytes());
            }
            w.u8(0);
            Encoding::Float
        }
        ColType::Bool => {
            w.words(&b.bools, words_for(rows));
            Encoding::Bool
        }
    };

    w.u32(b.side.len() as u32);
    for (r, s) in &b.side {
        w.u32(*r);
        w.u32(s.len() as u32);
        w.bytes(s.as_bytes());
    }
    w.u32(b.raw_ex.len() as u32);
    for (r, s) in &b.raw_ex {
        w.u32(*r);
        w.u32(s.len() as u32);
        w.bytes(s.as_bytes());
    }
    if matches!(b.ty, ColType::Float | ColType::Time | ColType::Bool)
        && (!b.raw_ex.is_empty() || !b.side.is_empty())
    {
        // a filter over only the verbatim rows; the format rule covers the rest
        let it = b
            .raw_ex
            .iter()
            .chain(b.side.iter())
            .map(|(_, s)| s.as_bytes());
        bloom = Some(Bloom::build(it).to_bytes());
    }
    let fmt = match b.ty {
        ColType::Time => b.tmpl.clone().map(ColFormat::Time),
        ColType::Bool => b.bool_case.map(ColFormat::Bool),
        _ => None,
    };

    let raw = w.0;
    let raw_len = raw.len() as u64;
    let blob = zstd::bulk::compress(&raw, ZSTD_LEVEL).expect("zstd compress");
    if b.null_count >= rows {
        bloom = None; // all-null column: nothing to find
    }
    EncodedColumn {
        meta: ColMeta {
            col_id: b.col_id,
            ty: b.ty,
            enc,
            nulls: b.null_count,
            min_i: b.min_i,
            max_i: b.max_i,
            min_f: b.min_f,
            max_f: b.max_f,
            dict_len,
            side: b.side.len() as u32,
            ex: b.raw_ex.len() as u32,
            fmt,
            off: 0,
            len: 0,
            raw_len,
            bloom_off: 0,
            bloom_len: 0,
            in_source: false,
        },
        blob,
        bloom,
    }
}

// ─────────────── decode ───────────────

pub enum ColData {
    Dict {
        dict_off: Vec<u32>,
        dict_bytes: Vec<u8>,
        codes: Vec<u16>,
    },
    Plain {
        off: Vec<u32>,
        bytes: Vec<u8>,
    },
    Int(Vec<i64>),
    Float(Vec<f64>),
    /// value = m / 10^k; `k == 255` rows use `ex` (row, f64).
    Decimal {
        m: Vec<i64>,
        k: Vec<u8>,
        ex: Vec<(u32, f64)>,
    },
    Bool(Vec<u64>),
}

/// A decoded column chunk. Holds its budget reservation until dropped.
pub struct DecodedColumn {
    pub ty: ColType,
    pub rows: u32,
    nulls: Option<Vec<u64>>,
    data: ColData,
    side: Vec<(u32, Box<str>)>,
    fmt: Option<ColFormat>,
    decs: Option<Vec<u8>>,
    raw_ex: Vec<(u32, Box<str>)>,
    _res: Reservation,
}

pub(super) fn decoded_estimate(m: &ColMeta, rows: u32) -> u64 {
    m.raw_len + rows as u64 * 8 + 256
}

pub(super) fn decode(
    m: &ColMeta,
    rows: u32,
    blob: &[u8],
    res: Reservation,
) -> Result<DecodedColumn> {
    let raw = zstd::bulk::decompress(blob, m.raw_len as usize)
        .map_err(|e| Error::Corrupt(format!("zstd: {e}")))?;
    let mut r = R { b: &raw, p: 0 };
    let n = r.u32()?;
    if n != rows {
        return Err(Error::Corrupt(format!("row count {n} != {rows}")));
    }
    let nulls = if r.u8()? == 1 {
        Some(r.words(words_for(rows))?)
    } else {
        None
    };
    let mut decs: Option<Vec<u8>> = None;
    let data = match m.enc {
        Encoding::Dict => {
            let nd = r.u32()? as usize;
            let dict_off = r.u32s(nd + 1)?;
            let dict_bytes = r.take(*dict_off.last().unwrap_or(&0) as usize)?.to_vec();
            let width = r.u8()?;
            let codes: Vec<u16> = if width == 1 {
                r.take(rows as usize)?.iter().map(|&c| c as u16).collect()
            } else {
                r.take(rows as usize * 2)?
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect()
            };
            if codes.iter().any(|&c| c as usize >= nd.max(1)) {
                return Err(Error::Corrupt("dict code out of range".into()));
            }
            ColData::Dict {
                dict_off,
                dict_bytes,
                codes,
            }
        }
        Encoding::Plain => {
            let off = r.u32s(rows as usize + 1)?;
            let bytes = r.take(*off.last().unwrap_or(&0) as usize)?.to_vec();
            ColData::Plain { off, bytes }
        }
        Encoding::Int => {
            let mut v = Vec::with_capacity(rows as usize);
            let mut prev = 0i64;
            for _ in 0..rows {
                prev = prev.wrapping_add(unzigzag(r.varint()?));
                v.push(prev);
            }
            ColData::Int(v)
        }
        Encoding::Float => {
            let raw = r.take(rows as usize * 8)?;
            let v = raw
                .chunks_exact(8)
                .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
                .collect();
            if r.u8()? == 1 {
                decs = Some(r.take(rows as usize)?.to_vec());
            }
            ColData::Float(v)
        }
        Encoding::Decimal => {
            let mut m = Vec::with_capacity(rows as usize);
            for _ in 0..rows {
                m.push(unzigzag(r.varint()?));
            }
            let k = r.take(rows as usize)?.to_vec();
            let n = r.u32()? as usize;
            let mut ex = Vec::with_capacity(n);
            for _ in 0..n {
                let row = r.u32()?;
                ex.push((row, f64::from_le_bytes(r.take(8)?.try_into().unwrap())));
            }
            ColData::Decimal { m, k, ex }
        }
        Encoding::Bool => ColData::Bool(r.words(words_for(rows))?),
    };
    let ns = r.u32()? as usize;
    let mut side = Vec::with_capacity(ns);
    for _ in 0..ns {
        let row = r.u32()?;
        let len = r.u32()? as usize;
        let s = std::str::from_utf8(r.take(len)?)
            .map_err(|_| Error::Corrupt("side value utf8".into()))?;
        side.push((row, s.into()));
    }
    let ne = r.u32()? as usize;
    let mut raw_ex = Vec::with_capacity(ne);
    for _ in 0..ne {
        let row = r.u32()?;
        let len = r.u32()? as usize;
        let s = std::str::from_utf8(r.take(len)?)
            .map_err(|_| Error::Corrupt("raw value utf8".into()))?;
        raw_ex.push((row, s.into()));
    }
    Ok(DecodedColumn {
        ty: m.ty,
        rows,
        nulls,
        data,
        side,
        fmt: m.fmt.clone(),
        decs,
        raw_ex,
        _res: res,
    })
}

#[inline]
fn bit(words: &[u64], r: usize) -> bool {
    words.get(r / 64).is_some_and(|w| w & (1 << (r % 64)) != 0)
}

impl DecodedColumn {
    pub fn data(&self) -> &ColData {
        &self.data
    }

    #[inline]
    pub fn is_null(&self, r: usize) -> bool {
        self.nulls.as_ref().is_some_and(|n| bit(n, r))
    }

    fn side_value(&self, r: usize) -> Option<&str> {
        if self.side.is_empty() {
            return None;
        }
        self.side
            .binary_search_by_key(&(r as u32), |(row, _)| *row)
            .ok()
            .map(|i| &*self.side[i].1)
    }

    /// Text of a dictionary entry.
    pub fn dict_entry(&self, code: usize) -> Option<&str> {
        match &self.data {
            ColData::Dict {
                dict_off,
                dict_bytes,
                ..
            } => {
                let a = *dict_off.get(code)? as usize;
                let b = *dict_off.get(code + 1)? as usize;
                std::str::from_utf8(&dict_bytes[a..b]).ok()
            }
            _ => None,
        }
    }

    /// Raw text bytes of row `r` for text columns (no allocation).
    #[inline]
    pub fn text_bytes(&self, r: usize) -> Option<&[u8]> {
        if self.is_null(r) {
            return None;
        }
        match &self.data {
            ColData::Dict {
                dict_off,
                dict_bytes,
                codes,
            } => {
                let c = codes[r] as usize;
                Some(&dict_bytes[dict_off[c] as usize..dict_off[c + 1] as usize])
            }
            ColData::Plain { off, bytes } => Some(&bytes[off[r] as usize..off[r + 1] as usize]),
            _ => self.verbatim(r).map(|s| s.as_bytes()),
        }
    }

    /// Display text of row `r` (`None` = null).
    pub fn text(&self, r: usize) -> Option<Cow<'_, str>> {
        if self.is_null(r) {
            return None;
        }
        if let Some(s) = self.verbatim(r) {
            return Some(Cow::Borrowed(s));
        }
        match &self.data {
            ColData::Dict { .. } | ColData::Plain { .. } => {
                self.text_bytes(r).map(|b| String::from_utf8_lossy(b))
            }
            ColData::Int(v) => Some(Cow::Owned(match (&self.fmt, self.ty) {
                (Some(ColFormat::Time(t)), ColType::Time) => t.render(v[r]),
                (_, ColType::Time) => fmt_time(v[r]),
                _ => v[r].to_string(),
            })),
            ColData::Float(v) => Some(Cow::Owned(match self.decs.as_ref().map(|d| d[r]) {
                Some(k) if k != 255 => render_decimals(v[r], k),
                _ => fmt_float(v[r]),
            })),
            ColData::Decimal { m, k, ex } => Some(Cow::Owned(if k[r] != 255 {
                render_mk(m[r], k[r])
            } else {
                ex.iter()
                    .find(|(row, _)| *row as usize == r)
                    .map(|(_, f)| fmt_float(*f))
                    .unwrap_or_default()
            })),
            ColData::Bool(w) => Some(Cow::Borrowed(match &self.fmt {
                Some(ColFormat::Bool(c)) => c.render(bit(w, r)),
                _ if bit(w, r) => "true",
                _ => "false",
            })),
        }
    }

    /// Whether values are kept as a dictionary (one entry per distinct value).
    pub fn is_dict(&self) -> bool {
        matches!(self.data, ColData::Dict { .. })
    }

    /// The row's value as a dictionary key: 0 when empty, the entry number + 1
    /// when the row holds a dictionary entry; `None` when the row's value is
    /// kept apart (verbatim text) or the column is not a dictionary. Rows with
    /// the same key have the same value.
    pub fn dict_key(&self, r: usize) -> Option<u32> {
        let ColData::Dict { codes, .. } = &self.data else {
            return None;
        };
        if self.is_null(r) {
            return Some(0);
        }
        if self.verbatim(r).is_some() {
            return None;
        }
        Some(codes[r] as u32 + 1)
    }

    /// Source text kept verbatim for this row (type mismatch or unusual spelling).
    fn verbatim(&self, r: usize) -> Option<&str> {
        if !self.raw_ex.is_empty() {
            if let Ok(i) = self
                .raw_ex
                .binary_search_by_key(&(r as u32), |(row, _)| *row)
            {
                return Some(&self.raw_ex[i].1);
            }
        }
        self.side_value(r)
    }

    pub fn format(&self) -> Option<&ColFormat> {
        self.fmt.as_ref()
    }

    pub fn int(&self, r: usize) -> Option<i64> {
        if self.is_null(r) || self.side_value(r).is_some() {
            return None;
        }
        match &self.data {
            ColData::Int(v) => Some(v[r]),
            _ => None,
        }
    }

    pub fn float(&self, r: usize) -> Option<f64> {
        if self.is_null(r) || self.side_value(r).is_some() {
            return None;
        }
        match &self.data {
            ColData::Float(v) => Some(v[r]),
            ColData::Int(v) => Some(v[r] as f64),
            ColData::Decimal { m, k, ex } => {
                if k[r] != 255 {
                    Some(mk_value(m[r], k[r]))
                } else {
                    ex.binary_search_by_key(&(r as u32), |(row, _)| *row)
                        .ok()
                        .map(|i| ex[i].1)
                }
            }
            _ => None,
        }
    }

    pub fn bool(&self, r: usize) -> Option<bool> {
        if self.is_null(r) || self.side_value(r).is_some() {
            return None;
        }
        match &self.data {
            ColData::Bool(w) => Some(bit(w, r)),
            _ => None,
        }
    }

    pub fn has_side(&self) -> bool {
        !self.side.is_empty()
    }
}
