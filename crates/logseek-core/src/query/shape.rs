//! Shapes: a value read as its runs of digits, letters and spaces with the
//! marks between them kept as they are ("10.2.3.4" is digits, ".", digits,
//! ".", digits, ".", digits). Nothing here knows what any value is; the shape
//! is read from the value itself. Used for "is shaped like" (from one example
//! the user gives) and for "Shapes in this column" (every shape a column
//! holds, with how many rows have it).

use super::exec::{each_in, over_blocks, ChunkCache, FormulaSet, MatchSet};
use crate::error::Result;
use crate::store::Store;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// How closely a value must follow the example's shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Shape {
    /// Digits where it has digits, letters where it has letters, any number of each.
    #[default]
    AnyLength,
    /// The same, and each run exactly as long as in the example.
    SameLength,
    /// Letters and digits count as the same thing (codes like "a1b2").
    Alike,
}

impl Shape {
    pub const ALL: [Shape; 3] = [Shape::AnyLength, Shape::SameLength, Shape::Alike];

    pub fn label(self) -> &'static str {
        match self {
            Shape::AnyLength => "any lengths",
            Shape::SameLength => "the same lengths",
            Shape::Alike => "letters and digits alike",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Shape::AnyLength => "Digits where the example has digits, letters where it has letters, however many of each",
            Shape::SameLength => "As the example: each run of digits or letters exactly as long as it is there",
            Shape::Alike => "A run of letters and digits mixed counts as one piece, however long",
        }
    }
}

/// One piece of an example's shape.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Piece {
    Digits(usize),
    Letters(usize),
    /// Letters and digits together.
    Mixed(usize),
    Spaces(usize),
    /// A mark, kept as it is.
    Mark(char),
}

/// The example cut into pieces. Nothing here knows what the example is.
fn pieces(example: &str, shape: Shape) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    for c in example.chars() {
        let next = if c.is_ascii_digit() {
            Piece::Digits(1)
        } else if c.is_ascii_alphabetic() {
            Piece::Letters(1)
        } else if c == ' ' || c == '\t' {
            Piece::Spaces(1)
        } else {
            Piece::Mark(c)
        };
        let alike = shape == Shape::Alike;
        match (out.last_mut(), &next) {
            (Some(Piece::Digits(n)), Piece::Digits(_)) => *n += 1,
            (Some(Piece::Letters(n)), Piece::Letters(_)) => *n += 1,
            (Some(Piece::Spaces(n)), Piece::Spaces(_)) => *n += 1,
            (Some(Piece::Mixed(n)), Piece::Digits(_) | Piece::Letters(_)) if alike => *n += 1,
            (
                Some(last @ (Piece::Digits(_) | Piece::Letters(_))),
                Piece::Digits(_) | Piece::Letters(_),
            ) if alike => {
                let n = match last {
                    Piece::Digits(n) | Piece::Letters(n) => *n,
                    _ => 0,
                };
                *last = Piece::Mixed(n + 1);
            }
            _ => out.push(match next {
                // alone so far, but in "alike" a single letter or digit is already a mixed run
                Piece::Digits(_) | Piece::Letters(_) if alike => Piece::Mixed(1),
                other => other,
            }),
        }
    }
    out
}

/// The pattern for an example's shape (not anchored); `None` for an empty example.
pub fn shape_pattern(example: &str, shape: Shape) -> Option<String> {
    let ps = pieces(example, shape);
    if ps.is_empty() {
        return None;
    }
    let same = shape == Shape::SameLength;
    let count = |n: usize| {
        if same {
            format!("{{{n}}}")
        } else {
            "+".to_string()
        }
    };
    let mut out = String::new();
    for p in ps {
        match p {
            Piece::Digits(n) => out.push_str(&format!("[0-9]{}", count(n))),
            Piece::Letters(n) => out.push_str(&format!("[A-Za-z]{}", count(n))),
            Piece::Mixed(_) => out.push_str("[A-Za-z0-9]+"),
            Piece::Spaces(n) => out.push_str(&format!("\\s{}", count(n))),
            Piece::Mark(c) => out.push_str(&regex::escape(&c.to_string())),
        }
    }
    Some(out)
}

/// A shape pattern read back in words: `digits, ".", digits`. `None` when
/// the pattern is not one that [`shape_pattern`] writes.
pub fn shape_words(pattern: &str) -> Option<String> {
    let mut rest = pattern;
    let mut parts: Vec<String> = Vec::new();
    let mut marks = String::new();
    let flush = |marks: &mut String, parts: &mut Vec<String>| {
        if !marks.is_empty() {
            parts.push(format!("\"{}\"", std::mem::take(marks)));
        }
    };
    let mut pieces_seen = 0;
    while !rest.is_empty() {
        let classes: [(&str, &str, &str); 4] = [
            ("[A-Za-z0-9]", "letters or digits", "letter or digit"),
            ("[0-9]", "digits", "digit"),
            ("[A-Za-z]", "letters", "letter"),
            ("\\s", "spaces", "space"),
        ];
        if let Some((class, many, one)) = classes.iter().find(|(c, _, _)| rest.starts_with(c)) {
            flush(&mut marks, &mut parts);
            rest = &rest[class.len()..];
            if let Some(r) = rest.strip_prefix('+') {
                rest = r;
                parts.push((*many).to_string());
            } else if let Some(r) = rest.strip_prefix('{') {
                let end = r.find('}')?;
                let n: usize = r[..end].parse().ok()?;
                rest = &r[end + 1..];
                parts.push(format!("{n} {}", if n == 1 { one } else { many }));
            } else {
                return None;
            }
            pieces_seen += 1;
            continue;
        }
        let mut it = rest.chars();
        let c = it.next()?;
        if c == '\\' {
            let n = it.next()?;
            // an escaped mark; a class such as \d is not ours
            if n.is_alphanumeric() {
                return None;
            }
            marks.push(n);
        } else if ".+*?()[]{}|^$".contains(c) {
            return None;
        } else {
            marks.push(c);
        }
        rest = it.as_str();
    }
    flush(&mut marks, &mut parts);
    // plain text with no digits-or-letters piece is not a shape
    (pieces_seen > 0).then(|| parts.join(", "))
}

