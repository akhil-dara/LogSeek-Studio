//! Keyword search across cells: every row that has the keyword, how many
//! cells have it in each column, and where it sits inside a cell — with
//! match-case, whole-word and one-column options. No limits: the rows are a
//! match set, read by page like any result.

use super::exec::{page_with, ChunkCache, FormulaSet, MatchSet};
use crate::error::{Error, Result};
use crate::store::Store;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchOpts {
    pub text: String,
    /// Capitals must match.
    pub case: bool,
    /// The keyword must stand alone (not inside a longer word).
    pub word: bool,
    /// Only in this column (an index into the columns searched).
    pub col: Option<usize>,
}

pub struct Searched {
    /// The rows with at least one matching cell.
    pub rows: MatchSet,
    /// Matching cells per column (same order as the columns searched).
    pub per_col: Vec<u64>,
    pub cells: u64,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Byte ranges of the keyword inside `v` (not overlapping).
pub fn cell_spans(v: &str, o: &SearchOpts) -> Vec<(usize, usize)> {
    let needle = o.text.as_str();
    if needle.is_empty() {
        return Vec::new();
    }
    // lowercasing ASCII keeps byte positions; other scripts match exactly as written
    let (hay, pat) = if o.case {
        (v.to_string(), needle.to_string())
    } else {
        (v.to_ascii_lowercase(), needle.to_ascii_lowercase())
    };
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = hay[from..].find(&pat) {
        let (a, b) = (from + i, from + i + pat.len());
        let alone = !o.word
            || (!v[..a].chars().next_back().is_some_and(is_word)
                && !v[b..].chars().next().is_some_and(is_word));
        if alone {
            out.push((a, b));
        }
        from = b.max(a + 1);
        while from < hay.len() && !hay.is_char_boundary(from) {
            from += 1;
        }
        if from >= hay.len() {
            break;
        }
    }
    out
}

/// Search the cells of `cols` on the rows of `candidates` (rows already
/// known to contain the text somewhere, or any wider set).
pub fn search_all(
    st: &Store,
    candidates: &MatchSet,
    cols: &[usize],
    fs: &FormulaSet,
    cache: &ChunkCache,
    o: &SearchOpts,
    cancel: &AtomicBool,
) -> Result<Searched> {
    let mut per_col = vec![0u64; cols.len()];
    let mut cells = 0u64;
    let mut rows = candidates.clone();
    let total = candidates.count();
    let mut start = 0u64;
    while start < total {
        if cancel.load(Ordering::SeqCst) {
            return Err(Error::Cancelled);
        }
        let len = super::exec::rows_per_page(cols.len());
        let p = page_with(st, candidates, start, len, cols, fs, cache)?;
        if p.rows.is_empty() {
            break;
        }
        let mut drop_rows: Vec<u64> = Vec::new();
        for (row, cs) in p.rows.iter().zip(&p.cells) {
            let mut any = false;
            for (k, v) in cs.iter().enumerate() {
                if o.col.is_some_and(|c| c != k) {
                    continue;
                }
                if v.as_deref().is_some_and(|v| !cell_spans(v, o).is_empty()) {
                    per_col[k] += 1;
                    cells += 1;
                    any = true;
                }
            }
            if !any {
                drop_rows.push(*row);
            }
        }
        if !drop_rows.is_empty() {
            rows = rows.without(st, &drop_rows);
        }
        start += p.rows.len() as u64;
    }
    Ok(Searched {
        rows,
        per_col,
        cells,
    })
}
