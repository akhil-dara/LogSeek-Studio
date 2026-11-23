//! Per-column filters and how they combine with the
//! global search into one engine query. The query text the engine runs is
//! always reproducible: `[@range] (global) col:"text" (col="a" OR col="b") | pipes`.

use logseek_core::formula::{column_ref, quote};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq)]
pub struct ColFilter {
    /// Typed in the filter row: "contains" by default, or `>100`, `=x`, `!=x`, `~regex`.
    pub text: String,
    /// Values from the header list; `None` = no value filter.
    pub values: Option<BTreeSet<String>>,
    /// `values` holds the unchecked values: keep every value but these (a
    /// column of millions of values never needs its whole list kept).
    pub values_except: bool,
    /// Switched off from its chip: kept, but filters nothing.
    pub enabled: bool,
    /// Inverted from its chip: keeps the rows that do not match.
    pub invert: bool,
}

impl Default for ColFilter {
    fn default() -> Self {
        ColFilter {
            text: String::new(),
            values: None,
            values_except: false,
            enabled: true,
            invert: false,
        }
    }
}

impl ColFilter {
    pub fn is_active(&self) -> bool {
        !self.text.trim().is_empty() || self.values.is_some()
    }
}

/// Column name → its filter.
pub type Filters = BTreeMap<String, ColFilter>;

/// The syntax the filter row and the value list accept (shown by the **?** button).
pub const HELP: &[(&str, &str)] = &[
    (
        "text  /  !text",
        "contain / do not contain the text (any case)",
    ),
    ("^=text  /  $=text", "start / end with the text"),
    (">5  >=5  <5  <=5", "compare as a number (or a date / time)"),
    ("=x  /  <>x  (or !=x)", "equal / not equal"),
    ("5~10", "lie in the range, ends included"),
    (
        "IN (a, \"b c\", 5)  /  NOT IN (a, b)",
        "are / are not one of the values",
    ),
    ("a%b_c", "match the pattern: % any text, _ one character"),
    ("cmd*", "match the wildcard pattern over the whole value"),
    (
        "/regex/  /  /regex/i",
        "match the regular expression (i: any case)",
    ),
    ("EMPTY  /  NOT EMPTY", "have no value / have a value"),
    (
        "{a} AND {b}  /  {a} OR {b}",
        "match both / either condition",
    ),
];

/// Undo `quote`: \" and \\ become " and \; any other backslash stays.
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\\' && matches!(it.peek(), Some('"') | Some('\\')) {
            out.push(it.next().unwrap());
        } else {
            out.push(c);
        }
    }
    out
}

fn lit(v: &str) -> String {
    if v.parse::<f64>().is_ok() {
        v.to_string()
    } else {
        quote(v)
    }
}

/// `(a, "b c", 5)` → the values.
fn parse_list(s: &str) -> Result<Vec<String>, String> {
    let s = s.trim();
    let Some(inner) = s.strip_prefix('(').and_then(|r| r.strip_suffix(')')) else {
        return Err("IN needs its values in parentheses, like IN (a, b)".into());
    };
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut in_q, mut esc, mut quoted) = (false, false, false);
    for ch in inner.chars() {
        if esc {
            cur.push(ch);
            esc = false;
            continue;
        }
        match ch {
            '\\' if in_q => esc = true,
            '"' => {
                if !in_q && cur.trim().is_empty() {
                    cur.clear(); // spaces before an opening quote are not part of the value
                }
                in_q = !in_q;
                quoted = true;
            }
            ',' if !in_q => {
                let v = if quoted {
                    cur.clone()
                } else {
                    cur.trim().to_string()
                };
                if !v.is_empty() || quoted {
                    out.push(v);
                }
                cur.clear();
                quoted = false;
            }
            _ => cur.push(ch),
        }
    }
    if in_q {
        return Err("a quoted value is not closed".into());
    }
    let v = if quoted { cur } else { cur.trim().to_string() };
    if !v.is_empty() || quoted {
        out.push(v);
    }
    if out.is_empty() {
        return Err("IN needs at least one value".into());
    }
    Ok(out)
}

/// `{a} AND {b} OR {c}` → [(None, a), (And, b), (Or, c)].
fn split_groups(t: &str) -> Result<Vec<(Option<bool>, String)>, String> {
    let mut out = Vec::new();
    let mut rest = t.trim();
    let mut op: Option<bool> = None; // Some(true) = AND, Some(false) = OR
    loop {
        let Some(r) = rest.strip_prefix('{') else {
            return Err("write combined conditions as {a} AND {b}".into());
        };
        let mut depth = 1;
        let mut end = None;
        for (i, ch) in r.char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(e) = end else {
            return Err("a { is not closed".into());
        };
        out.push((op, r[..e].to_string()));
        rest = r[e + 1..].trim_start();
        if rest.is_empty() {
            return Ok(out);
        }
        let up = rest.to_ascii_uppercase();
        if up.starts_with("AND") {
            op = Some(true);
            rest = rest[3..].trim_start();
        } else if up.starts_with("OR") {
            op = Some(false);
            rest = rest[2..].trim_start();
        } else {
            return Err("between groups in braces write AND or OR".into());
        }
    }
}

