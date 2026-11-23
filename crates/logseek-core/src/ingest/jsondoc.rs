//! A JSON file that is one object holding several lists, such as
//! `{"basics": {..}, "work": [..], "skills": [..]}`. Read as rows, such a
//! file is a single row, or only one of its lists. Here it is treated as a
//! book: each list (and each object) at the top is a part that can be opened
//! as rows of its own. Nothing is held in memory but the item being read:
//! the file is walked once to list the parts, and once to write a part out
//! as one JSON record per line.

use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use std::fmt;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The name of the part made of the plain values at the top of the file.
pub const TOP: &str = "values at the top";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A list: each item is a row.
    List,
    /// One object: a single row.
    Record,
    /// The plain values at the top of the file, together: a single row.
    Values,
}

/// One part of the file that can be opened as rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub name: String,
    pub kind: Kind,
    /// How many rows it gives.
    pub rows: u64,
}

enum Shape {
    List(u64),
    Record,
    Plain(Value),
}

/// Reads one value only far enough to say what it is.
struct ShapeSeed;

impl<'de> DeserializeSeed<'de> for ShapeSeed {
    type Value = Shape;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Shape, D::Error> {
        d.deserialize_any(ShapeVisitor)
    }
}

struct ShapeVisitor;

impl<'de> Visitor<'de> for ShapeVisitor {
    type Value = Shape;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON value")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Shape, A::Error> {
        let mut n = 0u64;
        while seq.next_element::<IgnoredAny>()?.is_some() {
            n += 1;
        }
        Ok(Shape::List(n))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Shape, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Shape::Record)
    }
    fn visit_bool<E>(self, v: bool) -> Result<Shape, E> {
        Ok(Shape::Plain(Value::Bool(v)))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Shape, E> {
        Ok(Shape::Plain(Value::from(v)))
    }
    fn visit_u64<E>(self, v: u64) -> Result<Shape, E> {
        Ok(Shape::Plain(Value::from(v)))
    }
    fn visit_f64<E>(self, v: f64) -> Result<Shape, E> {
        Ok(Shape::Plain(Value::from(v)))
    }
    fn visit_str<E>(self, v: &str) -> Result<Shape, E> {
        Ok(Shape::Plain(Value::String(v.to_string())))
    }
    fn visit_string<E>(self, v: String) -> Result<Shape, E> {
        Ok(Shape::Plain(Value::String(v)))
    }
    fn visit_unit<E>(self) -> Result<Shape, E> {
        Ok(Shape::Plain(Value::Null))
    }
}

/// Walks the object at the top, giving each key and what it holds.
struct TopVisitor<F: FnMut(String, Shape)>(F);

impl<'de, F: FnMut(String, Shape)> Visitor<'de> for TopVisitor<F> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON object")
    }
    fn visit_map<A: MapAccess<'de>>(mut self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            let shape = map.next_value_seed(ShapeSeed)?;
            (self.0)(key, shape);
        }
        Ok(())
    }
}

fn open(path: &Path) -> Result<BufReader<std::fs::File>, String> {
    std::fs::File::open(path)
        .map(|f| BufReader::with_capacity(1 << 20, f))
        .map_err(|e| e.to_string())
}

/// The parts of the file, in its own order; the plain values at the top,
/// when there are any, come first as one part.
pub fn parts(path: &Path) -> Result<Vec<Part>, String> {
    let mut out: Vec<Part> = Vec::new();
    let mut plain = 0u64;
    let mut de = serde_json::Deserializer::from_reader(open(path)?);
    de.deserialize_map(TopVisitor(|name, shape| match shape {
        Shape::List(n) => out.push(Part {
            name,
            kind: Kind::List,
            rows: n,
        }),
        Shape::Record => out.push(Part {
            name,
            kind: Kind::Record,
            rows: 1,
        }),
        Shape::Plain(_) => plain += 1,
    }))
    .map_err(|e| e.to_string())?;
    // one object and nothing after it: objects back to back are rows, not parts
    de.end().map_err(|e| e.to_string())?;
    if plain > 0 {
        out.insert(
            0,
            Part {
                name: TOP.to_string(),
                kind: Kind::Values,
                rows: 1,
            },
        );
    }
    Ok(out)
}

