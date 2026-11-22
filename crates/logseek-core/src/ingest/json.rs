//! JSON records: NDJSON, arrays (incl. one-object-per-line), wrapper documents
//! like `{"Records":[...]}`, nested objects flattened to dotted columns,
//! schema drift, and invalid records kept raw.
//!
//! Parsing is a byte-level flattener (no tree per record): numbers and arrays
//! are copied byte-exact, strings are unescaped, structure is validated.

use super::batch::RecordBatch;
use super::encoding::to_text;
use super::report::ImportReport;
use std::collections::HashMap;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JsonOptions {
    /// Top-level key whose array holds the records (e.g. CloudTrail `Records`).
    pub record_path: Option<String>,
    /// Elasticsearch bulk: fold each instruction line into the next document.
    pub bulk: bool,
    /// The source is read one record per line (NDJSON, bulk): a line that
    /// happens to start with `[` is a record, not the start of an array. Set
    /// from the `SourceSpec`; when false the first byte of the buffer decides.
    pub line_oriented: bool,
}

/// An Elasticsearch bulk instruction line (`{"index":{"_index":..,"_id":..}}`,
/// also create / update / delete): its `_index` and `_id`, when it is one.
pub fn bulk_action(line: &[u8]) -> Option<Vec<(String, String)>> {
    let t = line.trim_ascii();
    let body = t.strip_prefix(b"{")?.trim_ascii_start();
    let starts = [
        &b"\"index\""[..],
        b"\"create\"",
        b"\"update\"",
        b"\"delete\"",
    ];
    if !starts.iter().any(|s| body.starts_with(s)) {
        return None;
    }
    let v: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(t).ok()?;
    if v.len() != 1 {
        return None;
    }
    let inner = v.values().next()?.as_object()?;
    let mut meta = Vec::new();
    for k in ["_index", "_id"] {
        if let Some(x) = inner.get(k) {
            let s = match x {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            meta.push((k.to_string(), s));
        }
    }
    Some(meta)
}

/// The document with the instruction's fields put first.
fn fold_bulk(doc: &[u8], meta: &[(String, String)]) -> Vec<u8> {
    let t = doc.trim_ascii();
    let Some(rest) = t.strip_prefix(b"{") else {
        return t.to_vec();
    };
    let mut out = Vec::with_capacity(t.len() + 64);
    out.push(b'{');
    for (i, (k, v)) in meta.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        out.extend_from_slice(serde_json::to_string(k).unwrap_or_default().as_bytes());
        out.push(b':');
        out.extend_from_slice(serde_json::to_string(v).unwrap_or_default().as_bytes());
    }
    if !meta.is_empty() && !rest.trim_ascii_start().starts_with(b"}") {
        out.push(b',');
    }
    out.extend_from_slice(rest);
    out
}

fn stored_name(name: &str) -> std::borrow::Cow<'_, str> {
    if super::pipeline::RESERVED_COLUMNS.contains(&name) {
        std::borrow::Cow::Owned(format!("{name}_2"))
    } else {
        std::borrow::Cow::Borrowed(name)
    }
}

/// Column registry that grows as new keys appear.
#[derive(Debug, Clone, Default)]
pub struct SchemaBuilder {
    names: Vec<String>,
    index: HashMap<String, usize>,
}

impl SchemaBuilder {
    /// The column for a field name. A field named like one of the app's own
    /// columns (`_file`, `_overflow`) is stored under `<name>_2`, so its values
    /// never share a column id with the app's column; `index_of` follows it.
    pub fn column_index(&mut self, name: &str) -> usize {
        let name = stored_name(name);
        if let Some(&i) = self.index.get(&*name) {
            return i;
        }
        let i = self.names.len();
        self.names.push(name.to_string());
        self.index.insert(name.into_owned(), i);
        i
    }
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.index.get(&*stored_name(name)).copied()
    }
    pub fn names(&self) -> &[String] {
        &self.names
    }
    pub fn len(&self) -> usize {
        self.names.len()
    }
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

/// Objects (and arrays inside a value) nested deeper than this are not
/// flattened: the record is kept raw and counted in `invalid_json`. The
/// flattener recurses once per level, so this also bounds the stack it uses.
pub const MAX_NESTING: usize = 512;

pub const RAW_COLUMN: &str = "_raw";
pub const VALUE_COLUMN: &str = "_value";