/// What a value-list button does to the values a search found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
    /// Check every one of them, leaving the other values as they are.
    All,
    /// Uncheck every one of them, leaving the other values as they are.
    None,
    /// Check them and nothing else.
    Only,
}

/// Apply `how` to `found` (every value the search matches). `sel` with
/// `except` is the value list's state: `except` = `sel` holds the unchecked values.
pub fn tick<'a>(
    sel: &mut BTreeSet<String>,
    except: &mut bool,
    found: impl Iterator<Item = &'a String>,
    how: Tick,
) {
    match how {
        Tick::Only => {
            *except = false;
            *sel = found.cloned().collect();
        }
        Tick::All | Tick::None => {
            // checked = not in `sel` when it lists the unchecked, in it otherwise
            let put_in = (how == Tick::All) != *except;
            for v in found {
                if put_in {
                    sel.insert(v.clone());
                } else {
                    sel.remove(v);
                }
            }
        }
    }
}

/// One filter-row expression on column `col` → query text, or why it cannot be used.
pub fn text_clause(col: &str, text: &str) -> Result<Option<String>, String> {
    let c = column_ref(col);
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    let up = t.to_ascii_uppercase();
    let clause = if t.starts_with('{') {
        let groups = split_groups(t)?;
        let mut acc = String::new();
        for (op, g) in groups {
            let Some(inner) = text_clause(col, &g)? else {
                return Err("an empty {} group".into());
            };
            acc = match op {
                None => format!("({inner})"),
                Some(true) => format!("({acc} ({inner}))"),
                Some(false) => format!("({acc} OR ({inner}))"),
            };
        }
        acc
    } else if t.eq_ignore_ascii_case(crate::summary::ERROR_WORD) {
        // a made column's rows that could not be worked out
        format!("iserror({c})")
    } else if up
        .strip_prefix("NOT ")
        .is_some_and(|r| r.trim().eq_ignore_ascii_case(crate::summary::ERROR_WORD))
    {
        format!("NOT iserror({c})")
    } else if matches!(up.as_str(), "NULL" | "EMPTY" | "\"\"" | "''") {
        format!("NOT {c}:\"*\"")
    } else if matches!(up.as_str(), "NOT NULL" | "NOT EMPTY") {
        format!("{c}:\"*\"")
    } else if up.starts_with("NOT IN") && (t[6..].trim_start().starts_with('(')) {
        let vals = parse_list(&t[6..])?;
        format!(
            "NOT ({})",
            vals.iter()
                .map(|v| format!("{c}={}", lit(v)))
                .collect::<Vec<_>>()
                .join(" OR ")
        )
    } else if up.starts_with("IN") && t[2..].trim_start().starts_with('(') {
        let vals = parse_list(&t[2..])?;
        format!(
            "({})",
            vals.iter()
                .map(|v| format!("{c}={}", lit(v)))
                .collect::<Vec<_>>()
                .join(" OR ")
        )
    } else if let Some(r) = t.strip_prefix('/') {
        let Some(last) = r.rfind('/') else {
            return Err("a regular expression ends with /, like /ab+c/".into());
        };
        let (pat, flags) = (&r[..last], &r[last + 1..]);
        if pat.is_empty() {
            return Err("the regular expression is empty".into());
        }
        if !flags.chars().all(|f| f == 'i') {
            return Err("after /regex/ only i (any case) is understood".into());
        }
        let pat = pat.replace("\\/", "/").replace('/', "\\/");
        format!(
            "{c}~/{}{pat}/",
            if flags.contains('i') { "(?i)" } else { "" }
        )
    } else if let Some(r) = t.strip_prefix('~') {
        format!("{c}~/{}/", r.trim().replace('/', "\\/"))
    } else if let Some(v) = t.strip_prefix("^=") {
        format!("{c}:{}", quote(&format!("{}*", v.trim())))
    } else if let Some(v) = t.strip_prefix("$=") {
        format!("{c}:{}", quote(&format!("*{}", v.trim())))
    } else if let Some((op, v)) = ["!=", "<>", ">=", "<=", "=", ">", "<"]
        .iter()
        .find_map(|o| t.strip_prefix(*o).map(|v| (*o, v.trim())))
    {
        if v.is_empty() {
            return Err(format!("{op} needs a value after it"));
        }
        let op = if op == "<>" { "!=" } else { op };
        let v = match v.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
            Some(x) => quote(&unescape(x)),
            None => lit(v),
        };
        format!("{c}{op}{v}")
    } else if let Some(v) = t.strip_prefix('!') {
        if v.trim().is_empty() {
            return Err("! needs text after it, like !svchost".into());
        }
        format!("NOT {c}:{}", quote(v.trim()))
    } else if let Some((a, b)) = t
        .split_once('~')
        .filter(|(a, b)| a.trim().parse::<f64>().is_ok() && b.trim().parse::<f64>().is_ok())
    {
        let (a, b) = (a.trim(), b.trim());
        format!("({c}>={a} {c}<={b})")
    } else if t.contains('%') {
        format!("{c}:{}", quote(&t.replace('%', "*").replace('_', "?")))
    } else {
        // contains (a `*` makes it a whole-value wildcard pattern)
        format!("{c}:{}", quote(t))
    };
    // the engine is the judge: anything it cannot run is reported, never silently ignored
    match logseek_core::query::parse(&clause) {
        Ok(_) => Ok(Some(clause)),
        Err(e) => Err(format!("cannot be used: {}", e.msg)),
    }
}

