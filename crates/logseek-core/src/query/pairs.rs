//! Rare pairs of two columns, A and B, with exact counts:
//!
//! - a value of B rarely seen with a value of A that is otherwise common
//!   ("Acme, in 5,000 rows, paid into ACC-9 once": 1 in 5,000 of Acme's
//!   rows). Ranked by how rare the pair is beyond the variety A already
//!   has: bits of rarity (how small a share of A's rows) less the bits A's
//!   own number of different partners explains. A value with two partners
//!   that breaks its habit outranks one with a new partner every time, which
//!   has no habit to break. An empty A is not something with habits and is
//!   left out.
//! - values of B seen with the fewest different values of A ("/admin/export.sql:
//!   1 of 501 clients"), however many rows they have, so one busy client
//!   cannot hide behind its own row count.
//!
//! Nothing is known in advance; every count comes from the rows. Empty
//! cells are values like any other.

use super::exec::{each_in, over_blocks, ChunkCache, FormulaSet, MatchSet, SegMatch};
use crate::error::Result;
use crate::store::Store;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// A pair of values and how often it happens.
#[derive(Clone, Debug, PartialEq)]
pub struct PairLine {
    pub a: Option<String>,
    pub b: Option<String>,
    /// Rows with both.
    pub rows: u64,
    /// Rows with this value of A, and how many different values of B it has.
    pub a_rows: u64,
    pub partners: u64,
    /// How rare the pair is beyond A's own variety, in bits (higher: rarer).
    pub surprise: f64,
}

/// A value of B and how many different values of A it is seen with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerLine {
    pub b: Option<String>,
    pub peers: u64,
    pub rows: u64,
}

pub struct Pairs {
    /// Every pair, the rarest for its value of A first.
    pub unusual: Vec<PairLine>,
    /// Every value of B, the fewest values of A first.
    pub peers: Vec<PeerLine>,
    /// Different values of A, and rows read.
    pub a_values: u64,
    pub total: u64,
}

fn key_of(v: Option<&str>) -> u64 {
    let Some(v) = v.filter(|s| !s.is_empty()) else {
        return 0;
    };
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &x in v.as_bytes() {
        h ^= x as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h ^= h >> 29;
    h | 1
}

/// Each row's text in `col` for the rows of `m` in block `si`.
fn texts(
    st: &Store,
    cache: &ChunkCache,
    fs: &FormulaSet,
    si: usize,
    m: &SegMatch,
    col: usize,
    mut f: impl FnMut(usize, Option<&str>),
) -> Result<()> {
    if col >= st.schema().len() {
        let mut segs = vec![SegMatch::None; st.segments().len()];
        segs[si] = m.clone();
        let one = MatchSet::from_segs(segs);
        let base = st.segments()[si].meta().row_start;
        let mut start = 0;
        while start < one.count() {
            let p = super::exec::page_with(st, &one, start, 20_000, &[col], fs, cache)?;
            if p.rows.is_empty() {
                break;
            }
            start += p.rows.len() as u64;
            for (g, cells) in p.rows.iter().zip(&p.cells) {
                f(
                    (g - base) as usize,
                    cells.first().and_then(|c| c.as_deref()),
                );
            }
        }
        return Ok(());
    }
    let blk = cache.block(st, si)?;
    let c = blk.column(col as u32)?;
    each_in(m, |r| match &c {
        None => f(r, None),
        Some(c) => f(r, c.text(r).as_deref()),
    });
    Ok(())
}

/// Count every pair of values of columns `a` and `b` on the rows of `m`.
#[allow(clippy::too_many_arguments)]
pub fn rare_pairs(
    st: &Store,
    m: &MatchSet,
    a: usize,
    b: usize,
    fs: &FormulaSet,
    cache: &ChunkCache,
    cancel: &AtomicBool,
    done: &AtomicU64,
) -> Result<Pairs> {
    struct Part {
        pairs: HashMap<(u64, u64), u64>,
        names: HashMap<u64, String>,
    }
    let parts = over_blocks(
        st,
        cache,
        |si| m.segs[si].count() > 0,
        cancel,
        || Part {
            pairs: HashMap::new(),
            names: HashMap::new(),
        },
        |p, si| {
            let sm = &m.segs[si];
            let rows = st.segments()[si].meta().rows as usize;
            let mut ka = vec![0u64; rows];
            texts(st, cache, fs, si, sm, a, |r, v| {
                let k = key_of(v);
                if k != 0 && !p.names.contains_key(&k) {
                    p.names.insert(k, v.unwrap_or_default().to_string());
                }
                ka[r] = k;
            })?;
            texts(st, cache, fs, si, sm, b, |r, v| {
                let k = key_of(v);
                if k != 0 && !p.names.contains_key(&k) {
                    p.names.insert(k, v.unwrap_or_default().to_string());
                }
                *p.pairs.entry((ka[r], k)).or_default() += 1;
            })?;
            done.fetch_add(sm.count(), Ordering::Relaxed);
            Ok(())
        },
    )?;
    let mut pairs: HashMap<(u64, u64), u64> = HashMap::new();
    let mut names: HashMap<u64, String> = HashMap::new();
    for p in parts {
        for (k, n) in p.pairs {
            *pairs.entry(k).or_default() += n;
        }
        for (k, v) in p.names {
            names.entry(k).or_insert(v);
        }
    }
    let text = |k: u64| (k != 0).then(|| names.get(&k).cloned().unwrap_or_default());
    // per value of A (an empty A has no habits: not counted)
    let mut a_rows: HashMap<u64, (u64, u64)> = HashMap::new(); // (rows, partners)
    let mut b_rows: HashMap<u64, (u64, u64)> = HashMap::new(); // (peers, rows)
    for (&(ka, kb), &n) in &pairs {
        let e = b_rows.entry(kb).or_default();
        e.1 += n;
        if ka == 0 {
            continue;
        }
        let a = a_rows.entry(ka).or_default();
        a.0 += n;
        a.1 += 1;
        e.0 += 1;
    }
    let mut unusual: Vec<PairLine> = pairs
        .iter()
        .filter(|((ka, _), _)| *ka != 0)
        .map(|(&(ka, kb), &n)| {
            let (ar, partners) = a_rows[&ka];
            PairLine {
                a: text(ka),
                b: text(kb),
                rows: n,
                a_rows: ar,
                partners,
                surprise: (ar as f64 / n as f64).log2() - (partners as f64).log2(),
            }
        })
        .collect();
    // rarest beyond A's own variety first; then the commonest A, then by the values
    unusual.sort_by(|x, y| {
        y.surprise
            .total_cmp(&x.surprise)
            .then(y.a_rows.cmp(&x.a_rows))
            .then_with(|| x.a.cmp(&y.a))
            .then_with(|| x.b.cmp(&y.b))
    });
    // a value of B only ever seen where A is empty has no peers to count
    let mut peers: Vec<PeerLine> = b_rows
        .iter()
        .filter(|(_, &(p, _))| p > 0)
        .map(|(&kb, &(p, n))| PeerLine {
            b: text(kb),
            peers: p,
            rows: n,
        })
        .collect();
    peers.sort_by(|x, y| {
        x.peers
            .cmp(&y.peers)
            .then(y.rows.cmp(&x.rows))
            .then_with(|| x.b.cmp(&y.b))
    });
    Ok(Pairs {
        unusual,
        peers,
        a_values: a_rows.len() as u64,
        total: m.count(),
    })
}
