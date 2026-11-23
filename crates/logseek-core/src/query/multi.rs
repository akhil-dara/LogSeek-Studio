//! Many keywords at once. A list of any length (thousands of indicators) is
//! looked for in every cell in ONE pass over the rows, and the answer says,
//! for each keyword: how many rows have it, how many cells, and in which
//! columns. So "which of my 2,000 values are in these million events, and
//! where" is one read of the data, not 2,000.

use super::exec::{page_with, rows_per_page, ChunkCache, FormulaSet, MatchSet, SegMatch};
use crate::error::{Error, Result};
use crate::store::Store;
use aho_corasick::{AhoCorasick, MatchKind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListOpts {
    /// Capitals must match.
    pub case: bool,
    /// The keyword must stand alone (not inside a longer word).
    pub word: bool,
}

/// What one keyword found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TermHits {
    /// Rows with the keyword in at least one cell.
    pub rows: u64,
    /// Cells that hold it.
    pub cells: u64,
    /// (column, cells that hold it), most cells first; the column is an
    /// index into the columns searched.
    pub cols: Vec<(usize, u64)>,
}

pub struct ListHits {
    /// One entry per keyword, in the order given.
    pub terms: Vec<TermHits>,
    /// The rows with at least one keyword.
    pub rows: MatchSet,
    /// Rows accounted for: read, or in a block known to hold no keyword.
    pub scanned: u64,
    /// Blocks of rows looked at, and how many of them were not read at all
    /// because the index says none of the keywords can be in them.
    pub blocks: u64,
    pub blocks_skipped: u64,
    /// Columns read, and columns not read for the same reason, over all blocks.
    pub columns_read: u64,
    pub columns_skipped: u64,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Look for every one of `terms` in the cells of `cols` on the rows of
/// `candidates`. Empty keywords find nothing. `done` counts rows read.
#[allow(clippy::too_many_arguments)]
pub fn search_list(
    st: &Store,
    candidates: &MatchSet,
    cols: &[usize],
    fs: &FormulaSet,
    cache: &ChunkCache,
    terms: &[String],
    o: &ListOpts,
    cancel: &AtomicBool,
    done: &AtomicU64,
) -> Result<ListHits> {
    let n = terms.len();
    // empty keywords would match everywhere: they are given a text no cell holds
    let pats: Vec<&str> = terms
        .iter()
        .map(|t| {
            if t.is_empty() {
                "\u{0}\u{1}\u{0}"
            } else {
                t.as_str()
            }
        })
        .collect();
    let ac = AhoCorasick::builder()
        .ascii_case_insensitive(!o.case)
        .match_kind(MatchKind::Standard)
        .build(&pats)
        .map_err(|e| Error::Invalid(format!("the list could not be searched: {e}")))?;
    let mut rows_hit = vec![0u64; n];
    let mut cells_hit = vec![0u64; n];
    let mut by_col: Vec<std::collections::HashMap<usize, u64>> = vec![Default::default(); n];
    // "seen in this cell / this row" marks, by a counter instead of clearing
    let mut cell_mark = vec![0u64; n];
    let mut row_mark = vec![0u64; n];
    let (mut cell_no, mut row_no) = (0u64, 0u64);
    // lower-case keywords, for asking the index "can this be here?"
    let lows: Vec<Vec<u8>> = terms
        .iter()
        .map(|t| t.to_ascii_lowercase().into_bytes())
        .collect();
    let schema_len = st.schema().len();
    let nsegs = st.segments().len();
    let mut out_segs: Vec<SegMatch> = candidates.segs.clone();
    let (mut blocks, mut blocks_skipped) = (0u64, 0u64);
    let (mut columns_read, mut columns_skipped) = (0u64, 0u64);
    let mut scanned = 0u64;
    for si in 0..nsegs {
        if cancel.load(Ordering::SeqCst) {
            return Err(Error::Cancelled);
        }
        let seg_count = candidates.segs[si].count();
        if seg_count == 0 {
            continue;
        }
        blocks += 1;
        let seg = &st.segments()[si];
        let meta = seg.meta();
        // the columns of this block that can hold at least one keyword: the
        // index keeps, per column of each block, what text cannot be there.
        // It only ever says "cannot" or "may", so nothing is missed.
        let need: Vec<(usize, usize)> = cols
            .iter()
            .enumerate()
            .filter(|&(_, &c)| {
                if c >= schema_len {
                    return true; // a made column: worked out, not indexed
                }
                let Some(cm) = meta.col(c as u32) else {
                    return false; // the block has no value in this column
                };
                // the column's filter, read once for all the keywords
                let bloom = seg.bloom_bytes(c as u32);
                lows.iter()
                    .any(|l| !l.is_empty() && super::exec::eligible_with(bloom.as_deref(), cm, l))
            })
            .map(|(k, &c)| (k, c))
            .collect();
        columns_read += need.len() as u64;
        columns_skipped += (cols.len() - need.len()) as u64;
        if need.is_empty() {
            blocks_skipped += 1;
            out_segs[si] = SegMatch::None;
            scanned += seg_count;
            done.store(scanned, Ordering::Relaxed);
            continue;
        }
        let need_cols: Vec<usize> = need.iter().map(|&(_, c)| c).collect();
        // this block alone
        let mut one: Vec<SegMatch> = vec![SegMatch::None; nsegs];
        one[si] = candidates.segs[si].clone();
        let one = MatchSet::from_segs(one);
        let page = rows_per_page(need_cols.len());
        let mut hit_rows: Vec<u32> = Vec::new();
        let mut start = 0u64;
        while start < seg_count {
            if cancel.load(Ordering::SeqCst) {
                return Err(Error::Cancelled);
            }
            let p = page_with(st, &one, start, page, &need_cols, fs, cache)?;
            if p.rows.is_empty() {
                break;
            }
            for (row, cs) in p.rows.iter().zip(&p.cells) {
                row_no += 1;
                let mut any = false;
                for (j, v) in cs.iter().enumerate() {
                    let Some(v) = v.as_deref() else { continue };
                    if v.is_empty() {
                        continue;
                    }
                    // the column's place in the list the caller gave
                    let k = need[j].0;
                    cell_no += 1;
                    for m in ac.find_overlapping_iter(v) {
                        let t = m.pattern().as_usize();
                        if cell_mark[t] == cell_no {
                            continue;
                        }
                        if o.word
                            && (v[..m.start()].chars().next_back().is_some_and(is_word)
                                || v[m.end()..].chars().next().is_some_and(is_word))
                        {
                            continue;
                        }
                        cell_mark[t] = cell_no;
                        cells_hit[t] += 1;
                        *by_col[t].entry(k).or_default() += 1;
                        if row_mark[t] != row_no {
                            row_mark[t] = row_no;
                            rows_hit[t] += 1;
                        }
                        any = true;
                    }
                }
                if any {
                    hit_rows.push((*row - meta.row_start) as u32);
                }
            }
            start += p.rows.len() as u64;
            done.store(scanned + start, Ordering::Relaxed);
        }
        scanned += seg_count;
        out_segs[si] = SegMatch::of_rows(&hit_rows, meta.rows);
    }
    let rows = MatchSet::from_segs(out_segs);
    let terms = (0..n)
        .map(|t| {
            let mut cols: Vec<(usize, u64)> = by_col[t].iter().map(|(c, k)| (*c, *k)).collect();
            cols.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            TermHits {
                rows: rows_hit[t],
                cells: cells_hit[t],
                cols,
            }
        })
        .collect();
    Ok(ListHits {
        terms,
        rows,
        scanned,
        blocks,
        blocks_skipped,
        columns_read,
        columns_skipped,
    })
}