/// Parse records already cut out by an `ArrayElementScanner`:
/// `elems[i] = (source offset, start, end)` into `buf`; `end_offset` = source
/// offset just past the last element's separator.
pub fn parse_json_elements(
    buf: &[u8],
    elems: &[(u64, usize, usize)],
    end_offset: u64,
    schema: &mut SchemaBuilder,
    rep: &mut ImportReport,
) -> Vec<RecordBatch> {
    let mut out = Vec::new();
    let mut batch = RecordBatch::new_sparse(schema.len());
    for &(off, a, b) in elems {
        push_record(&buf[a..b], off, schema, &mut batch, &mut out, rep);
    }
    if batch.rows() > 0 {
        batch.set_end_offset(end_offset);
        out.push(batch);
    }
    out
}

/// Parse a buffer of JSON records starting at source offset `base`.
pub fn parse_json_records(
    data: &[u8],
    base: u64,
    schema: &mut SchemaBuilder,
    opts: &JsonOptions,
    rep: &mut ImportReport,
) -> Vec<RecordBatch> {
    let mut out = Vec::new();
    let mut batch = RecordBatch::new_sparse(schema.len());
    let first = data.iter().position(|b| !b.is_ascii_whitespace());
    // a line-oriented source never reads a chunk as an array: a line that
    // starts with `[` is one record like the others
    let starts_array = !opts.line_oriented && first.is_some_and(|i| data[i] == b'[');
    if opts.record_path.is_some() || starts_array {
        let mut sc = ArrayElementScanner::new(opts.record_path.clone());
        sc.set_base(base);
        let mut buf = Vec::new();
        sc.feed_with_offsets(data, &mut buf, |off, el| {
            push_record(el, off, schema, &mut batch, &mut out, rep);
        });
    } else {
        let mut pos = 0usize;
        // bulk: the instruction waiting for its document (row starts there)
        let mut pending: Option<(u64, Vec<(String, String)>)> = None;
        for line in data.split(|&b| b == b'\n') {
            let off = base + pos as u64;
            pos += line.len() + 1;
            let t = line.trim_ascii();
            if t.is_empty() {
                continue;
            }
            let lead = line.len() - line.trim_ascii_start().len();
            let at = off + lead as u64;
            if opts.bulk {
                if let Some(meta) = bulk_action(t) {
                    // a delete has no document: a new instruction replaces it
                    pending = Some((at, meta));
                    continue;
                }
                if let Some((start, meta)) = pending.take() {
                    let folded = fold_bulk(t, &meta);
                    push_record(&folded, start, schema, &mut batch, &mut out, rep);
                    continue;
                }
            }
            push_record(t, at, schema, &mut batch, &mut out, rep);
        }
    }
    if batch.rows() > 0 {
        batch.set_end_offset(base + data.len() as u64);
        out.push(batch);
    }
    out
}

fn push_record(
    raw: &[u8],
    off: u64,
    schema: &mut SchemaBuilder,
    batch: &mut RecordBatch,
    out: &mut Vec<RecordBatch>,
    rep: &mut ImportReport,
) {
    if batch.is_full() {
        let mut full = std::mem::replace(batch, RecordBatch::new_sparse(schema.len()));
        full.set_end_offset(off);
        out.push(full);
    }
    batch.begin_row(off);
    rep.rows += 1;
    let mark = batch.mark();
    let mut p = Flat {
        b: raw,
        i: 0,
        decode_errors: 0,
    };
    if p.record(schema, batch).is_ok() {
        rep.decode_errors += p.decode_errors;
    } else {
        batch.rollback_row_cells(mark);
        rep.invalid_json += 1;
        let c = schema.column_index(RAW_COLUMN);
        let (t, _) = to_text(raw);
        batch.set(c, &t);
    }
}

type R<T> = Result<T, ()>;

struct Flat<'a> {
    b: &'a [u8],
    i: usize,
    decode_errors: u64,
}

impl<'a> Flat<'a> {
    #[inline]
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    #[inline]
    fn peek(&self) -> R<u8> {
        self.b.get(self.i).copied().ok_or(())
    }
    #[inline]
    fn eat(&mut self, c: u8) -> R<()> {
        if self.peek()? == c {
            self.i += 1;
            Ok(())
        } else {
            Err(())
        }
    }

    fn text(&mut self, bytes: &[u8]) -> String {
        let (t, bad) = to_text(bytes);
        if bad {
            self.decode_errors += 1;
        }
        t.into_owned()
    }

