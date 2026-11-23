//! Text logs with no header, read as columns worked out from the file's own
//! lines, never from a list of known formats. A line is a run of parts split
//! by spaces: words, `"quoted"` groups, `[bracketed]` groups and comma chains
//! (`a, b, c`); a part may sit right against the one before (`"x"y=1`).
//!
//! When most lines have the same number of parts of the same kinds, each part
//! is a column. When lines run on (free text after a few fixed parts), the
//! columns are the parts every line shares in shape, and the rest of the line
//! is one value. Whatever does not fit a line's columns, and a line in another
//! shape, is kept whole in the last column: nothing is ever dropped.

use super::batch::RecordBatch;
use super::encoding::{detect_encoding, to_text};
use super::report::ImportReport;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;

/// The column that keeps what does not fit.
pub const REST: &str = "_rest";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PartKind {
    Word,
    /// Words joined by a comma and a space (`203.0.113.6, 10.0.0.5:3759`).
    Chain,
    Quoted,
    Bracketed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    pub kind: PartKind,
    /// Right against the part before, with no space between.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub glued: bool,
}

/// One part of a line: what it is, its value, and where it starts and ends.
struct Tok<'l> {
    part: Part,
    value: Cow<'l, str>,
    start: usize,
    end: usize,
}

fn unescape(inner: &[u8]) -> Cow<'_, str> {
    if !inner.contains(&b'\\') {
        return to_text(inner).0;
    }
    let mut out = Vec::with_capacity(inner.len());
    let mut j = 0;
    while j < inner.len() {
        if inner[j] == b'\\' && j + 1 < inner.len() && matches!(inner[j + 1], b'"' | b'\\') {
            out.push(inner[j + 1]);
            j += 2;
        } else {
            out.push(inner[j]);
            j += 1;
        }
    }
    Cow::Owned(to_text(&out).0.into_owned())
}

/// The parts of a line, in order.
fn tokens(line: &[u8]) -> Vec<Tok<'_>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < line.len() {
        let spaced = i == 0 || line[i] == b' ';
        while i < line.len() && line[i] == b' ' {
            i += 1;
        }
        if i >= line.len() {
            break;
        }
        let glued = !out.is_empty() && !spaced;
        let start = i;
        let closing = |open: u8, close: u8| -> Option<usize> {
            if line[i] != open {
                return None;
            }
            let mut k = i + 1;
            while k < line.len() {
                if open == b'"' && line[k] == b'\\' {
                    k += 2;
                    continue;
                }
                if line[k] == close {
                    return Some(k);
                }
                k += 1;
            }
            None
        };
        if let Some(e) = closing(b'"', b'"') {
            out.push(Tok {
                part: Part {
                    kind: PartKind::Quoted,
                    glued,
                },
                value: unescape(&line[i + 1..e]),
                start,
                end: e + 1,
            });
            i = e + 1;
            continue;
        }
        if let Some(e) = closing(b'[', b']') {
            out.push(Tok {
                part: Part {
                    kind: PartKind::Bracketed,
                    glued,
                },
                value: to_text(&line[i + 1..e]).0,
                start,
                end: e + 1,
            });
            i = e + 1;
            continue;
        }
        // a word, to the next space; one ending in a comma runs on into the next
        let mut e = i;
        let mut kind = PartKind::Word;
        loop {
            while e < line.len() && line[e] != b' ' {
                e += 1;
            }
            let next = line[e..].iter().position(|&b| b != b' ').map(|k| e + k);
            match next {
                Some(n) if e > i && line[e - 1] == b',' && line[n] != b'"' && line[n] != b'[' => {
                    kind = PartKind::Chain;
                    e = n;
                }
                _ => break,
            }
        }
        out.push(Tok {
            part: Part { kind, glued },
            value: to_text(&line[i..e]).0,
            start,
            end: e,
        });
        i = e;
    }
    out
}

/// A word's shape: digits as 9, letters as a, other characters as written,
/// runs folded (`2026-11-12` is `9-9-9`, `app[42]:` is `a[9]:`).
fn shape(v: &str) -> String {
    let mut s = String::new();
    for c in v.chars() {
        let k = if c.is_ascii_digit() {
            '9'
        } else if c.is_alphabetic() {
            'a'
        } else {
            c
        };
        if s.chars().last() != Some(k) || !(k == '9' || k == 'a') {
            s.push(k);
        }
    }
    s
}

fn entropy<K: std::hash::Hash + Eq>(counts: &HashMap<K, usize>, n: usize) -> f64 {
    counts
        .values()
        .map(|&c| {
            let p = c as f64 / n as f64;
            -p * p.log2()
        })
        .sum()
}

/// The parts every line has, and the names of the columns (one per part, then `_rest`).
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub parts: Vec<Part>,
    pub names: Vec<String>,
}