/// The query text for one column's filter, `None` when it filters nothing (an
/// expression that cannot be used filters nothing and is shown in red).
/// "a OR b OR c" for a few conditions. For many, the same thing nested in
/// halves, `((a OR b) OR (c OR d))`: a list of 10,000 values is then 14
/// levels deep for the engine instead of 10,000 (which overflowed its stack).
fn any_of(parts: &[String]) -> String {
    const FLAT: usize = 32;
    if parts.len() <= FLAT {
        return parts.join(" OR ");
    }
    let (a, b) = parts.split_at(parts.len() / 2);
    format!("({}) OR ({})", any_of(a), any_of(b))
}

pub fn column_clause(col: &str, f: &ColFilter) -> Option<String> {
    if !f.enabled {
        return None;
    }
    let c = column_ref(col);
    let mut parts = Vec::new();
    if let Ok(Some(t)) = text_clause(col, &f.text) {
        parts.push(t);
    }
    if let (Some(vals), true) = (&f.values, f.values_except) {
        if !vals.is_empty() {
            let ors: Vec<String> = vals.iter().map(|v| format!("{c}={}", quote(v))).collect();
            parts.push(format!("NOT ({})", any_of(&ors)));
        }
    } else if let Some(vals) = &f.values {
        if vals.is_empty() {
            // nothing checked: show nothing, honestly
            parts.push(format!("{c}={} {c}!={}", quote(""), quote("")));
        } else {
            let ors: Vec<String> = vals.iter().map(|v| format!("{c}={}", quote(v))).collect();
            parts.push(if ors.len() == 1 {
                ors[0].clone()
            } else {
                format!("({})", any_of(&ors))
            });
        }
    }
    (!parts.is_empty()).then(|| {
        let all = parts.join(" ");
        if f.invert {
            format!("NOT ({all})")
        } else {
            all
        }
    })
}

/// A number with its digits grouped ("1,000"); anything else as it is.
fn num_words(v: &str) -> String {
    match v.parse::<i64>() {
        Ok(n) if n < 0 => format!("-{}", crate::moment::group(n.unsigned_abs())),
        Ok(n) => crate::moment::group(n as u64),
        Err(_) => v.to_string(),
    }
}

/// One filter-row text in words, without the column name: "starts with we".
fn describe_text(t: &str) -> String {
    let t = t.trim();
    if t.starts_with('{') {
        if let Ok(groups) = split_groups(t) {
            return groups
                .iter()
                .map(|(op, g)| match op {
                    None => describe_text(g),
                    Some(true) => format!("and {}", describe_text(g)),
                    Some(false) => format!("or {}", describe_text(g)),
                })
                .collect::<Vec<_>>()
                .join(" ");
        }
    }
    let up = t.to_ascii_uppercase();
    // the special words and lists, in words
    if up == "EMPTY" || up == "NULL" {
        return "is empty".into();
    }
    if up == "NOT EMPTY" || up == "NOT NULL" {
        return "has a value".into();
    }
    if t.eq_ignore_ascii_case(crate::summary::ERROR_WORD) {
        return "could not be worked out".into();
    }
    for (kw, words) in [("NOT IN", "is not one of"), ("IN", "is one of")] {
        if let Some(rest) = up.strip_prefix(kw).map(|_| t[kw.len()..].trim()) {
            if let Ok(items) = parse_list(rest) {
                return format!("{words} {}", items.join(", "));
            }
        }
    }
    if let Some((a, b)) = t.split_once('~') {
        if a.trim().parse::<f64>().is_ok() && b.trim().parse::<f64>().is_ok() {
            return format!(
                "is between {} and {}",
                num_words(a.trim()),
                num_words(b.trim())
            );
        }
    }
    let (verb, rest) = if let Some(v) = t.strip_prefix("^=") {
        ("starts with", v)
    } else if let Some(v) = t.strip_prefix("$=") {
        ("ends with", v)
    } else if let Some((verb, v)) = [
        ("!=", "is not"),
        ("<>", "is not"),
        (">=", "is at least"),
        ("<=", "is at most"),
        ("=", "is"),
        (">", "is more than"),
        ("<", "is less than"),
    ]
    .iter()
    .find_map(|(op, w)| t.strip_prefix(op).map(|v| (*w, v)))
    {
        (verb, v.trim().trim_matches('"'))
    } else if t.starts_with(['=', '<', '>'])
        || t.starts_with("!=")
        || t.starts_with('{')
        || t.starts_with('/')
        || t.to_ascii_uppercase().starts_with("IN")
        || t.to_ascii_uppercase().starts_with("NOT")
        || t.eq_ignore_ascii_case("EMPTY")
        || t.eq_ignore_ascii_case(crate::summary::ERROR_WORD)
    {
        ("", t)
    } else if let Some(v) = t.strip_prefix('!') {
        ("does not contain", v)
    } else {
        ("contains", t)
    };
    let rest = rest.trim();
    if verb.is_empty() {
        rest.to_string()
    } else if verb.starts_with("is ") {
        // a number reads with its digits grouped
        format!("{verb} {}", num_words(rest))
    } else {
        format!("{verb} {rest}")
    }
}