/// A pattern written between `~/` and `/` in a search: its own `/` escaped.
pub fn between_slashes(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    let mut esc = false;
    for c in p.chars() {
        if c == '/' && !esc {
            out.push('\\');
        }
        esc = c == '\\' && !esc;
        out.push(c);
    }
    out
}

/// The search term for "the whole value of `col` has this shape" (a pattern
/// from [`shape_pattern`]).
pub fn shape_term(col: &str, pattern: &str) -> String {
    format!(
        "{}~/{}/",
        crate::formula::column_ref(col),
        between_slashes(&format!("^{pattern}$"))
    )
}

/// One shape a column holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShapeCount {
    /// The shape as a pattern (see [`shape_pattern`]); empty for empty cells.
    pub pattern: String,
    /// Rows whose value has this shape.
    pub rows: u64,
    /// One value with it, as an example.
    pub example: String,
}

/// Every shape the values of `col` have on the rows of `m`, most rows first
/// (then by shape), with exact counts; empty cells are counted apart. Values
/// whose shape cannot be written (none, in practice) count as their own text.
#[allow(clippy::too_many_arguments)]
pub fn column_shapes(
    st: &Store,
    m: &MatchSet,
    col: usize,
    how: Shape,
    fs: &FormulaSet,
    cache: &ChunkCache,
    cancel: &AtomicBool,
    done: &AtomicU64,
) -> Result<(Vec<ShapeCount>, u64)> {
    let ns = st.schema().len();
    let parts = over_blocks(
        st,
        cache,
        |si| m.segs[si].count() > 0,
        cancel,
        || (HashMap::<String, (u64, String)>::new(), 0u64),
        |(acc, empty), si| {
            let sm = &m.segs[si];
            let mut add = |v: Option<&str>| match v.filter(|v| !v.is_empty()) {
                None => *empty += 1,
                Some(v) => {
                    let p = shape_pattern(v, how).unwrap_or_else(|| v.to_string());
                    acc.entry(p).or_insert_with(|| (0, v.to_string())).0 += 1;
                }
            };
            if col >= ns {
                // a made column: worked out for these rows
                let mut segs = vec![super::exec::SegMatch::None; st.segments().len()];
                segs[si] = sm.clone();
                let one = MatchSet::from_segs(segs);
                let mut start = 0;
                while start < one.count() {
                    let p = super::exec::page_with(st, &one, start, 20_000, &[col], fs, cache)?;
                    if p.rows.is_empty() {
                        break;
                    }
                    start += p.rows.len() as u64;
                    for cells in &p.cells {
                        add(cells.first().and_then(|c| c.as_deref()));
                    }
                }
            } else {
                let blk = cache.block(st, si)?;
                match blk.column(col as u32)? {
                    None => *empty += sm.count(),
                    Some(c) => match c.data() {
                        // a shape per different value, then counted by code
                        crate::segment::ColData::Dict {
                            codes, dict_off, ..
                        } => {
                            let nd = dict_off.len().saturating_sub(1);
                            let mut by_code = vec![0u64; nd];
                            each_in(sm, |r| {
                                if c.is_null(r) {
                                    *empty += 1;
                                } else {
                                    by_code[codes[r] as usize] += 1;
                                }
                            });
                            for (k, &n) in by_code.iter().enumerate() {
                                if n == 0 {
                                    continue;
                                }
                                match c.dict_entry(k).filter(|v| !v.is_empty()) {
                                    None => *empty += n,
                                    Some(v) => {
                                        let p =
                                            shape_pattern(v, how).unwrap_or_else(|| v.to_string());
                                        acc.entry(p).or_insert_with(|| (0, v.to_string())).0 += n;
                                    }
                                }
                            }
                        }
                        _ => each_in(sm, |r| add(c.text(r).as_deref())),
                    },
                }
            }
            done.fetch_add(sm.count(), Ordering::Relaxed);
            Ok(())
        },
    )?;
    let mut all: HashMap<String, (u64, String)> = HashMap::new();
    let mut empty = 0u64;
    for (acc, e) in parts {
        empty += e;
        for (p, (n, ex)) in acc {
            all.entry(p).or_insert((0, ex)).0 += n;
        }
    }
    let mut out: Vec<ShapeCount> = all
        .into_iter()
        .map(|(pattern, (rows, example))| ShapeCount {
            pattern,
            rows,
            example,
        })
        .collect();
    out.sort_by(|a, b| b.rows.cmp(&a.rows).then(a.pattern.cmp(&b.pattern)));
    Ok((out, empty))
}
