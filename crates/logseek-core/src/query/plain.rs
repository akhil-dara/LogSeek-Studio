//! Plain words, read against the open file. The nouns come from the file
//! itself: its column names (split into words: "TargetUserName" is target,
//! user, name) and its values ("cancelled" is a value of order_status). Only
//! the small words of the language are known in advance: over, under,
//! between, not, except, empty, by, per, total, average, how many, last.
//! Nothing about any kind of data is built in, and no word is dropped: a word
//! the file does not know is looked for as text, and said so.
//!
//! The result is ordinary search terms (and, when the words ask for one, a
//! summary), each said back in words, with the other readings offered when a
//! word fits more than one column.

use crate::formula::{column_ref, quote};
use std::collections::HashMap;

/// A column as the words see it.
#[derive(Clone, Debug)]
pub struct ColInfo {
    pub name: String,
    /// Its name in words, lower case, plurals made singular.
    pub tokens: Vec<String>,
    pub numeric: bool,
    pub time: bool,
    /// Smallest and largest value, for a column of numbers.
    pub range: Option<(f64, f64)>,
}

/// A word made singular and lower case ("cities" -> "city", "Totals" -> "total").
fn stem(w: &str) -> String {
    let w = w.to_lowercase();
    if let Some(s) = w.strip_suffix("ies") {
        if s.len() >= 2 {
            return format!("{s}y");
        }
    }
    if w.len() > 3 && w.ends_with('s') && !w.ends_with("ss") {
        return w[..w.len() - 1].to_string();
    }
    w
}

/// A column name cut into words: at marks, spaces, digits and capitals.
pub fn name_words(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = name.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let prev = if i > 0 { Some(chars[i - 1]) } else { None };
        let next = chars.get(i + 1).copied();
        let boundary = match prev {
            Some(p) if p.is_alphanumeric() => {
                (p.is_lowercase() && c.is_uppercase())
                    || (p.is_alphabetic() && c.is_ascii_digit())
                    || (p.is_ascii_digit() && c.is_alphabetic())
                    || (p.is_uppercase()
                        && c.is_uppercase()
                        && next.is_some_and(|n| n.is_lowercase()))
            }
            _ => false,
        };
        if boundary && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.iter().map(|w| stem(w)).collect()
}

impl ColInfo {
    pub fn new(name: &str, numeric: bool, time: bool, range: Option<(f64, f64)>) -> ColInfo {
        ColInfo {
            name: name.to_string(),
            tokens: name_words(name),
            numeric,
            time,
            range,
        }
    }
}

/// What the words are read against: the file's columns and values.
#[derive(Clone, Debug, Default)]
pub struct Lexicon {
    pub cols: Vec<ColInfo>,
    /// Lower-case value -> (column, the value as written, rows).
    values: HashMap<String, Vec<(usize, String, u64)>>,
    /// Longest value in words, so phrases are looked up no longer than that.
    longest: usize,
    /// The first and last time of the file (µs), for "the last 2 hours".
    pub time_span: Option<(i64, i64)>,
}

impl Lexicon {
    pub fn new(cols: Vec<ColInfo>) -> Lexicon {
        Lexicon {
            cols,
            ..Default::default()
        }
    }
    pub fn add_value(&mut self, col: usize, value: &str, rows: u64) {
        let low = value.trim().to_lowercase();
        if low.is_empty() {
            return;
        }
        self.longest = self.longest.max(low.split_whitespace().count());
        let e = self.values.entry(low).or_default();
        match e.iter_mut().find(|x| x.0 == col) {
            Some(x) => x.2 += rows,
            None => e.push((col, value.trim().to_string(), rows)),
        }
    }
    /// Values known, over all columns.
    pub fn values(&self) -> usize {
        self.values.values().map(Vec::len).sum()
    }
}

/// Another reading of the same words.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub words: String,
    pub term: String,
}