/// True when the start of a `.json` file says it may be a document of parts:
/// it opens with an object that is not one whole record on the first line.
/// Only reading it through (`parts`) says whether it is one; this is the
/// quick look that needs no more than the file's first bytes.
pub fn may_be_doc(path: &Path) -> bool {
    let json = path
        .extension()
        .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case("json"));
    if !json {
        return false;
    }
    let mut head = vec![0u8; 64 << 10];
    let n = match std::fs::File::open(path).and_then(|mut f| f.read(&mut head)) {
        Ok(n) => n,
        Err(_) => return false,
    };
    let head = &head[..n];
    let first = head
        .iter()
        .copied()
        .find(|b| !b.is_ascii_whitespace() && !matches!(*b, 0xEF | 0xBB | 0xBF));
    if first != Some(b'{') {
        return false;
    }
    let line_end = head.iter().position(|&b| b == b'\n').unwrap_or(head.len());
    let one_per_line = line_end < head.len()
        && serde_json::from_slice::<IgnoredAny>(head[..line_end].trim_ascii()).is_ok()
        && head[line_end..].iter().any(|b| !b.is_ascii_whitespace());
    !one_per_line
}

/// The parts of a file say it is a document to open part by part: a list in
/// it and at least one more part.
pub fn parts_make_a_doc(parts: &[Part]) -> bool {
    parts.len() >= 2 && parts.iter().any(|x| x.kind == Kind::List)
}

/// True for a `.json` file that is one object with a list in it and at least
/// one more part: a file to open part by part. A file with one record per
/// line, a list at the top, or a single wrapped list is not one.
pub fn is_doc(path: &Path) -> bool {
    may_be_doc(path) && parts(path).is_ok_and(|p| parts_make_a_doc(&p))
}

/// Writes the items of one value as one record per line.
struct WriteSeed<'a, W: Write> {
    out: &'a mut W,
    cancel: &'a AtomicBool,
    done: &'a AtomicU64,
    rows: &'a mut u64,
}

fn line<W: Write, E: serde::de::Error>(out: &mut W, v: &Value) -> Result<(), E> {
    // an item that is not an object is a row with one column
    let res = match v {
        Value::Object(_) => serde_json::to_writer(&mut *out, v),
        other => serde_json::to_writer(&mut *out, &serde_json::json!({ "value": other })),
    };
    res.map_err(E::custom)?;
    out.write_all(b"\n").map_err(E::custom)
}

impl<'de, W: Write> DeserializeSeed<'de> for WriteSeed<'_, W> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de, W: Write> Visitor<'de> for WriteSeed<'_, W> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a list or an object")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while let Some(v) = seq.next_element::<Value>()? {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(serde::de::Error::custom("stopped"));
            }
            line(self.out, &v)?;
            *self.rows += 1;
            self.done.store(*self.rows, Ordering::Relaxed);
        }
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<(), A::Error> {
        let v = Value::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
        line(self.out, &v)?;
        *self.rows += 1;
        self.done.store(*self.rows, Ordering::Relaxed);
        Ok(())
    }
}

use serde::Deserialize;

struct PartVisitor<'a, W: Write> {
    name: &'a str,
    out: &'a mut W,
    cancel: &'a AtomicBool,
    done: &'a AtomicU64,
    rows: &'a mut u64,
    found: &'a mut bool,
}

impl<'de, W: Write> Visitor<'de> for PartVisitor<'_, W> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut top = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if self.name == TOP {
                if let Shape::Plain(v) = map.next_value_seed(ShapeSeed)? {
                    top.insert(key, v);
                }
            } else if key == self.name && !*self.found {
                *self.found = true;
                map.next_value_seed(WriteSeed {
                    out: &mut *self.out,
                    cancel: self.cancel,
                    done: self.done,
                    rows: &mut *self.rows,
                })?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        if self.name == TOP && !top.is_empty() {
            *self.found = true;
            line(self.out, &Value::Object(top))?;
            *self.rows += 1;
        }
        Ok(())
    }
}

/// Write the part named `name` to `out`, one JSON record per line. Returns
/// how many rows were written.
pub fn write_part(
    path: &Path,
    name: &str,
    out: &Path,
    cancel: &AtomicBool,
    done: &AtomicU64,
) -> Result<u64, String> {
    let file = std::fs::File::create(out).map_err(|e| e.to_string())?;
    let mut w = BufWriter::with_capacity(1 << 20, file);
    let (mut rows, mut found) = (0u64, false);
    let mut de = serde_json::Deserializer::from_reader(open(path)?);
    let res = de.deserialize_map(PartVisitor {
        name,
        out: &mut w,
        cancel,
        done,
        rows: &mut rows,
        found: &mut found,
    });
    let res = res
        .map_err(|e| e.to_string())
        .and_then(|_| w.flush().map_err(|e| e.to_string()));
    drop(w);
    match res {
        Ok(()) if found => Ok(rows),
        Ok(()) => {
            let _ = std::fs::remove_file(out);
            Err(format!("the file has no part named {name}"))
        }
        Err(e) => {
            // never leave half a part behind to be taken for a whole one
            let _ = std::fs::remove_file(out);
            Err(e)
        }
    }
}