    fn record(&mut self, schema: &mut SchemaBuilder, batch: &mut RecordBatch) -> R<()> {
        self.ws();
        let mut prefix = String::new();
        if self.peek()? == b'{' {
            self.object(&mut prefix, schema, batch, true, 0)?;
        } else {
            let c = schema.column_index(VALUE_COLUMN);
            self.scalar(Some(c), &prefix, schema, batch)?;
        }
        self.ws();
        if self.i == self.b.len() {
            Ok(())
        } else {
            Err(())
        }
    }

    fn object(
        &mut self,
        prefix: &mut String,
        schema: &mut SchemaBuilder,
        batch: &mut RecordBatch,
        top: bool,
        depth: usize,
    ) -> R<()> {
        if depth > MAX_NESTING {
            return Err(());
        }
        self.eat(b'{')?;
        self.ws();
        if self.peek()? == b'}' {
            self.i += 1;
            if !top {
                let c = schema.column_index(prefix);
                batch.set(c, "{}");
            }
            return Ok(());
        }
        loop {
            self.ws();
            let key = self.string()?;
            self.ws();
            self.eat(b':')?;
            self.ws();
            let saved = prefix.len();
            if !prefix.is_empty() {
                prefix.push('.');
            }
            prefix.push_str(&key);
            if self.peek()? == b'{' {
                self.object(prefix, schema, batch, false, depth + 1)?;
            } else {
                self.scalar(None, prefix, schema, batch)?;
            }
            prefix.truncate(saved);
            self.ws();
            match self.peek()? {
                b',' => self.i += 1,
                b'}' => {
                    self.i += 1;
                    return Ok(());
                }
                _ => return Err(()),
            }
        }
    }

    /// A non-object value (or any value when `fixed` is set), stored as text.
    fn scalar(
        &mut self,
        fixed: Option<usize>,
        prefix: &str,
        schema: &mut SchemaBuilder,
        batch: &mut RecordBatch,
    ) -> R<()> {
        let value: Option<String> = match self.peek()? {
            b'{' | b'[' => {
                let s = self.i;
                self.skip_container()?;
                let raw = &self.b[s..self.i];
                Some(self.text(raw))
            }
            b'"' => Some(self.string()?),
            b't' => {
                self.lit(b"true")?;
                Some("true".into())
            }
            b'f' => {
                self.lit(b"false")?;
                Some("false".into())
            }
            b'n' => {
                self.lit(b"null")?;
                None
            }
            b'-' | b'0'..=b'9' => {
                let s = self.i;
                while self.i < self.b.len()
                    && matches!(
                        self.b[self.i],
                        b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
                    )
                {
                    self.i += 1;
                }
                Some(String::from_utf8_lossy(&self.b[s..self.i]).into_owned())
            }
            _ => return Err(()),
        };
        if let Some(v) = value {
            let c = match fixed {
                Some(c) => c,
                None => schema.column_index(prefix),
            };
            batch.set(c, &v);
        }
        Ok(())
    }

    fn lit(&mut self, w: &[u8]) -> R<()> {
        if self.b[self.i..].starts_with(w) {
            self.i += w.len();
            Ok(())
        } else {
            Err(())
        }
    }

    fn skip_container(&mut self) -> R<()> {
        let mut depth = 0usize;
        while self.i < self.b.len() {
            match self.b[self.i] {
                b'"' => {
                    self.skip_string()?;
                    continue;
                }
                b'{' | b'[' => {
                    depth += 1;
                    if depth > MAX_NESTING {
                        return Err(());
                    }
                }
                b'}' | b']' => {
                    depth = depth.checked_sub(1).ok_or(())?;
                    if depth == 0 {
                        self.i += 1;
                        return Ok(());
                    }
                }
                _ => {}
            }
            self.i += 1;
        }
        Err(())
    }

    fn skip_string(&mut self) -> R<()> {
        self.eat(b'"')?;
        while self.i < self.b.len() {
            match self.b[self.i] {
                b'\\' => self.i += 2,
                b'"' => {
                    self.i += 1;
                    return Ok(());
                }
                _ => self.i += 1,
            }
        }
        Err(())
    }