/// The layout of a text log, from the first lines of the file; `None` when
/// fewer than two parts line up.
pub fn infer(head: &[u8]) -> Option<Layout> {
    let head = &head[detect_encoding(head).bom_len().min(head.len())..];
    let mut raw: Vec<&[u8]> = head.split(|&b| b == b'\n').collect();
    // the last line may be cut by the sample
    if raw.len() > 1 && !head.ends_with(b"\n") {
        raw.pop();
    }
    let mut fields_line: Option<Vec<String>> = None;
    let mut lines: Vec<Vec<Tok>> = Vec::new();
    for l in raw {
        let l = l.strip_suffix(b"\r").unwrap_or(l);
        if l.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        if l.first() == Some(&b'#') {
            // a self-describing log names its columns on a #Fields: line
            if let Some(rest) = l.strip_prefix(b"#Fields:") {
                fields_line = Some(
                    to_text(rest)
                        .0
                        .split_whitespace()
                        .map(str::to_string)
                        .collect(),
                );
            }
            continue;
        }
        lines.push(tokens(l));
        if lines.len() >= 2000 {
            break;
        }
    }
    let n = lines.len();
    if n == 0 {
        return None;
    }
    let most = |share: usize| share * 10 >= n * 9;
    let mut by_len: HashMap<usize, usize> = HashMap::new();
    for l in &lines {
        *by_len.entry(l.len()).or_insert(0) += 1;
    }
    let (&mode, &mode_n) = by_len.iter().max_by_key(|(len, c)| (**c, **len))?;
    let fixed = most(mode_n);
    let len_h = entropy(&by_len, n);
    let mut parts: Vec<Part> = Vec::new();
    for i in 0.. {
        if fixed && i >= mode {
            break;
        }
        let at: Vec<&Tok> = lines.iter().filter_map(|l| l.get(i)).collect();
        if !most(at.len()) {
            break;
        }
        // the kind (and, where lines run on, the shape) most lines share here
        let key = |t: &Tok| -> (PartKind, bool, String) {
            let s = if fixed || t.part.kind != PartKind::Word {
                String::new()
            } else {
                shape(&t.value)
            };
            (t.part.kind, t.part.glued, s)
        };
        let mut kinds: HashMap<(PartKind, bool, String), usize> = HashMap::new();
        for t in &at {
            *kinds.entry(key(t)).or_insert(0) += 1;
        }
        let (best, &best_n) = kinds.iter().max_by_key(|(_, c)| **c)?;
        if !most(best_n) {
            break;
        }
        if !fixed && best.0 == PartKind::Word {
            // where lines run on, free text starts at a word that either
            // varies like prose, or tells how long the line is (the first
            // word of a message does; a level or a host does not)
            let mut vals: HashMap<&str, usize> = HashMap::new();
            for t in &at {
                *vals.entry(t.value.as_ref()).or_insert(0) += 1;
            }
            let letters = best.2.chars().all(|c| c == 'a');
            if vals.len() * 10 <= n {
                let mut joint: HashMap<(&str, usize), usize> = HashMap::new();
                for l in &lines {
                    if let Some(t) = l.get(i) {
                        *joint.entry((t.value.as_ref(), l.len())).or_insert(0) += 1;
                    }
                }
                let given = entropy(&joint, at.len()) - entropy(&vals, at.len());
                if len_h > 0.0 && given < len_h * 0.5 {
                    break;
                }
            } else if letters {
                break;
            }
        }
        parts.push(Part {
            kind: best.0,
            glued: best.1,
        });
    }
    if parts.len() < 2 {
        return None;
    }
    let mut names: Vec<String> = match fields_line {
        Some(f) if f.len() == parts.len() => f,
        _ => (1..=parts.len()).map(|i| format!("column_{i}")).collect(),
    };
    names.push(REST.to_string());
    Some(Layout { parts, names })
}

/// Lines of a chunk as rows: each part of the layout a column, the rest of
/// the line (or the whole line, in another shape) in the last one. Comment
/// lines (`#…`) and empty lines are not rows. `chunk` starts and ends on line
/// boundaries; `ncols` is the layout's parts plus one.
pub fn parse_chunk(
    chunk: &[u8],
    base: u64,
    parts: &[Part],
    ncols: usize,
    rep: &mut ImportReport,
) -> Vec<RecordBatch> {
    let start = if base == 0 {
        detect_encoding(chunk).bom_len().min(chunk.len())
    } else {
        0
    };
    let mut out = Vec::new();
    let mut batch = RecordBatch::new(ncols);
    let mut pos = start;
    while pos < chunk.len() {
        let end = chunk[pos..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(chunk.len(), |k| pos + k);
        let line = &chunk[pos..end];
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let at = base + pos as u64;
        pos = end + 1;
        if line.iter().all(|b| b.is_ascii_whitespace()) || line.first() == Some(&b'#') {
            continue;
        }
        if batch.is_full() {
            let mut full = std::mem::replace(&mut batch, RecordBatch::new(ncols));
            full.set_end_offset(at);
            out.push(full);
        }
        batch.begin_row(at);
        rep.rows += 1;
        let toks = tokens(line);
        // the parts that match the layout, in order, until one does not
        let mut fit = 0;
        while fit < parts.len()
            && toks
                .get(fit)
                .is_some_and(|t| t.part.kind == parts[fit].kind)
        {
            fit += 1;
        }
        if fit < parts.len() {
            rep.ragged_rows += 1;
            // a line in another shape altogether is kept whole, not spread
            // over columns it only seems to share
            if fit * 2 < parts.len() {
                fit = 0;
            }
        }
        for (c, t) in toks.iter().take(fit).enumerate() {
            batch.set(c, &t.value);
        }
        let rest_from = if fit == 0 {
            0
        } else if fit < parts.len() {
            toks.get(fit).map_or(line.len(), |t| t.start)
        } else {
            toks[fit - 1].end
        };
        let rest = &line[rest_from.min(line.len())..];
        let rest = rest.strip_prefix(b" ").unwrap_or(rest);
        if !rest.iter().all(|b| b.is_ascii_whitespace()) {
            let (t, fell_back) = to_text(rest);
            if fell_back {
                rep.decode_errors += 1;
            }
            batch.set(ncols - 1, &t);
        }
    }
    if batch.rows() > 0 {
        batch.set_end_offset(base + chunk.len() as u64);
        out.push(batch);
    }
    out
}