/// One part of what the words say.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    /// In words: "order_status is cancelled".
    pub words: String,
    /// As a search term.
    pub term: String,
    /// Other readings (when a word fits more than one column), likelier first.
    pub others: Vec<Choice>,
    /// Looked for as text, the file not knowing the word.
    pub free: bool,
    /// A time range.
    pub time: bool,
}

/// A summary the words ask for.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Summary {
    pub by: Vec<String>,
    /// "rows", "total", "average", "different", "smallest", "largest".
    pub measure: String,
    pub column: Option<String>,
    /// Per minute / hour / day (µs).
    pub per: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Recast {
    pub parts: Vec<Part>,
    pub summary: Option<Summary>,
}

impl Recast {
    /// The search: a time range first, then every other part.
    pub fn query(&self) -> String {
        let mut t: Vec<&str> = self
            .parts
            .iter()
            .filter(|p| p.time)
            .map(|p| p.term.as_str())
            .collect();
        t.extend(
            self.parts
                .iter()
                .filter(|p| !p.time)
                .map(|p| p.term.as_str()),
        );
        t.join(" ")
    }
    /// Did any word say more than text to look for?
    pub fn understood(&self) -> bool {
        self.summary.is_some() || self.parts.iter().any(|p| !p.free)
    }
}

/// Words that only join others: not looked for.
const JOINING: &[&str] = &[
    "a", "an", "the", "and", "of", "in", "on", "at", "to", "for", "from", "with", "where", "which",
    "that", "who", "is", "are", "was", "were", "be", "show", "me", "find", "list", "give", "get",
    "all", "any", "rows", "row", "events", "event", "records", "record", "lines", "line",
    "entries", "please", "whose", "has", "have", "had", "there", "it", "its", "them", "those",
    "these", "what", "when", "did", "do", "does", "only", "just", "into", "than", "as", "by",
];
const NOT: &[&str] = &[
    "not",
    "except",
    "without",
    "excluding",
    "exclude",
    "no",
    "isn't",
    "aren't",
    "never",
];

fn num(w: &str) -> Option<f64> {
    let w = w.replace(',', "").to_lowercase();
    let (body, mul) = if let Some(b) = w.strip_suffix('k') {
        (b.to_string(), 1e3)
    } else if let Some(b) = w.strip_suffix('m') {
        (b.to_string(), 1e6)
    } else if let Some(b) = w.strip_suffix('b') {
        (b.to_string(), 1e9)
    } else {
        (w.clone(), 1.0)
    };
    body.parse::<f64>().ok().map(|x| x * mul)
}

/// A number as said ("15,000") and as written in a search ("15000").
fn num_words(x: f64) -> (String, String) {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        let i = x as i64;
        let s = i.abs().to_string();
        let mut g = String::new();
        for (k, c) in s.chars().enumerate() {
            if k > 0 && (s.len() - k) % 3 == 0 {
                g.push(',');
            }
            g.push(c);
        }
        (
            format!("{}{g}", if i < 0 { "-" } else { "" }),
            i.to_string(),
        )
    } else {
        (format!("{x}"), format!("{x}"))
    }
}

fn unit_us(w: &str) -> Option<i64> {
    match stem(w).as_str() {
        "second" | "sec" => Some(1_000_000),
        "minute" | "min" => Some(60_000_000),
        "hour" | "hr" => Some(3_600_000_000),
        "day" => Some(86_400_000_000),
        "week" => Some(7 * 86_400_000_000),
        _ => None,
    }
}

struct Tok {
    raw: String,
    low: String,
    quoted: bool,
}