    fn string(&mut self) -> R<String> {
        self.eat(b'"')?;
        let s = self.i;
        while self.i < self.b.len() {
            match self.b[self.i] {
                b'"' => {
                    let raw = &self.b[s..self.i];
                    self.i += 1;
                    return Ok(self.text(raw));
                }
                b'\\' => break,
                _ => self.i += 1,
            }
        }
        let mut out: Vec<u8> = self.b[s..self.i].to_vec();
        while self.i < self.b.len() {
            let c = self.b[self.i];
            self.i += 1;
            match c {
                b'"' => return Ok(self.text(&out)),
                b'\\' => {
                    let e = self.peek()?;
                    self.i += 1;
                    match e {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp)
                                && self.b[self.i..].starts_with(b"\\u")
                            {
                                self.i += 2;
                                let lo = self.hex4()?;
                                if (0xDC00..0xE000).contains(&lo) {
                                    cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                }
                            }
                            let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
                            let mut tmp = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
                        }
                        _ => return Err(()),
                    }
                }
                _ => out.push(c),
            }
        }
        Err(())
    }

    fn hex4(&mut self) -> R<u32> {
        let h = self.b.get(self.i..self.i + 4).ok_or(())?;
        self.i += 4;
        u32::from_str_radix(std::str::from_utf8(h).map_err(|_| ())?, 16).map_err(|_| ())
    }
}

// ───────────────────────── streaming array scanner ─────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Seeking,
    InArray,
    Done,
}

/// Finds the elements of a JSON array in a byte stream fed in arbitrary
/// pieces, without parsing or holding more than one element at a time.
/// With `path = None` the array is the top-level value; with `Some(key)` it is
/// the value of `key` in the top-level object.
pub struct ArrayElementScanner {
    path: Option<Vec<u8>>,
    phase: Phase,
    stack: Vec<u8>,
    in_str: bool,
    esc: bool,
    arr_depth: usize,
    // key tracking for the top-level object
    expect_key: bool,
    capturing_key: bool,
    key_buf: Vec<u8>,
    value_key: Option<Vec<u8>>,
    // top-level values back to back instead of an array
    stream: bool,
    // element tracking
    active: bool,
    elem_start: u64,
    pos: u64,
    // after the array of records closed: what follows is not read, only counted
    tail_depth: usize,
    tail_wrapper: bool,
    tail_in_str: bool,
    tail_esc: bool,
    unread: u64,
}

impl ArrayElementScanner {
    pub fn new(path: Option<String>) -> Self {
        ArrayElementScanner {
            path: path.map(|p| p.into_bytes()),
            phase: Phase::Seeking,
            stack: Vec::new(),
            in_str: false,
            esc: false,
            arr_depth: 0,
            expect_key: false,
            capturing_key: false,
            key_buf: Vec::new(),
            value_key: None,
            stream: false,
            active: false,
            elem_start: 0,
            pos: 0,
            tail_depth: 0,
            tail_wrapper: false,
            tail_in_str: false,
            tail_esc: false,
            unread: 0,
        }
    }

    /// A scanner for top-level values written back to back (pretty-printed
    /// objects over many lines, command-line tool output, streaming APIs): each complete
    /// top-level value is one element; whitespace and stray commas between
    /// values are skipped.
    pub fn stream() -> Self {
        let mut s = Self::new(None);
        s.stream = true;
        s.phase = Phase::InArray;
        s.arr_depth = 0;
        s
    }

    /// Source offset of the first byte fed.
    pub fn set_base(&mut self, base: u64) {
        self.pos = base;
    }

