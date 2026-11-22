//! Genson-style JSON schema inference: merge observed documents into a tree
//! of nodes that record *sets* of types, key presence counts, array item
//! schemas and length stats, format hints and a few examples.
//!
//! Every field merges associatively and commutatively, so per-chunk schemas
//! built by parallel workers combine exactly (examples use bottom-k hashing).

mod hints;

pub use hints::{detect_hints, Hint};

use crate::ingest::batch::RecordBatch;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use xxhash_rust::xxh3::xxh3_64;

pub const T_NULL: u8 = 1;
pub const T_BOOL: u8 = 2;
pub const T_INT: u8 = 4;
pub const T_FLOAT: u8 = 8;
pub const T_STRING: u8 = 16;
pub const T_OBJECT: u8 = 32;
pub const T_ARRAY: u8 = 64;

/// Above this many distinct keys an object is treated as a map (keys are data).
pub const MAX_KEYS: usize = 4_096;
const MAX_EXAMPLES: usize = 8;
const HINT_RATIO: f64 = 0.8;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeSet(pub u8);

impl TypeSet {
    pub fn has(self, t: u8) -> bool {
        self.0 & t != 0
    }
    pub fn add(&mut self, t: u8) {
        self.0 |= t;
    }
    pub fn labels(self) -> Vec<&'static str> {
        [
            (T_NULL, "null"),
            (T_BOOL, "bool"),
            (T_INT, "int"),
            (T_FLOAT, "float"),
            (T_STRING, "string"),
            (T_OBJECT, "object"),
            (T_ARRAY, "array"),
        ]
        .iter()
        .filter(|(t, _)| self.has(*t))
        .map(|(_, l)| *l)
        .collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SchemaNode {
    pub types: TypeSet,
    /// Times this node was observed (including as null).
    pub count: u64,
    /// Times observed as an object (denominator for child presence).
    pub obj_count: u64,
    pub children: BTreeMap<String, SchemaNode>,
    pub map_like_overflow: u64,
    pub items: Option<Box<SchemaNode>>,
    pub arr_count: u64,
    pub arr_min: u32,
    pub arr_max: u32,
    pub arr_sum: u64,
    /// Up to 8 example values: the ones with the smallest hashes (deterministic).
    pub examples: Vec<String>,
    /// Scalar (string/number) observations, the denominator for hints.
    pub scalar_count: u64,
    pub hint_counts: BTreeMap<Hint, u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PathInfo {
    pub path: String,
    pub types: TypeSet,
    pub presence: f64,
    pub count: u64,
    pub hints: BTreeSet<Hint>,
    pub examples: Vec<String>,
    pub depth: usize,
    pub map_like: bool,
}

impl SchemaNode {
    pub fn child(&self, key: &str) -> Option<&SchemaNode> {
        self.children.get(key)
    }

    pub fn is_map_like(&self) -> bool {
        self.map_like_overflow > 0
    }

    /// Formats seen in at least 80 % of this node's scalar values.
    pub fn hints(&self) -> BTreeSet<Hint> {
        if self.scalar_count == 0 {
            return BTreeSet::new();
        }
        self.hint_counts
            .iter()
            .filter(|(_, &c)| c as f64 / self.scalar_count as f64 >= HINT_RATIO)
            .map(|(h, _)| *h)
            .collect()
    }

    pub fn add(&mut self, v: &Value) {
        self.count += 1;
        match v {
            Value::Null => self.types.add(T_NULL),
            Value::Bool(_) => self.types.add(T_BOOL),
            Value::Number(n) => {
                self.types.add(if n.is_i64() || n.is_u64() {
                    T_INT
                } else {
                    T_FLOAT
                });
                self.scalar(&n.to_string());
            }
            Value::String(s) => {
                self.types.add(T_STRING);
                self.scalar(s);
            }
            Value::Array(a) => {
                self.types.add(T_ARRAY);
                let len = a.len() as u32;
                if self.arr_count == 0 {
                    self.arr_min = len;
                    self.arr_max = len;
                } else {
                    self.arr_min = self.arr_min.min(len);
                    self.arr_max = self.arr_max.max(len);
                }
                self.arr_count += 1;
                self.arr_sum += len as u64;
                if !a.is_empty() {
                    let items = self.items.get_or_insert_with(Default::default);
                    for x in a {
                        items.add(x);
                    }
                }
            }
            Value::Object(m) => {
                self.types.add(T_OBJECT);
                self.obj_count += 1;
                for (k, x) in m {
                    if let Some(c) = self.children.get_mut(k) {
                        c.add(x);
                    } else if self.children.len() < MAX_KEYS {
                        let mut c = SchemaNode::default();
                        c.add(x);
                        self.children.insert(k.clone(), c);
                    } else {
                        self.map_like_overflow += 1;
                    }
                }
            }
        }
    }

    fn scalar(&mut self, s: &str) {
        self.scalar_count += 1;
        for h in detect_hints(s) {
            *self.hint_counts.entry(h).or_insert(0) += 1;
        }
        self.offer_example(s);
    }

    fn offer_example(&mut self, s: &str) {
        if self.examples.iter().any(|e| e == s) {
            return;
        }
        let h = xxh3_64(s.as_bytes());
        if self.examples.len() < MAX_EXAMPLES {
            self.examples.push(s.to_string());
        } else {
            let (worst, wh) = self
                .examples
                .iter()
                .enumerate()
                .map(|(i, e)| (i, xxh3_64(e.as_bytes())))
                .max_by_key(|(_, h)| *h)
                .unwrap();
            if h < wh {
                self.examples[worst] = s.to_string();
            } else {
                return;
            }
        }
        self.examples.sort_by_key(|e| xxh3_64(e.as_bytes()));
    }

    pub fn merge(&mut self, o: &SchemaNode) {
        self.types.add(o.types.0);
        self.count += o.count;
        self.obj_count += o.obj_count;
        self.map_like_overflow += o.map_like_overflow;
        for (k, c) in &o.children {
            if let Some(mine) = self.children.get_mut(k) {
                mine.merge(c);
            } else if self.children.len() < MAX_KEYS {
                self.children.insert(k.clone(), c.clone());
            } else {
                self.map_like_overflow += c.count;
            }
        }
        match (&mut self.items, &o.items) {
            (Some(a), Some(b)) => a.merge(b),
            (None, Some(b)) => self.items = Some(b.clone()),
            _ => {}
        }
        if o.arr_count > 0 {
            if self.arr_count == 0 {
                self.arr_min = o.arr_min;
                self.arr_max = o.arr_max;
            } else {
                self.arr_min = self.arr_min.min(o.arr_min);
                self.arr_max = self.arr_max.max(o.arr_max);
            }
            self.arr_count += o.arr_count;
            self.arr_sum += o.arr_sum;
        }
        self.scalar_count += o.scalar_count;
        for (h, c) in &o.hint_counts {
            *self.hint_counts.entry(*h).or_insert(0) += c;
        }
        for e in &o.examples {
            self.offer_example(e);
        }
    }

    /// Every path in the tree, depth-first, with presence relative to its parent.
    pub fn paths(&self) -> Vec<PathInfo> {
        let mut out = Vec::new();
        self.walk("", 0, &mut out);
        out
    }

    fn walk(&self, prefix: &str, depth: usize, out: &mut Vec<PathInfo>) {
        for (k, c) in &self.children {
            let path = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            out.push(PathInfo {
                path: path.clone(),
                types: c.types,
                presence: if self.obj_count == 0 {
                    0.0
                } else {
                    c.count as f64 / self.obj_count as f64
                },
                count: c.count,
                hints: c.hints(),
                examples: c.examples.clone(),
                depth,
                map_like: c.is_map_like(),
            });
            c.walk(&path, depth + 1, out);
            if let Some(items) = &c.items {
                items.walk(&format!("{path}[*]"), depth + 1, out);
            }
        }
    }
}

/// A field that splits records into types with different fields.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeKeySuggestion {
    pub key: String,
    /// `(value, records, distinct fields)` sorted by record count, descending.
    pub types: Vec<(String, u64, usize)>,
    /// 0..1 — how different the field sets of the types are.
    pub score: f64,
}

/// Find, without knowing the format, a top-level string field that best
/// predicts *which fields a record has* — the "event type" of security logs.
///
/// Score = share of field-presence entropy explained by grouping on the key
/// (an information-gain measure over flattened paths), minus a small penalty
/// per bit of cardinality. Identifier-like keys (average group < 5 records)
/// are skipped because they "explain" everything trivially.
pub fn suggest_type_key(samples: &[Value]) -> Option<TypeKeySuggestion> {
    let objs: Vec<&serde_json::Map<String, Value>> =
        samples.iter().filter_map(|v| v.as_object()).collect();
    let n = objs.len();
    if n < 10 {
        return None;
    }
    // field-presence sets over paths up to depth 3
    let mut field_ix: HashMap<String, usize> = HashMap::new();
    let mut sets: Vec<Vec<usize>> = Vec::with_capacity(n);
    for o in &objs {
        let mut s = Vec::new();
        collect_paths(o, "", 0, &mut |p| {
            let next = field_ix.len();
            s.push(*field_ix.entry(p.to_string()).or_insert(next));
        });
        s.sort_unstable();
        s.dedup();
        sets.push(s);
    }
    let nf = field_ix.len();
    let mut fcount = vec![0u64; nf];
    for s in &sets {
        for &f in s {
            fcount[f] += 1;
        }
    }
    let h = |p: f64| {
        if p <= 0.0 || p >= 1.0 {
            0.0
        } else {
            -(p * p.log2() + (1.0 - p) * (1.0 - p).log2())
        }
    };

    let mut cand: HashMap<&str, usize> = HashMap::new();
    for o in &objs {
        for (k, v) in o.iter() {
            if v.is_string() {
                *cand.entry(k.as_str()).or_insert(0) += 1;
            }
        }
    }
    let mut best: Option<(f64, usize, TypeKeySuggestion)> = None;
    for (key, present) in cand {
        if (present as f64) < n as f64 * 0.9 {
            continue;
        }
        let key_field = field_ix.get(key).copied();
        let mut groups: HashMap<&str, (u64, HashMap<usize, u64>)> = HashMap::new();
        for (o, s) in objs.iter().zip(&sets) {
            let Some(v) = o.get(key).and_then(|v| v.as_str()) else {
                continue;
            };
            let g = groups.entry(v).or_default();
            g.0 += 1;
            for &f in s {
                *g.1.entry(f).or_insert(0) += 1;
            }
        }
        let m = groups.len();
        if m < 2 || m > 1_000 || (present as f64 / m as f64) < 5.0 {
            continue;
        }
        let mut h_tot = 0.0;
        for (f, &c) in fcount.iter().enumerate() {
            if Some(f) != key_field {
                h_tot += h(c as f64 / n as f64);
            }
        }
        if h_tot == 0.0 {
            continue;
        }
        let mut h_cond = 0.0;
        for (gn, fc) in groups.values() {
            let w = *gn as f64 / n as f64;
            for (&f, &c) in fc {
                if Some(f) != key_field {
                    h_cond += w * h(c as f64 / *gn as f64);
                }
            }
        }
        let explained = (h_tot - h_cond) / h_tot;
        let score = explained - 0.04 * (m as f64).log2();
        if explained < 0.4 || score < 0.25 {
            continue;
        }
        let pri = name_priority(key);
        let better = match &best {
            None => true,
            Some((bs, bp, b)) => {
                score > bs + 1e-9
                    || ((score - bs).abs() <= 1e-9
                        && (pri < *bp || (pri == *bp && m < b.types.len())))
            }
        };
        if better {
            let mut types: Vec<(String, u64, usize)> = groups
                .iter()
                .map(|(v, (c, fc))| (v.to_string(), *c, fc.len()))
                .collect();
            types.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            best = Some((
                score,
                pri,
                TypeKeySuggestion {
                    key: key.to_string(),
                    types,
                    score: explained,
                },
            ));
        }
    }
    best.map(|(_, _, s)| s)
}

/// Tie-breaker only: names that usually denote a record type rank first.
fn name_priority(key: &str) -> usize {
    let k = key.to_ascii_lowercase();
    ["event", "type", "kind", "category", "action", "name"]
        .iter()
        .position(|w| k.contains(w))
        .unwrap_or(99)
}

fn collect_paths(
    o: &serde_json::Map<String, Value>,
    prefix: &str,
    depth: usize,
    f: &mut impl FnMut(&str),
) {
    for (k, v) in o {
        let p = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            Value::Null => {}
            Value::Object(c) if !c.is_empty() && depth < 2 => collect_paths(c, &p, depth + 1, f),
            _ => f(&p),
        }
    }
}

