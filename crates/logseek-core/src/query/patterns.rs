//! Patterns in a column: lines of free text folded into patterns, the parts
//! that vary marked, in the manner of fixed-depth log template mining. Words
//! are what lies between spaces; a word that starts with a digit, or is at
//! least half digits, is taken as a number that may vary (a character class,
//! nothing known about any kind of value). Lines with the same number of
//! words and the same first word fold together when enough of their words
//! are the same; the words that differ become "varies". Only words that are
//! the same count: a place that already varies does not, so a pattern never
//! swallows lines merely by varying everywhere.
//!
//! Blocks are folded side by side and put together in file order, so the
//! answer is the same every time. A second read gives every row to the most
//! specific pattern it fits, so a pattern's count is exactly the rows it
//! shows. There is no limit on the number of patterns.

use super::exec::{each_in, over_blocks, ChunkCache, FormulaSet, MatchSet, SegMatch};
use crate::error::Result;
use crate::store::Store;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// How alike two lines must be to fold together: the share of their words
/// that agree (0.5 by default).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PatternOpts {
    pub alike: f64,
}

impl Default for PatternOpts {
    fn default() -> Self {
        PatternOpts { alike: 0.5 }
    }
}

/// One word of a pattern.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tok {
    /// A word every line of the pattern has, as it is.
    Word(String),
    /// A word with digits in it; the same in every line so far (`Some`) or not.
    Num(Option<String>),
    /// Differs from line to line.
    Varies,
}