/// A short human label for a filter chip: `host contains 8105`, `host is one of a, b`.
pub fn describe(col: &str, f: &ColFilter) -> String {
    let mut bits = Vec::new();
    let t = f.text.trim();
    if !t.is_empty() {
        bits.push(format!("{col} {}", describe_text(t)));
    }
    let wrap = |s: String| {
        let s = if f.invert { format!("not ({s})") } else { s };
        if f.enabled {
            s
        } else {
            format!("off: {s}")
        }
    };
    // a value list names every value
    let listed = |v: &BTreeSet<String>, sep: &str| v.iter().cloned().collect::<Vec<_>>().join(sep);
    if let (Some(v), true) = (&f.values, f.values_except) {
        bits.push(match v.len() {
            0 => format!("{col}: every value"),
            _ => format!("{col} is not {}", listed(v, " or ")),
        });
    } else if let Some(v) = &f.values {
        bits.push(match v.len() {
            0 => format!("{col}: nothing ticked"),
            1 => format!("{col} is {}", listed(v, "")),
            _ => format!("{col} is one of {}", listed(v, ", ")),
        });
    }
    wrap(bits.join(" · "))
}

/// Split at the first `|` outside quotes, backticks and regex slashes.
fn split_pipes(q: &str) -> (&str, &str) {
    let (mut in_q, mut in_b, mut esc) = (false, false, false);
    for (i, ch) in q.char_indices() {
        if esc {
            esc = false;
            continue;
        }
        match ch {
            '\\' if in_q => esc = true,
            '"' if !in_b => in_q = !in_q,
            '`' if !in_q => in_b = !in_b,
            '|' if !in_q && !in_b => return (&q[..i], &q[i..]),
            _ => {}
        }
    }
    (q, "")
}

/// A leading `@a..b` token: (token, rest).
pub fn split_range(q: &str) -> (&str, &str) {
    let t = q.trim_start();
    let end = t.find(char::is_whitespace).unwrap_or(t.len());
    if t.starts_with('@') && t[..end].contains("..") {
        (&t[..end], &t[end..])
    } else {
        ("", t)
    }
}

/// The query the engine runs: global search AND every active column filter.
/// `skip` leaves one column's *value list* out (to list that column's values).
pub fn compose(global: &str, filters: &Filters, skip_values_of: Option<&str>) -> String {
    let (range, rest) = split_range(global);
    let (filter_part, pipes) = split_pipes(rest);
    let mut out: Vec<String> = Vec::new();
    if !range.is_empty() {
        out.push(range.to_string());
    }
    let g = filter_part.trim();
    let has_filters = filters.values().any(|f| f.is_active() && f.enabled);
    if !g.is_empty() {
        out.push(if has_filters {
            format!("({g})")
        } else {
            g.to_string()
        });
    }
    for (col, f) in filters {
        let f = if skip_values_of == Some(col.as_str()) {
            ColFilter {
                values: None,
                ..f.clone()
            }
        } else {
            f.clone()
        };
        if let Some(c) = column_clause(col, &f) {
            out.push(c);
        }
    }
    let mut q = out.join(" ");
    if !pipes.is_empty() {
        q.push(' ');
        q.push_str(pipes.trim());
    }
    q
}

/// Plain words and phrases of the global search, for highlighting in cells:
/// skips operators, field comparisons, keywords, time ranges and pipes.
pub fn highlight_terms(global: &str) -> Vec<String> {
    let (_, rest) = split_range(global);
    let (filter_part, _) = split_pipes(rest);
    let mut terms = Vec::new();
    let b = filter_part.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                let mut j = i + 1;
                let mut s = String::new();
                while j < b.len() && b[j] != b'"' {
                    if b[j] == b'\\' && j + 1 < b.len() {
                        j += 1;
                    }
                    s.push(b[j] as char);
                    j += 1;
                }
                // a quoted value right after an operator belongs to a comparison
                let after_op = i > 0 && matches!(b[i - 1], b'=' | b':' | b'<' | b'>' | b'~');
                if !after_op && !s.trim().is_empty() {
                    terms.push(
                        String::from_utf8_lossy(&filter_part.as_bytes()[i + 1..j.min(b.len())])
                            .replace("\\\"", "\"")
                            .replace("\\\\", "\\"),
                    );
                }
                i = j + 1;
            }
            c if c.is_ascii_whitespace() || c == b'(' || c == b')' => i += 1,
            _ => {
                let mut j = i;
                while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b'(' && b[j] != b')' {
                    if b[j] == b'"' {
                        // operator followed by a quoted value: skip the whole comparison
                        j += 1;
                        while j < b.len() && b[j] != b'"' {
                            j += if b[j] == b'\\' { 2 } else { 1 };
                        }
                    }
                    j += 1;
                }
                let w = &filter_part[i..j.min(filter_part.len())];
                let kw = matches!(w.to_ascii_uppercase().as_str(), "AND" | "OR" | "NOT");
                let op = w.contains(['=', ':', '<', '>', '~', '!', '`', '*']);
                if !kw && !op && !w.is_empty() {
                    terms.push(w.to_string());
                }
                i = j;
            }
        }
    }
    terms.sort_by_key(|t| std::cmp::Reverse(t.len()));
    terms.dedup();
    terms
}