fn tokens(text: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut it = text.chars().peekable();
    while let Some(&c) = it.peek() {
        if c.is_whitespace() {
            it.next();
            continue;
        }
        if c == '"' {
            it.next();
            let mut s = String::new();
            for ch in it.by_ref() {
                if ch == '"' {
                    break;
                }
                s.push(ch);
            }
            out.push(Tok {
                low: s.to_lowercase(),
                raw: s,
                quoted: true,
            });
            continue;
        }
        let mut s = String::new();
        while let Some(&ch) = it.peek() {
            if ch.is_whitespace() || ch == '"' {
                break;
            }
            s.push(ch);
            it.next();
        }
        // a mark at the end of a word is not part of it ("Hyderabad," "200.")
        let trimmed = s
            .trim_end_matches([',', '.', '?', '!', ';', ':'])
            .to_string();
        let s = if trimmed.is_empty() { s } else { trimmed };
        out.push(Tok {
            low: s.to_lowercase(),
            raw: s,
            quoted: false,
        });
    }
    out
}

/// Columns whose name the words `ws` say: all of them in the name, in order.
/// Better fits first (more of the name said, then the shorter name).
fn columns_named(lex: &Lexicon, ws: &[String]) -> Vec<usize> {
    if ws.is_empty() {
        return Vec::new();
    }
    let joined: String = ws.concat();
    let mut found: Vec<(usize, usize, usize)> = Vec::new(); // (col, words of the name not said, name length)
    for (c, col) in lex.cols.iter().enumerate() {
        let whole: String = col.tokens.concat();
        let fits = if whole == joined || col.name.to_lowercase() == joined {
            true
        } else {
            // the words, in order, each a word of the name
            let mut k = 0;
            for t in &col.tokens {
                if k < ws.len() && *t == ws[k] {
                    k += 1;
                }
            }
            k == ws.len() && ws.iter().all(|w| w.len() >= 3 || ws.len() > 1)
        };
        if fits {
            found.push((c, col.tokens.len().saturating_sub(ws.len()), col.name.len()));
        }
    }
    found.sort_by_key(|x| (x.1, x.2));
    found.into_iter().map(|x| x.0).collect()
}

fn cmp_words(op: &str) -> &'static str {
    match op {
        ">" => "is more than",
        ">=" => "is at least",
        "<" => "is less than",
        _ => "is at most",
    }
}