/// For a document that is one top-level object, the first key whose value is
/// an array of objects (e.g. CloudTrail `Records`, API `items`). Works on a
/// truncated head of the file.
pub fn suggest_record_path(head: &[u8]) -> Option<String> {
    let start = head.iter().position(|b| !b.is_ascii_whitespace())?;
    if head[start] != b'{' {
        return None;
    }
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    let mut key = Vec::new();
    let mut last_key: Option<Vec<u8>> = None;
    let mut capturing = false;
    let mut i = start;
    while i < head.len() {
        let b = head[i];
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
                if capturing {
                    capturing = false;
                    last_key = Some(std::mem::take(&mut key));
                }
                i += 1;
                continue;
            }
            if capturing {
                key.push(b);
            }
            i += 1;
            continue;
        }
        match b {
            b'"' => {
                in_str = true;
                // a string at depth 1 that is followed by ':' is a key; capture optimistically
                capturing = depth == 1;
                key.clear();
            }
            b':' if depth == 1 => {
                let mut j = i + 1;
                while j < head.len() && head[j].is_ascii_whitespace() {
                    j += 1;
                }
                if head.get(j) == Some(&b'[') {
                    let mut k = j + 1;
                    while k < head.len() && head[k].is_ascii_whitespace() {
                        k += 1;
                    }
                    if head.get(k) == Some(&b'{') {
                        return last_key.and_then(|k| String::from_utf8(k).ok());
                    }
                }
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }
    None
}

/// Columns of a CSV batch that hold JSON objects/arrays in ≥ 90 % of
/// their non-empty sampled cells (at least 5 cells).
pub fn detect_json_columns(batch: &RecordBatch) -> Vec<usize> {
    let rows = batch.rows().min(1_000);
    (0..batch.ncols())
        .filter(|&c| {
            let mut seen = 0usize;
            let mut json = 0usize;
            for r in 0..rows {
                let Some(v) = batch.field(r, c) else { continue };
                let t = v.trim();
                if t.is_empty() {
                    continue;
                }
                seen += 1;
                if (t.starts_with('{') || t.starts_with('['))
                    && serde_json::from_str::<serde::de::IgnoredAny>(t).is_ok()
                {
                    json += 1;
                }
            }
            seen >= 5 && json as f64 >= seen as f64 * 0.9
        })
        .collect()
}