/// A line's words, as the pattern sees them.
#[derive(Clone, Debug, PartialEq, Eq)]
enum LTok<'a> {
    Word(&'a str),
    Num(&'a str),
}

/// A number that may vary: starts with a digit, or is at least half digits
/// (counting letters and digits only). "system32" is a word; "10.0.1.2",
/// "4s" and "000A" are numbers.
fn numlike(w: &str) -> bool {
    let b = w.as_bytes();
    if b.first().is_some_and(|c| c.is_ascii_digit()) {
        return true;
    }
    let digits = b.iter().filter(|c| c.is_ascii_digit()).count();
    let alnum = b.iter().filter(|c| c.is_ascii_alphanumeric()).count();
    digits > 0 && digits * 2 >= alnum
}

fn line_toks(s: &str) -> Vec<LTok<'_>> {
    s.split_ascii_whitespace()
        .map(|w| {
            if numlike(w) {
                LTok::Num(w)
            } else {
                LTok::Word(w)
            }
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    pub toks: Vec<Tok>,
    /// Rows given to this pattern (exact, after the second read).
    pub rows: u64,
    /// One line that fits it, as an example.
    pub example: String,
}

/// The pattern in words: "Connection to [varies] timed out after [varies] ms".
pub fn pattern_words(p: &Pattern) -> String {
    p.toks
        .iter()
        .map(|t| match t {
            Tok::Word(w) | Tok::Num(Some(w)) => w.as_str(),
            Tok::Num(None) | Tok::Varies => "[varies]",
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Every pattern, most rows first; and the rows with nothing in the column.
pub struct Patterns {
    pub patterns: Vec<Pattern>,
    pub empty: u64,
    alike: f64,
}

/// Is this pattern word the same as this line word (for folding)? A place
/// that varies is not the same as anything.
fn agrees(t: &Tok, l: &LTok) -> bool {
    match (t, l) {
        (Tok::Word(a), LTok::Word(b)) => a == b,
        (Tok::Num(Some(a)), LTok::Num(b)) => a == b,
        _ => false,
    }
}

/// Does this line fit the pattern exactly (for giving rows to patterns)?
fn fits(p: &[Tok], l: &[LTok]) -> bool {
    p.len() == l.len()
        && p.iter().zip(l).all(|(t, w)| match (t, w) {
            (Tok::Varies, _) => true,
            (Tok::Word(a), LTok::Word(b)) => a == b,
            (Tok::Num(None), LTok::Num(_)) => true,
            (Tok::Num(Some(a)), LTok::Num(b)) => a == b,
            _ => false,
        })
}

/// How many words of a pattern hold a line down (a more specific pattern wins).
fn binding(p: &[Tok]) -> usize {
    p.iter()
        .filter(|t| matches!(t, Tok::Word(_) | Tok::Num(Some(_))))
        .count()
}

fn absorb(p: &mut [Tok], l: &[LTok]) {
    for (t, w) in p.iter_mut().zip(l) {
        let next = match (&*t, w) {
            (Tok::Varies, _) => Tok::Varies,
            (Tok::Word(a), LTok::Word(b)) if a == b => continue,
            (Tok::Num(Some(a)), LTok::Num(b)) if a == b => continue,
            (Tok::Num(_), LTok::Num(_)) => Tok::Num(None),
            _ => Tok::Varies,
        };
        *t = next;
    }
}

fn join(p: &mut [Tok], q: &[Tok]) {
    for (t, u) in p.iter_mut().zip(q) {
        let next = match (&*t, u) {
            (a, b) if a == b => continue,
            (Tok::Num(_), Tok::Num(_)) => Tok::Num(None),
            _ => Tok::Varies,
        };
        *t = next;
    }
}

fn key_of(first: Option<&Tok>) -> String {
    match first {
        Some(Tok::Word(w)) => w.clone(),
        _ => String::new(),
    }
}

/// Patterns being folded, found by (words, first word).
#[derive(Default)]
struct Fold {
    pats: Vec<(Vec<Tok>, String)>,
    by: HashMap<(usize, String), Vec<usize>>,
}

impl Fold {
    fn candidates(&self, len: usize, first: &str) -> impl Iterator<Item = usize> + '_ {
        let a = self.by.get(&(len, first.to_string())).into_iter().flatten();
        let b = if first.is_empty() {
            None
        } else {
            self.by.get(&(len, String::new()))
        };
        a.chain(b.into_iter().flatten()).copied()
    }

    fn rekey(&mut self, i: usize, old: String) {
        let new = key_of(self.pats[i].0.first());
        if new != old {
            let len = self.pats[i].0.len();
            if let Some(v) = self.by.get_mut(&(len, old)) {
                v.retain(|&j| j != i);
            }
            self.by.entry((len, new)).or_default().push(i);
        }
    }

    fn add_line(&mut self, line: &str, alike: f64) {
        let l = line_toks(line);
        if l.is_empty() {
            return;
        }
        let first = match &l[0] {
            LTok::Word(w) => w.to_string(),
            LTok::Num(_) => String::new(),
        };
        let mut best: Option<(usize, usize)> = None; // (pattern, words agreeing)
        for i in self.candidates(l.len(), &first) {
            let n = self.pats[i]
                .0
                .iter()
                .zip(&l)
                .filter(|(t, w)| agrees(t, w))
                .count();
            if best.is_none_or(|(_, m)| n > m) {
                best = Some((i, n));
            }
        }
        match best {
            Some((i, n)) if n as f64 >= alike * l.len() as f64 => {
                let old = key_of(self.pats[i].0.first());
                absorb(&mut self.pats[i].0, &l);
                self.rekey(i, old);
            }
            _ => {
                let toks: Vec<Tok> = l
                    .iter()
                    .map(|w| match w {
                        LTok::Word(w) => Tok::Word(w.to_string()),
                        LTok::Num(n) => Tok::Num(Some(n.to_string())),
                    })
                    .collect();
                let i = self.pats.len();
                self.by
                    .entry((toks.len(), key_of(toks.first())))
                    .or_default()
                    .push(i);
                self.pats.push((toks, line.to_string()));
            }
        }
    }

    fn add_pattern(&mut self, p: Vec<Tok>, example: String, alike: f64) {
        let first = key_of(p.first());
        let mut best: Option<(usize, usize)> = None;
        for i in self.candidates(p.len(), &first) {
            let q = &self.pats[i].0;
            let n = q
                .iter()
                .zip(&p)
                .filter(|(a, b)| a == b && matches!(a, Tok::Word(_) | Tok::Num(Some(_))))
                .count();
            if best.is_none_or(|(_, m)| n > m) {
                best = Some((i, n));
            }
        }
        match best {
            Some((i, n)) if n as f64 >= alike * p.len() as f64 => {
                let old = key_of(self.pats[i].0.first());
                join(&mut self.pats[i].0, &p);
                self.rekey(i, old);
            }
            _ => {
                let i = self.pats.len();
                self.by.entry((p.len(), first)).or_default().push(i);
                self.pats.push((p, example));
            }
        }
    }
}

/// Each row's text in `col` for the rows of `m` in block `si`.
fn each_text(
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

/// Find the most specific pattern a line fits, by (words, first word).
struct Index<'a> {
    pats: &'a [Pattern],
    by: HashMap<(usize, String), Vec<usize>>,
}

impl<'a> Index<'a> {
    fn new(pats: &'a [Pattern]) -> Index<'a> {
        let mut by: HashMap<(usize, String), Vec<usize>> = HashMap::new();
        for (i, p) in pats.iter().enumerate() {
            by.entry((p.toks.len(), key_of(p.toks.first())))
                .or_default()
                .push(i);
        }
        Index { pats, by }
    }
    fn find(&self, line: &str) -> Option<usize> {
        let l = line_toks(line);
        if l.is_empty() {
            return None;
        }
        let first = match &l[0] {
            LTok::Word(w) => w.to_string(),
            LTok::Num(_) => String::new(),
        };
        let mut best: Option<(usize, usize)> = None;
        let a = self.by.get(&(l.len(), first.clone())).into_iter().flatten();
        let b = if first.is_empty() {
            None
        } else {
            self.by.get(&(l.len(), String::new()))
        };
        for &i in a.chain(b.into_iter().flatten()) {
            let p = &self.pats[i].toks;
            if fits(p, &l) {
                let n = binding(p);
                // the most specific; on a tie, by the words themselves (so the
                // choice does not depend on the order of the list)
                if best.is_none_or(|(j, m)| n > m || (n == m && p < &self.pats[j].toks)) {
                    best = Some((i, n));
                }
            }
        }
        best.map(|(i, _)| i)
    }
}

/// Fold the column's lines on the rows of `m` into patterns, with exact
/// counts. `done` counts rows read (two reads).
#[allow(clippy::too_many_arguments)]
pub fn patterns(
    st: &Store,
    m: &MatchSet,
    col: usize,
    o: &PatternOpts,
    fs: &FormulaSet,
    cache: &ChunkCache,
    cancel: &AtomicBool,
    done: &AtomicU64,
) -> Result<Patterns> {
    let alike = o.alike.clamp(0.0, 1.0);
    // 1. each block folded on its own, side by side
    let parts = over_blocks(
        st,
        cache,
        |si| m.segs[si].count() > 0,
        cancel,
        Vec::<(usize, Fold)>::new,
        |out, si| {
            let mut f = Fold::default();
            each_text(st, cache, fs, si, &m.segs[si], col, |_, v| {
                if let Some(v) = v {
                    f.add_line(v, alike);
                }
            })?;
            out.push((si, f));
            done.fetch_add(m.segs[si].count(), Ordering::Relaxed);
            Ok(())
        },
    )?;
    // put together in file order: the same answer every time
    let mut blocks: Vec<(usize, Fold)> = parts.into_iter().flatten().collect();
    blocks.sort_by_key(|b| b.0);
    let mut all = Fold::default();
    for (_, f) in blocks {
        for (p, ex) in f.pats {
            all.add_pattern(p, ex, alike);
        }
    }
    let mut pats: Vec<Pattern> = all
        .pats
        .into_iter()
        .map(|(toks, example)| Pattern {
            toks,
            rows: 0,
            example,
        })
        .collect();
    // 2. every row given to the most specific pattern it fits, counted exactly
    let idx = Index::new(&pats);
    let parts = over_blocks(
        st,
        cache,
        |si| m.segs[si].count() > 0,
        cancel,
        || (HashMap::<usize, u64>::new(), 0u64),
        |(n, empty), si| {
            each_text(st, cache, fs, si, &m.segs[si], col, |_, v| {
                match v.and_then(|v| idx.find(v)) {
                    Some(i) => *n.entry(i).or_default() += 1,
                    None => *empty += 1,
                }
            })?;
            done.fetch_add(m.segs[si].count(), Ordering::Relaxed);
            Ok(())
        },
    )?;
    let mut empty = 0;
    for (n, e) in parts {
        empty += e;
        for (i, k) in n {
            pats[i].rows += k;
        }
    }
    // a pattern that no row ended up in (all went to more specific ones) is dropped
    pats.retain(|p| p.rows > 0);
    pats.sort_by(|a, b| {
        b.rows
            .cmp(&a.rows)
            .then_with(|| pattern_words(a).cmp(&pattern_words(b)))
    });
    Ok(Patterns {
        patterns: pats,
        empty,
        alike,
    })
}

/// The ids of the rows given to pattern `which` (the same rule as the
/// counts), in file order.
#[allow(clippy::too_many_arguments)]
pub fn pattern_rows(
    st: &Store,
    m: &MatchSet,
    col: usize,
    p: &Patterns,
    which: usize,
    fs: &FormulaSet,
    cache: &ChunkCache,
    cancel: &AtomicBool,
) -> Result<Vec<u64>> {
    let _ = p.alike;
    let idx = Index::new(&p.patterns);
    let parts = over_blocks(
        st,
        cache,
        |si| m.segs[si].count() > 0,
        cancel,
        Vec::<u64>::new,
        |ids, si| {
            let base = st.segments()[si].meta().row_start;
            each_text(st, cache, fs, si, &m.segs[si], col, |r, v| {
                if v.and_then(|v| idx.find(v)) == Some(which) {
                    ids.push(base + r as u64);
                }
            })
        },
    )?;
    let mut ids: Vec<u64> = parts.into_iter().flatten().collect();
    ids.sort_unstable();
    Ok(ids)
}