/// Byte ranges of case-insensitive (ASCII) matches of `terms` in `s`, merged:
/// every match, however many.
pub fn match_ranges(s: &str, terms: &[String]) -> Vec<(usize, usize)> {
    let hay = s.to_ascii_lowercase();
    let mut out: Vec<(usize, usize)> = Vec::new();
    for t in terms {
        let n = t.to_ascii_lowercase();
        if n.is_empty() {
            continue;
        }
        let mut from = 0;
        while let Some(p) = hay[from..].find(&n) {
            let a = from + p;
            out.push((a, a + n.len()));
            from = a + n.len().max(1);
        }
    }
    out.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (a, b) in out {
        match merged.last_mut() {
            Some(l) if a <= l.1 => l.1 = l.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    merged.retain(|&(a, b)| s.is_char_boundary(a) && s.is_char_boundary(b));
    merged
}

/// `text` laid out with every match of `terms` on a highlight (the row view
/// marks what the search found, as the grid does), wrapping at `width`.
pub fn highlighted(
    text: &str,
    terms: &[String],
    font: eframe::egui::FontId,
    color: eframe::egui::Color32,
    hl: eframe::egui::Color32,
) -> eframe::egui::text::LayoutJob {
    use eframe::egui::text::{LayoutJob, TextFormat};
    let mut job = LayoutJob::default();
    let plain = TextFormat::simple(font.clone(), color);
    let hit = TextFormat {
        background: hl,
        ..TextFormat::simple(font, color)
    };
    let mut at = 0;
    for (a, b) in match_ranges(text, terms) {
        if a > at {
            job.append(&text[at..a], 0.0, plain.clone());
        }
        job.append(&text[a..b], 0.0, hit.clone());
        at = b;
    }
    if at < text.len() || text.is_empty() {
        job.append(&text[at..], 0.0, plain);
    }
    job
}

/// True when `b` is a plausible correction of `a` (a typo, not a different
/// word): edit distance at most a third of the longer name, ignoring case.
pub fn similar(a: &str, b: &str) -> bool {
    distance(a, b) <= (a.chars().count().max(b.chars().count()) / 3).max(1)
}

/// Edit distance between two names, ignoring case.
pub fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.to_lowercase().chars().collect();
    let b: Vec<char> = b.to_lowercase().chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            cur[j] = (prev[j] + 1)
                .min(cur[j - 1] + 1)
                .min(prev[j - 1] + usize::from(a[i - 1] != b[j - 1]));
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_match_in_a_value_is_highlighted_however_many() {
        use eframe::egui::{Color32, FontId};
        let v = "powershell -enc a; powershell -enc b; ".repeat(10);
        let terms = vec!["POWERSHELL".to_string()];
        // all twenty, not the first few
        assert_eq!(match_ranges(&v, &terms).len(), 20);
        let job = highlighted(
            &v,
            &terms,
            FontId::monospace(12.0),
            Color32::WHITE,
            Color32::RED,
        );
        let lit = job
            .sections
            .iter()
            .filter(|s| s.format.background == Color32::RED)
            .count();
        assert_eq!(lit, 20);
        // the text is whole, nothing dropped
        assert_eq!(job.text, v);
        // no search: one plain run
        let plain = highlighted(
            "abc",
            &[],
            FontId::monospace(12.0),
            Color32::WHITE,
            Color32::RED,
        );
        assert!(plain
            .sections
            .iter()
            .all(|s| s.format.background != Color32::RED));
    }

    #[test]
    fn rows_that_could_not_be_worked_out_are_a_filter() {
        use crate::summary::{drill_down, ERROR_GROUP, ERROR_WORD};
        assert_eq!(
            text_clause("size_kb", ERROR_WORD).unwrap().as_deref(),
            Some("iserror(size_kb)")
        );
        assert_eq!(
            text_clause("size_kb", &format!("NOT {ERROR_WORD}"))
                .unwrap()
                .as_deref(),
            Some("NOT iserror(size_kb)")
        );
        // the word ERROR alone is an ordinary value (a log level), not this filter
        assert_eq!(
            text_clause("level", "ERROR").unwrap().as_deref(),
            Some("level:\"ERROR\"")
        );
        // the summary's error group drills down to it
        let f = drill_down(&["size kb".to_string()], &[ERROR_GROUP.to_string()]);
        assert_eq!(f, vec![("size kb".to_string(), ERROR_WORD.to_string())]);
        let q = text_clause("size kb", ERROR_WORD).unwrap().unwrap();
        let parsed = logseek_core::query::parse(&q).unwrap();
        assert_eq!(logseek_core::query::to_string(&parsed), q);
        // and so does a click in the Fields panel: the column's filter, not search text
        for p in [
            crate::fields::Pick::Errors("size kb".into()),
            crate::fields::Pick::NoErrors("size kb".into()),
        ] {
            let mut fs = Filters::new();
            crate::fields::apply(&p, &mut fs);
            let q = compose("", &fs, None);
            assert!(q.contains("iserror("), "{q}");
            logseek_core::query::parse(&q).unwrap();
        }
    }
    #[test]
    fn quoted_equals_value_is_unescaped_and_kept_whole() {
        let want = "a \"b\" \\ c ";
        let t = format!("={}", logseek_core::formula::quote(want));
        let clause = text_clause("c", &t).unwrap().unwrap();
        assert!(
            clause.contains(&format!("c={}", logseek_core::formula::quote(want))),
            "{clause}"
        );
    }

    #[test]
    fn disabled_and_inverted_filters_compose() {
        let mut fs = Filters::new();
        fs.insert(
            "host".into(),
            ColFilter {
                text: "^=we".into(),
                ..Default::default()
            },
        );
        fs.insert(
            "user".into(),
            ColFilter {
                text: "bob".into(),
                enabled: false,
                ..Default::default()
            },
        );
        let q = compose("", &fs, None);
        assert!(q.contains("host"), "{q}");
        assert!(
            !q.contains("user"),
            "a switched-off filter filters nothing: {q}"
        );
        fs.get_mut("host").unwrap().invert = true;
        let q = compose("", &fs, None);
        assert!(q.starts_with("NOT ("), "inverted keeps the other rows: {q}");
        assert!(logseek_core::query::parse(&q).is_ok(), "{q}");
        // a new filter is on and not inverted
        let d = ColFilter::default();
        assert!(d.enabled && !d.invert);
    }

    #[test]
    fn chips_say_comparisons_in_words() {
        let w = |t: &str| {
            describe(
                "Image_last",
                &ColFilter {
                    text: t.into(),
                    ..Default::default()
                },
            )
        };
        assert_eq!(w("=WmiPrvSE.exe"), "Image_last is WmiPrvSE.exe");
        assert_eq!(w("!=x"), "Image_last is not x");
        assert_eq!(w(">=5"), "Image_last is at least 5");
        assert_eq!(w(">5"), "Image_last is more than 5");
        assert_eq!(w("<=5"), "Image_last is at most 5");
        assert_eq!(w("<5"), "Image_last is less than 5");
        let off = ColFilter {
            text: "bob".into(),
            enabled: false,
            ..Default::default()
        };
        assert_eq!(describe("user", &off), "off: user contains bob");
        let not = ColFilter {
            text: "bob".into(),
            invert: true,
            ..Default::default()
        };
        assert_eq!(describe("user", &not), "not (user contains bob)");
    }

    #[test]
    fn chip_words_read_as_sentences() {
        let t = |col: &str, text: &str| {
            describe(
                col,
                &ColFilter {
                    text: text.into(),
                    ..Default::default()
                },
            )
        };
        assert_eq!(t("status", "=404"), "status is 404");
        assert_eq!(t("host", "^=web0"), "host starts with web0");
        assert_eq!(t("bytes", ">1000"), "bytes is more than 1,000");
        assert_eq!(t("bytes", "<2500000"), "bytes is less than 2,500,000");
        assert_eq!(t("bytes", "10~20"), "bytes is between 10 and 20");
        assert_eq!(t("user", "EMPTY"), "user is empty");
        assert_eq!(t("user", "NOT EMPTY"), "user has a value");
        assert_eq!(t("user", "IN (alice, bob)"), "user is one of alice, bob");
        assert_eq!(
            t("user", "NOT IN (alice, bob)"),
            "user is not one of alice, bob"
        );
        assert_eq!(t("host", "!srv"), "host does not contain srv");
        assert_eq!(
            t("size_kb", crate::summary::ERROR_WORD),
            "size_kb could not be worked out"
        );
        // a value list names every value
        let vals = |v: &[&str], except: bool| ColFilter {
            values: Some(v.iter().map(|s| s.to_string()).collect()),
            values_except: except,
            ..Default::default()
        };
        assert_eq!(
            describe("user", &vals(&["bob", "alice"], false)),
            "user is one of alice, bob"
        );
        assert_eq!(describe("user", &vals(&["alice"], false)), "user is alice");
        assert_eq!(
            describe("user", &vals(&["alice", "bob"], true)),
            "user is not alice or bob"
        );
    }
    #[test]
    fn combined_conditions_read_as_words_on_the_chip() {
        let f = ColFilter {
            text: "{^=we} AND {!desk}".into(),
            values: None,
            ..Default::default()
        };
        assert_eq!(
            describe("host", &f),
            "host starts with we and does not contain desk"
        );
        let g = ColFilter {
            text: "{8105} OR {EMPTY}".into(),
            values: None,
            ..Default::default()
        };
        assert_eq!(describe("host", &g), "host contains 8105 or is empty");
    }

    #[test]
    fn did_you_mean_only_for_near_misses() {
        assert!(super::similar("EventId", "EventID"));
        assert!(super::similar("hostnme", "hostname"));
        assert!(!super::similar("processname", "_serial"));
        assert!(!super::similar("user", "Image"));
    }

    #[test]
    fn chips_say_what_an_unchecked_value_filter_keeps() {
        let one = ColFilter {
            values: Some(["web1".to_string()].into_iter().collect()),
            values_except: true,
            ..Default::default()
        };
        assert_eq!(describe("host", &one), "host is not web1");
        let two = ColFilter {
            values: Some(["a".to_string(), "b".to_string()].into_iter().collect()),
            values_except: true,
            ..Default::default()
        };
        assert_eq!(describe("host", &two), "host is not a or b");
    }

    #[test]
    fn unchecking_values_filters_them_out_whatever_the_column_holds() {
        // every value but these (no list of the millions kept)
        let f = ColFilter {
            values: Some(
                ["web1".to_string(), "db 2".to_string()]
                    .into_iter()
                    .collect(),
            ),
            values_except: true,
            ..Default::default()
        };
        assert_eq!(
            column_clause("host", &f).as_deref(),
            Some(r#"NOT (host="db 2" OR host="web1")"#)
        );
        // nothing unchecked: no filter at all
        let none = ColFilter {
            values: Some(Default::default()),
            values_except: true,
            ..Default::default()
        };
        assert_eq!(column_clause("host", &none), None);
    }

    fn f(text: &str, vals: Option<&[&str]>) -> ColFilter {
        ColFilter {
            text: text.into(),
            values: vals.map(|v| v.iter().map(|s| s.to_string()).collect()),
            ..Default::default()
        }
    }

    #[test]
    fn column_filters_become_valid_queries() {
        assert_eq!(
            column_clause("host", &f("8105", None)).unwrap(),
            "host:\"8105\""
        );
        assert_eq!(
            column_clause("_serial", &f(">=800000", None)).unwrap(),
            "_serial>=800000"
        );
        assert_eq!(
            column_clause("host", &f("!=srv-b02", None)).unwrap(),
            "host!=\"srv-b02\""
        );
        assert_eq!(
            column_clause("host", &f("", Some(&["a", "b"]))).unwrap(),
            "(host=\"a\" OR host=\"b\")"
        );
        assert_eq!(
            column_clause("geo src", &f("US", None)).unwrap(),
            "`geo src`:\"US\""
        );
        assert!(column_clause("host", &f("  ", None)).is_none());
        for c in [
            "host:\"8105\"",
            "_serial>=800000",
            "(host=\"a\" OR host=\"b\")",
        ] {
            assert!(logseek_core::query::parse(c).is_ok(), "{c}");
        }
    }

    #[test]
    fn the_filter_language_maps_to_engine_queries() {
        let q = |t: &str| text_clause("host", t).unwrap().unwrap();
        assert_eq!(q("8105"), "host:\"8105\"");
        assert_eq!(q("!svc"), "NOT host:\"svc\"");
        assert_eq!(q("^=we81"), "host:\"we81*\"");
        assert_eq!(q("$=desk"), "host:\"*desk\"");
        assert_eq!(q("<>srv-b02"), "host!=\"srv-b02\"");
        assert_eq!(q("5~10"), "(host>=5 host<=10)");
        assert_eq!(
            q("IN (a, \"b c\", 5)"),
            "(host=\"a\" OR host=\"b c\" OR host=5)"
        );
        assert_eq!(q("not in (a,b)"), "NOT (host=\"a\" OR host=\"b\")");
        assert_eq!(q("we%desk_"), "host:\"we*desk?\"");
        assert_eq!(q("/we\\d+srv/i"), "host~/(?i)we\\d+srv/");
        assert_eq!(q("EMPTY"), "NOT host:\"*\"");
        assert_eq!(q("not empty"), "host:\"*\"");
        assert_eq!(
            q("{^=we} AND {!srv}"),
            "((host:\"we*\") (NOT host:\"svc\"))".replace("svc", "srv")
        );
        assert_eq!(q("{=a} OR {=b}"), "((host=\"a\") OR (host=\"b\"))");
        // a tilde inside a path is text, not a range
        assert_eq!(q("C:\\PROGRA~1"), "host:\"C:\\\\PROGRA~1\"");
        // every produced clause is accepted by the engine
        for t in [
            "8105",
            "!svc",
            "^=we81",
            "5~10",
            "IN (a, b)",
            "/x/i",
            "EMPTY",
            "{=a} OR {=b}",
        ] {
            assert!(logseek_core::query::parse(&q(t)).is_ok(), "{t}");
        }
    }

    #[test]
    fn unusable_expressions_say_why() {
        let e = |t: &str| text_clause("host", t).unwrap_err();
        assert!(e("IN (a, b").contains("parentheses"));
        assert!(e("=").contains("needs a value"));
        assert!(e("/abc").contains("ends with /"));
        assert!(e("{a} XOR {b}").contains("AND or OR"));
        assert!(e("!").contains("needs text"));
        assert!(text_clause("host", "   ").unwrap().is_none());
    }

    #[test]
    fn chips_read_in_plain_words() {
        assert_eq!(describe("host", &f("8105", None)), "host contains 8105");
        assert_eq!(describe("host", &f("^=we", None)), "host starts with we");
        assert_eq!(
            describe("host", &f("", Some(&["a", "b", "c"]))),
            "host is one of a, b, c"
        );
        assert_eq!(
            describe("_serial", &f(">=800000", None)),
            "_serial is at least 800,000"
        );
    }

    #[test]
    fn global_and_column_filters_compose_with_correct_precedence() {
        let mut fl = Filters::new();
        fl.insert("host".into(), f("", Some(&["srv-b02", "srv-c03"])));
        fl.insert("_raw".into(), f("EventID>1<", None));
        let q = compose("@2016-08-24..2016-08-25 a OR b | top host", &fl, None);
        assert_eq!(q, "@2016-08-24..2016-08-25 (a OR b) _raw:\"EventID>1<\" (host=\"srv-b02\" OR host=\"srv-c03\") | top host");
        assert!(logseek_core::query::parse(&q).is_ok(), "{q}");
        // listing host's values ignores host's own checklist
        let q2 = compose("", &fl, Some("host"));
        assert_eq!(q2, "_raw:\"EventID>1<\"");
        assert_eq!(compose("powershell", &Filters::new(), None), "powershell");
    }

    #[test]
    fn highlight_terms_are_the_plain_words_only() {
        let t = highlight_terms("@2016-08-24..2016-08-25 powershell \"-enc SQB\" host=host-a01 OR EventID:\"4688\" | top host");
        assert_eq!(t, vec!["powershell".to_string(), "-enc SQB".to_string()]); // longest first
        assert!(highlight_terms("host=x AND port>5").is_empty());
    }

    #[test]
    fn matches_are_case_insensitive_and_merged() {
        let r = match_ranges(
            "PowerShell -nop powershell.exe",
            &["powershell".into(), "shell -n".into()],
        );
        assert_eq!(r, vec![(0, 13), (16, 26)]); // "PowerShell" + "shell -n" merge
    }

    #[test]
    fn ticking_what_a_search_found_leaves_the_other_values_alone() {
        let found: Vec<String> = ["web1", "web2"].iter().map(|s| s.to_string()).collect();
        let set = |v: &[&str]| {
            v.iter()
                .map(|s| s.to_string())
                .collect::<BTreeSet<String>>()
        };
        // "only these" list: select all adds them, clear takes them out
        let (mut sel, mut except) = (set(&["db1"]), false);
        tick(&mut sel, &mut except, found.iter(), Tick::All);
        assert_eq!(
            (sel.clone(), except),
            (set(&["db1", "web1", "web2"]), false)
        );
        tick(&mut sel, &mut except, found.iter(), Tick::None);
        assert_eq!((sel.clone(), except), (set(&["db1"]), false));
        // "all but these" list: clear adds them to the unchecked, select all removes them
        let (mut sel, mut except) = (set(&["db1"]), true);
        tick(&mut sel, &mut except, found.iter(), Tick::None);
        assert_eq!((sel.clone(), except), (set(&["db1", "web1", "web2"]), true));
        tick(&mut sel, &mut except, found.iter(), Tick::All);
        assert_eq!((sel.clone(), except), (set(&["db1"]), true));
        // keep only what was found
        tick(&mut sel, &mut except, found.iter(), Tick::Only);
        assert_eq!((sel, except), (set(&["web1", "web2"]), false));
    }

    #[test]
    fn a_list_of_ten_thousand_values_is_shallow_for_the_engine() {
        let f = ColFilter {
            values: Some((0..10_000).map(|i| format!("sess-{i:06}")).collect()),
            ..Default::default()
        };
        let q = column_clause("session", &f).unwrap();
        // a few values still read plainly
        let few = ColFilter {
            values: Some(["a", "b", "c"].iter().map(|s| s.to_string()).collect()),
            ..Default::default()
        };
        assert_eq!(
            column_clause("host", &few).unwrap(),
            "(host=\"a\" OR host=\"b\" OR host=\"c\")"
        );
        // parsed on a small stack: it would overflow if the list were one long chain
        let parsed = std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || logseek_core::query::parse(&q).is_ok())
            .unwrap()
            .join();
        assert_eq!(parsed.ok(), Some(true));
    }
}