/// Read `text` against the file.
pub fn recast(text: &str, lex: &Lexicon) -> Recast {
    let toks = tokens(text);
    let mut out = Recast::default();
    let mut summary: Option<Summary> = None;
    let mut not = false;
    // the column the words last named, for what follows it
    let mut named: Vec<usize> = Vec::new();
    let mut i = 0;
    let low = |k: usize| toks.get(k).map_or("", |t| t.low.as_str());
    while i < toks.len() {
        let t = &toks[i];
        if t.quoted {
            flush(lex, &mut named, &mut out.parts, &mut not);
            out.parts.push(free(&t.raw, not));
            not = false;
            i += 1;
            continue;
        }
        let w = t.low.as_str();
        // "other than", "is not"
        if w == "other" && low(i + 1) == "than" {
            not = true;
            i += 2;
            continue;
        }
        if NOT.contains(&w) {
            not = true;
            i += 1;
            continue;
        }
        // a summary: how many / count / total / average / different / smallest / largest
        let measure = match w {
            "how" if low(i + 1) == "many" => Some(("rows", 2)),
            "count" | "number" => Some(("rows", if low(i + 1) == "of" { 2 } else { 1 })),
            "total" | "sum" => Some(("total", if low(i + 1) == "of" { 2 } else { 1 })),
            "average" | "avg" | "mean" => Some(("average", if low(i + 1) == "of" { 2 } else { 1 })),
            "distinct" | "unique" | "different" => Some(("different", 1)),
            "smallest" | "lowest" | "min" | "minimum" => Some(("smallest", 1)),
            "largest" | "highest" | "biggest" | "max" | "maximum" => Some(("largest", 1)),
            _ => None,
        };
        if measure.is_some()
            || matches!(
                w,
                "by" | "per" | "each" | "every" | "grouped" | "hourly" | "daily"
            )
        {
            flush(lex, &mut named, &mut out.parts, &mut not);
        }
        if let Some((m, n)) = measure {
            // "total" may be part of a column's name ("grand total"): a name wins
            let (cols, len) = longest_name(lex, &toks, i);
            if len == 0
                || !cols
                    .iter()
                    .any(|&c| lex.cols[c].tokens.first().map(String::as_str) == Some(&stem(w)))
            {
                let s = summary.get_or_insert_with(Default::default);
                s.measure = m.to_string();
                i += n;
                if m != "rows" {
                    let (cols, len) = longest_name(lex, &toks, i);
                    if let Some(&c) = cols.first() {
                        s.column = Some(lex.cols[c].name.clone());
                        i += len;
                    }
                }
                continue;
            }
        }
        // by / per / each + a column, or per minute / hourly / daily
        if matches!(w, "by" | "per" | "each" | "every" | "grouped") {
            let j = if w == "grouped" && low(i + 1) == "by" {
                i + 2
            } else {
                i + 1
            };
            if let Some(u) = unit_us(low(j)) {
                summary.get_or_insert_with(Default::default).per = Some(u);
                i = j + 1;
                continue;
            }
            let (cols, len) = longest_name(lex, &toks, j);
            if let Some(&c) = cols.first() {
                let s = summary.get_or_insert_with(Default::default);
                s.by.push(lex.cols[c].name.clone());
                i = j + len;
                continue;
            }
        }
        match w {
            "hourly" => {
                summary.get_or_insert_with(Default::default).per = Some(3_600_000_000);
                i += 1;
                continue;
            }
            "daily" => {
                summary.get_or_insert_with(Default::default).per = Some(86_400_000_000);
                i += 1;
                continue;
            }
            _ => {}
        }
        // the last / first N minutes, hours, days of the file
        if matches!(w, "last" | "past" | "first") {
            if let Some((lo, hi)) = lex.time_span {
                let (n, unit, used) =
                    match (num(low(i + 1)), unit_us(low(i + 1)), unit_us(low(i + 2))) {
                        (Some(n), _, Some(u)) => (n, u, 3),
                        (None, Some(u), _) => (1.0, u, 2),
                        _ => (0.0, 0, 0),
                    };
                if used > 0 {
                    flush(lex, &mut named, &mut out.parts, &mut not);
                    let len = (n * unit as f64) as i64;
                    let (a, b) = if w == "first" {
                        (lo, lo + len)
                    } else {
                        (hi - len, hi)
                    };
                    let stamp = |us: i64| crate::types::fmt_time(us).replace(' ', "T");
                    let unit_word = toks[i + used - 1].low.clone();
                    out.parts.push(Part {
                        words: format!(
                            "the {} {}{} of the file",
                            if w == "first" { "first" } else { "last" },
                            if used == 3 {
                                format!("{} ", num_words(n).0)
                            } else {
                                String::new()
                            },
                            unit_word
                        ),
                        term: format!("@{}..{}", stamp(a), stamp(b)),
                        others: Vec::new(),
                        free: false,
                        time: true,
                    });
                    i += used;
                    continue;
                }
            }
        }
        // a comparison: over / under / at least / at most / between X and Y
        let cmp = match (w, low(i + 1)) {
            ("over" | "above" | ">", _) => Some((">", 1)),
            ("more" | "greater" | "bigger" | "larger" | "higher", "than") => Some((">", 2)),
            ("under" | "below" | "<", _) => Some(("<", 1)),
            ("less" | "fewer" | "smaller" | "lower", "than") => Some(("<", 2)),
            ("at", "least") => Some((">=", 2)),
            ("at", "most") => Some(("<=", 2)),
            (">=", _) => Some((">=", 1)),
            ("<=", _) => Some(("<=", 1)),
            ("between", _) => Some(("..", 1)),
            _ => None,
        };
        if let Some((op, n)) = cmp {
            if let Some(x) = num(low(i + n)) {
                let between = if op == ".." && low(i + n + 1) == "and" {
                    num(low(i + n + 2))
                } else {
                    None
                };
                if op != ".." || between.is_some() {
                    let (lo_v, hi_v) = (x, between.unwrap_or(x));
                    // the column named just before, if it holds numbers;
                    // else the columns of numbers whose values reach that far
                    let mut cols: Vec<usize> = named
                        .iter()
                        .copied()
                        .filter(|&c| lex.cols[c].numeric)
                        .collect();
                    if cols.is_empty() {
                        cols = (0..lex.cols.len())
                            .filter(|&c| {
                                lex.cols[c].numeric
                                    && lex.cols[c]
                                        .range
                                        .is_none_or(|(a, b)| lo_v >= a && hi_v <= b)
                            })
                            .collect();
                    }
                    if !cols.is_empty() {
                        let say = |c: usize| -> Choice {
                            let name = &lex.cols[c].name;
                            let cr = column_ref(name);
                            if op == ".." {
                                let (a, b) = (num_words(lo_v.min(hi_v)), num_words(lo_v.max(hi_v)));
                                Choice {
                                    words: format!(
                                        "{}{name} is between {} and {}",
                                        if not { "not: " } else { "" },
                                        a.0,
                                        b.0
                                    ),
                                    term: if not {
                                        format!("NOT ({cr}>={} {cr}<={})", a.1, b.1)
                                    } else {
                                        format!("{cr}>={} {cr}<={}", a.1, b.1)
                                    },
                                }
                            } else {
                                let v = num_words(x);
                                Choice {
                                    words: format!(
                                        "{name} {}{} {}",
                                        if not { "not " } else { "" },
                                        cmp_words(op),
                                        v.0
                                    )
                                    .replace("not is", "is not"),
                                    term: if not {
                                        format!("NOT {cr}{op}{}", v.1)
                                    } else {
                                        format!("{cr}{op}{}", v.1)
                                    },
                                }
                            }
                        };
                        out.parts.push(part(cols.iter().map(|&c| say(c)).collect()));
                        not = false;
                        named.clear();
                        i += n + if between.is_some() { 3 } else { 1 };
                        continue;
                    }
                }
            }
        }
        // empty / blank / missing: the column named just before
        if matches!(w, "empty" | "blank" | "missing" | "null" | "none") && !named.is_empty() {
            let choices: Vec<Choice> = named
                .iter()
                .map(|&c| {
                    let name = &lex.cols[c].name;
                    if not {
                        Choice {
                            words: format!("{name} has a value"),
                            term: format!("{}:*", column_ref(name)),
                        }
                    } else {
                        Choice {
                            words: format!("{name} is empty"),
                            term: format!("NOT {}:*", column_ref(name)),
                        }
                    }
                })
                .collect();
            out.parts.push(part(choices));
            not = false;
            named.clear();
            i += 1;
            continue;
        }
        // the longest run of words that is one of the file's values, or a
        // column's name: whichever is longer (a value on a tie)
        let (vlen, vals) = longest_value(lex, &toks, i);
        let (cols, clen) = longest_name(lex, &toks, i);
        if vlen > 0 && vlen >= clen {
            // in the column just named if it has the value; else every
            // column that has it, the one with most rows first
            let mut v = vals.clone();
            if v.iter().any(|x| named.contains(&x.0)) {
                v.retain(|x| named.contains(&x.0));
            }
            v.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
            let choices: Vec<Choice> = v
                .iter()
                .map(|(c, val, _)| {
                    let name = &lex.cols[*c].name;
                    let term = format!("{}={}", column_ref(name), quote(val));
                    if not {
                        Choice {
                            words: format!("{name} is not {val}"),
                            term: format!("NOT {term}"),
                        }
                    } else {
                        Choice {
                            words: format!("{name} is {val}"),
                            term,
                        }
                    }
                })
                .collect();
            out.parts.push(part(choices));
            not = false;
            named.clear();
            i += vlen;
            continue;
        }
        if clen > 0 {
            flush(lex, &mut named, &mut out.parts, &mut not);
            named = cols;
            i += clen;
            continue;
        }
        if JOINING.contains(&w) {
            i += 1;
            continue;
        }
        flush(lex, &mut named, &mut out.parts, &mut not);
        out.parts.push(free(&t.raw, not));
        not = false;
        i += 1;
    }
    flush(lex, &mut named, &mut out.parts, &mut not);
    if let Some(s) = summary.as_mut() {
        if s.measure.is_empty() {
            s.measure = "rows".into();
        }
    }
    out.summary = summary;
    out
}