    pub fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }

    /// Bytes fed after the array of records closed that were not read: a
    /// second array, a second document, trailing data. White space and the
    /// closing brace of the object around the array are not counted.
    pub fn unread_after_array(&self) -> u64 {
        self.unread
    }

    /// Source `(start, end)` of an element still open now, if any: at the end
    /// of the input, a record the file ended in the middle of.
    pub fn unfinished(&self) -> Option<(u64, u64)> {
        (self.active && self.phase != Phase::Done).then_some((self.elem_start, self.pos))
    }

    /// The array of records closed: whatever follows is only counted.
    fn array_closed(&mut self) {
        self.stack.pop();
        self.phase = Phase::Done;
        self.tail_depth = self.stack.len();
        self.tail_wrapper = self.stack.len() == 1 && self.stack[0] == b'{';
    }

    fn count_tail(&mut self, b: u8) {
        if self.tail_in_str {
            self.unread += 1;
            if self.tail_esc {
                self.tail_esc = false;
            } else if b == b'\\' {
                self.tail_esc = true;
            } else if b == b'"' {
                self.tail_in_str = false;
            }
            return;
        }
        if b.is_ascii_whitespace() {
            return;
        }
        match b {
            b'}' if self.tail_wrapper && self.tail_depth == 1 => {
                // the object that held the array ends: not data
                self.tail_depth = 0;
                self.tail_wrapper = false;
                return;
            }
            b'"' => self.tail_in_str = true,
            b'{' | b'[' => self.tail_depth += 1,
            b'}' | b']' => self.tail_depth = self.tail_depth.saturating_sub(1),
            _ => {}
        }
        self.unread += 1;
    }

    pub fn feed(&mut self, bytes: &[u8], mut on_element: impl FnMut(&[u8]), buf: &mut Vec<u8>) {
        self.feed_with_offsets(bytes, buf, |_, el| on_element(el));
    }

    /// `buf` carries a partial element between calls; keep passing the same one.
    pub fn feed_with_offsets(
        &mut self,
        bytes: &[u8],
        buf: &mut Vec<u8>,
        mut on_element: impl FnMut(u64, &[u8]),
    ) {
        for &b in bytes {
            let at = self.pos;
            self.pos += 1;
            if self.phase == Phase::Done {
                self.count_tail(b);
                continue;
            }
            if self.in_str {
                if self.active {
                    buf.push(b);
                }
                if self.capturing_key && !(b == b'"' && !self.esc) {
                    self.key_buf.push(b);
                }
                if self.esc {
                    self.esc = false;
                } else if b == b'\\' {
                    self.esc = true;
                } else if b == b'"' {
                    self.in_str = false;
                    if self.capturing_key {
                        self.capturing_key = false;
                    }
                }
                continue;
            }
            let at_elem_level = self.phase == Phase::InArray && self.stack.len() == self.arr_depth;
            if self.stream {
                if at_elem_level && !self.active {
                    if b.is_ascii_whitespace() || b == b',' || b == b']' || b == b'}' {
                        continue;
                    }
                    self.active = true;
                    self.elem_start = at;
                    buf.clear();
                }
                buf.push(b);
                match b {
                    b'"' => self.in_str = true,
                    b'{' | b'[' => self.stack.push(b),
                    b'}' | b']' => {
                        self.stack.pop();
                    }
                    _ => {}
                }
                // a value ends when its brackets close; a bare scalar at the
                // whitespace or comma that follows it
                let closed = self.stack.is_empty() && matches!(b, b'}' | b']');
                let scalar_end = self.stack.is_empty()
                    && !self.in_str
                    && !matches!(buf[0], b'{' | b'[')
                    && (b.is_ascii_whitespace() || b == b',');
                if closed || scalar_end {
                    let mut el: &[u8] = buf.trim_ascii_end();
                    if scalar_end {
                        el = el.strip_suffix(b",").unwrap_or(el).trim_ascii_end();
                    }
                    on_element(self.elem_start, el);
                    self.active = false;
                    buf.clear();
                }
                continue;
            }
            if at_elem_level && !self.active {
                if b.is_ascii_whitespace() || b == b',' {
                    continue;
                }
                if b == b']' {
                    self.array_closed();
                    continue;
                }
                self.active = true;
                self.elem_start = at;
                buf.clear();
            }
            if at_elem_level && self.active && (b == b',' || b == b']') {
                let el = buf.trim_ascii_end();
                on_element(self.elem_start, el);
                self.active = false;
                buf.clear();
                if b == b']' {
                    self.array_closed();
                }
                continue;
            }
            if self.active {
                buf.push(b);
            }
            match b {
                b'"' => {
                    self.in_str = true;
                    if self.phase == Phase::Seeking
                        && self.stack.len() == 1
                        && self.stack[0] == b'{'
                        && self.expect_key
                    {
                        self.capturing_key = true;
                        self.key_buf.clear();
                    }
                }
                b':' if self.stack.len() == 1 && self.stack[0] == b'{' => {
                    self.expect_key = false;
                    self.value_key = Some(std::mem::take(&mut self.key_buf));
                }
                b',' if self.stack.len() == 1 && self.stack[0] == b'{' => {
                    self.expect_key = true;
                    self.value_key = None;
                }
                b'{' | b'[' => {
                    if self.phase == Phase::Seeking && b == b'[' {
                        let hit = match &self.path {
                            None => self.stack.is_empty(),
                            Some(p) => {
                                self.stack.len() == 1
                                    && self.stack[0] == b'{'
                                    && self.value_key.as_deref() == Some(p.as_slice())
                            }
                        };
                        if hit {
                            self.stack.push(b);
                            self.arr_depth = self.stack.len();
                            self.phase = Phase::InArray;
                            continue;
                        }
                    }
                    self.stack.push(b);
                    if b == b'{' && self.stack.len() == 1 {
                        self.expect_key = true;
                    }
                }
                b'}' | b']' => {
                    self.stack.pop();
                }
                _ => {}
            }
        }
    }
}