/// A column the words named and nothing used: the rows where it has a
/// value (or, after "not", where it is empty). Every word is accounted for.
fn flush(lex: &Lexicon, named: &mut Vec<usize>, parts: &mut Vec<Part>, not: &mut bool) {
    if named.is_empty() {
        return;
    }
    let choices: Vec<Choice> = named
        .iter()
        .map(|&c| {
            let name = &lex.cols[c].name;
            if *not {
                Choice {
                    words: format!("{name} is empty"),
                    term: format!("NOT {}:*", column_ref(name)),
                }
            } else {
                Choice {
                    words: format!("{name} has a value"),
                    term: format!("{}:*", column_ref(name)),
                }
            }
        })
        .collect();
    parts.push(part(choices));
    named.clear();
    *not = false;
}

fn part(mut choices: Vec<Choice>) -> Part {
    let first = choices.remove(0);
    Part {
        words: first.words,
        term: first.term,
        others: choices,
        free: false,
        time: false,
    }
}

fn free(w: &str, not: bool) -> Part {
    Part {
        words: format!("{}\"{w}\" anywhere", if not { "not " } else { "" }),
        term: if not {
            format!("NOT {}", quote(w))
        } else {
            quote(w)
        },
        others: Vec::new(),
        free: true,
        time: false,
    }
}

/// The longest run of words from `i` that names columns (up to four words).
fn longest_name(lex: &Lexicon, toks: &[Tok], i: usize) -> (Vec<usize>, usize) {
    for n in (1..=4.min(toks.len().saturating_sub(i))).rev() {
        if toks[i..i + n].iter().any(|t| t.quoted) {
            continue;
        }
        let ws: Vec<String> = toks[i..i + n].iter().map(|t| stem(&t.low)).collect();
        if n == 1 && JOINING.contains(&toks[i].low.as_str()) {
            continue;
        }
        let cols = columns_named(lex, &ws);
        if !cols.is_empty() {
            return (cols, n);
        }
    }
    (Vec::new(), 0)
}

/// The longest run of words from `i` that is one of the file's values.
fn longest_value(lex: &Lexicon, toks: &[Tok], i: usize) -> (usize, Vec<(usize, String, u64)>) {
    let most = lex.longest.max(1).min(toks.len().saturating_sub(i));
    for n in (1..=most).rev() {
        if toks[i..i + n].iter().any(|t| t.quoted) {
            continue;
        }
        let phrase: Vec<&str> = toks[i..i + n].iter().map(|t| t.low.as_str()).collect();
        if let Some(v) = lex.values.get(&phrase.join(" ")) {
            return (n, v.clone());
        }
    }
    (0, Vec::new())
}

/// Different values a column may have and still be read for its values (a
/// column with more is still found by its name, and its values are looked
/// for as text).
pub const VALUES_PER_COLUMN: usize = 20_000;

/// The lexicon of a store: its column names, the smallest and largest value
/// of each column of numbers and the time span (from the index, no read),
/// and every value of each column with up to `VALUES_PER_COLUMN` different
/// values, with how many rows have it (one read, every core).
pub fn build_lexicon(
    st: &crate::store::Store,
    fs: &super::exec::FormulaSet,
    cache: &super::exec::ChunkCache,
    cancel: &std::sync::atomic::AtomicBool,
) -> crate::error::Result<Lexicon> {
    use crate::types::ColType;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let sch = st.schema();
    let ns = sch.len();
    let mut cols: Vec<ColInfo> = Vec::with_capacity(ns + fs.items.len());
    for (c, col) in sch.columns.iter().enumerate() {
        let numeric = matches!(col.ty, ColType::Int | ColType::Float);
        let mut range: Option<(f64, f64)> = None;
        if numeric {
            for seg in st.segments() {
                if let Some(m) = seg.meta().col(c as u32) {
                    let lo = m.min_i.map(|x| x as f64).or(m.min_f);
                    let hi = m.max_i.map(|x| x as f64).or(m.max_f);
                    if let (Some(lo), Some(hi)) = (lo, hi) {
                        range = Some(match range {
                            Some((a, b)) => (a.min(lo), b.max(hi)),
                            None => (lo, hi),
                        });
                    }
                }
            }
        }
        cols.push(ColInfo::new(
            &col.name,
            numeric,
            matches!(col.ty, ColType::Time) || Some(c) == sch.time_col,
            range,
        ));
    }
    for (name, _) in &fs.items {
        cols.push(ColInfo::new(name, false, false, None));
    }
    let mut lex = Lexicon::new(cols);
    let (mut lo, mut hi) = (i64::MAX, i64::MIN);
    for seg in st.segments() {
        if let (Some(a), Some(b)) = (seg.meta().time_min, seg.meta().time_max) {
            lo = lo.min(a);
            hi = hi.max(b);
        }
    }
    if lo <= hi {
        lex.time_span = Some((lo, hi));
    }
    // the values of the stored text columns
    let text: Vec<usize> = (0..ns)
        .filter(|&c| matches!(sch.columns[c].ty, ColType::Text | ColType::Bool))
        .collect();
    let dropped: Vec<AtomicBool> = text.iter().map(|_| AtomicBool::new(false)).collect();
    let seen: Vec<AtomicUsize> = text.iter().map(|_| AtomicUsize::new(0)).collect();
    let all = super::exec::MatchSet::all(st);
    let parts = super::exec::over_blocks(
        st,
        cache,
        |si| all.segs[si].count() > 0,
        cancel,
        || vec![HashMap::<String, u64>::new(); text.len()],
        |acc, si| {
            let blk = cache.block(st, si)?;
            for (j, &c) in text.iter().enumerate() {
                if dropped[j].load(Ordering::Relaxed) {
                    continue;
                }
                let Some(col) = blk.column(c as u32)? else {
                    continue;
                };
                let mut new = 0usize;
                super::exec::each_in(&all.segs[si], |r| {
                    if let Some(v) = col.text(r).filter(|v| !v.is_empty()) {
                        let e = acc[j].entry(v.into_owned()).or_insert(0);
                        if *e == 0 {
                            new += 1;
                        }
                        *e += 1;
                    }
                });
                if seen[j].fetch_add(new, Ordering::Relaxed) + new > VALUES_PER_COLUMN {
                    dropped[j].store(true, Ordering::Relaxed);
                    acc[j] = HashMap::new();
                }
            }
            Ok(())
        },
    )?;
    let mut merged: Vec<HashMap<String, u64>> = vec![HashMap::new(); text.len()];
    for p in parts {
        for (j, m) in p.into_iter().enumerate() {
            if dropped[j].load(Ordering::Relaxed) {
                continue;
            }
            for (k, v) in m {
                *merged[j].entry(k).or_default() += v;
            }
        }
    }
    for (j, m) in merged.into_iter().enumerate() {
        if dropped[j].load(Ordering::Relaxed) || m.len() > VALUES_PER_COLUMN {
            continue;
        }
        for (v, n) in m {
            lex.add_value(text[j], &v, n);
        }
    }
    Ok(lex)
}
