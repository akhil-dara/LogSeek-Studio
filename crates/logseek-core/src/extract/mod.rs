//! Extraction by example: the user highlights a value; LogSeek synthesises a
//! rule that pulls the same kind of value out of every row, explains it in
//! plain words, previews it, and re-learns from corrections. Nobody has to
//! write a regex (it stays available under "Advanced").

use crate::formula::Step;
use regex::Regex;
use std::collections::HashMap;

/// A highlighted value: `text[start..end]` (byte offsets) is what the user wants.
#[derive(Debug, Clone, PartialEq)]
pub struct Example {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueKind {
    Ipv4,
    Ipv6,
    Email,
    Url,
    Guid,
    Hash,
    WinPath,
    UnixPath,
    DomainUser,
    Timestamp,
    Base64,
    Number,
    Word,
    Token,
    UntilDelimiter,
    NonSpace,
}

impl ValueKind {
    fn rank(self) -> u8 {
        self as u8
    }
    fn noun(self) -> &'static str {
        match self {
            ValueKind::Ipv4 => "IP address",
            ValueKind::Ipv6 => "IPv6 address",
            ValueKind::Email => "email address",
            ValueKind::Url => "URL",
            ValueKind::Guid => "GUID",
            ValueKind::Hash => "hash",
            ValueKind::WinPath | ValueKind::UnixPath => "file path",
            ValueKind::DomainUser => r"DOMAIN\user name",
            ValueKind::Timestamp => "timestamp",
            ValueKind::Base64 => "base64 text",
            ValueKind::Number => "number",
            ValueKind::Word => "word",
            ValueKind::Token => "value",
            ValueKind::UntilDelimiter | ValueKind::NonSpace => "text",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    /// Regex with the wanted value in capture group 1 (shown only under "Advanced").
    pub regex: String,
    pub group: usize,
    pub kind: ValueKind,
    /// Plain-words explanation, e.g. "the IP address after `dst=`".
    pub description: String,
    /// Share of sample rows where the rule finds a value.
    pub coverage: f32,
    /// The extracted value for each sample row (None = no match).
    pub preview: Vec<Option<String>>,
}

impl Suggestion {
    pub fn to_step(&self) -> Step {
        Step::Regex {
            pattern: self.regex.clone(),
            group: self.group,
        }
    }
}

fn full(re: &str, v: &str) -> bool {
    Regex::new(&format!("^(?:{re})$")).is_ok_and(|r| r.is_match(v))
}

fn delim_name(c: char) -> String {
    match c {
        ' ' => "space".into(),
        '\t' => "tab".into(),
        ',' => "comma".into(),
        ';' => "semicolon".into(),
        '|' => "|".into(),
        '"' => "quote".into(),
        '\'' => "quote".into(),
        c => format!("`{c}`"),
    }
}

/// Candidate value patterns for one highlighted value, most specific first.
fn value_patterns(value: &str, right: &str) -> Vec<(ValueKind, String, Option<char>)> {
    let mut v: Vec<(ValueKind, String, Option<char>)> = Vec::new();
    let mut add = |k: ValueKind, re: String| v.push((k, re, None));
    if value.parse::<std::net::Ipv4Addr>().is_ok() {
        add(ValueKind::Ipv4, r"(?:\d{1,3}\.){3}\d{1,3}".into());
    }
    if value.contains(':') && value.parse::<std::net::Ipv6Addr>().is_ok() {
        add(
            ValueKind::Ipv6,
            r"(?:[0-9A-Fa-f]{0,4}:){2,7}[0-9A-Fa-f]{0,4}".into(),
        );
    }
    let email = r"[\w.+-]+@[\w-]+(?:\.[\w-]+)+";
    if full(email, value) {
        add(ValueKind::Email, email.into());
    }
    let url = r#"[A-Za-z][A-Za-z0-9+.-]*://[^\s"'<>]+"#;
    if full(url, value) {
        add(ValueKind::Url, url.into());
    }
    let guid = r"[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}";
    if full(guid, value) {
        add(ValueKind::Guid, guid.into());
    }
    if matches!(value.len(), 32 | 40 | 64 | 128) && value.bytes().all(|b| b.is_ascii_hexdigit()) {
        add(
            ValueKind::Hash,
            format!(r"\b[0-9A-Fa-f]{{{}}}\b", value.len()),
        );
    }
    let win = r#"(?:[A-Za-z]:|\\)\\[^\s"'|<>]*"#;
    if full(win, value) {
        add(ValueKind::WinPath, win.into());
    }
    let unix = r#"/[^\s"'|<>]*/[^\s"'|<>]*"#;
    if full(unix, value) {
        add(ValueKind::UnixPath, unix.into());
    }
    let du = r"[\w.-]+\\[\w.$-]+";
    if full(du, value) {
        add(ValueKind::DomainUser, du.into());
    }
    if crate::time::parse_time(value).is_some() && !value.bytes().all(|b| b.is_ascii_digit()) {
        let mut re = String::new();
        let mut digits = 0;
        for ch in value.chars() {
            if ch.is_ascii_digit() {
                digits += 1;
                continue;
            }
            if digits > 0 {
                re.push_str(&format!(r"\d{{{digits}}}"));
                digits = 0;
            }
            re.push_str(&regex::escape(&ch.to_string()));
        }
        if digits > 0 {
            re.push_str(&format!(r"\d{{{digits}}}"));
        }
        add(ValueKind::Timestamp, re);
    }
    let b64ish = value.len() >= 8
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
        && value.bytes().any(|b| b.is_ascii_uppercase())
        && value.bytes().any(|b| b.is_ascii_lowercase())
        && value
            .bytes()
            .any(|b| b.is_ascii_digit() || b == b'=' || b == b'+' || b == b'/');
    if b64ish {
        add(ValueKind::Base64, "[A-Za-z0-9+/]+={0,2}".into());
    }
    if full(r"-?\d+(?:\.\d+)?", value) {
        add(ValueKind::Number, r"-?\d+(?:\.\d+)?".into());
    }
    if full(r"\w+", value) {
        add(ValueKind::Word, r"\w+".into());
    }
    // token-shape generalisation: runs of digits / letters / alnum, literals kept
    let mut tok = String::new();
    let mut run: Option<u8> = None; // 0 digits, 1 letters, 2 mixed
    let flush = |tok: &mut String, run: &mut Option<u8>| match run.take() {
        Some(0) => tok.push_str(r"\d+"),
        Some(1) => tok.push_str("[A-Za-z]+"),
        Some(_) => tok.push_str("[A-Za-z0-9]+"),
        None => {}
    };
    for ch in value.chars() {
        let class = if ch.is_ascii_digit() {
            Some(0)
        } else if ch.is_ascii_alphabetic() {
            Some(1)
        } else {
            None
        };
        match (class, run) {
            (Some(c), Some(r)) if c == r || r == 2 => {}
            (Some(_), Some(_)) => run = Some(2),
            (Some(c), None) => run = Some(c),
            (None, _) => {
                flush(&mut tok, &mut run);
                tok.push_str(&regex::escape(&ch.to_string()));
            }
        }
    }
    flush(&mut tok, &mut run);
    if !tok.is_empty() {
        add(ValueKind::Token, tok);
    }
    if let Some(d) = right.chars().next() {
        if !value.contains(d) {
            let re = format!("[^{}]+", regex::escape(&d.to_string()));
            v.push((ValueKind::UntilDelimiter, re, Some(d)));
        }
    }
    if !value.chars().any(char::is_whitespace) {
        v.push((ValueKind::NonSpace, r"\S+".into(), Some(' ')));
    }
    v
}

/// Literal context just before the selection: separators plus the preceding
/// word (`user=`, `dst=`, `-enc `, `pid: `). `"^"` when at the start.
fn left_anchor(left: &str) -> Option<String> {
    if left.is_empty() {
        return Some("^".into());
    }
    let chars: Vec<char> = left.chars().collect();
    let mut i = chars.len();
    while i > 0 && !chars[i - 1].is_alphanumeric() {
        i -= 1;
    }
    let sep_start = i;
    while i > 0
        && (chars[i - 1].is_alphanumeric() || matches!(chars[i - 1], '_' | '-' | '.' | '@' | '#'))
    {
        i -= 1;
    }
    let anchor: String = chars[i..].iter().collect();
    let _ = sep_start;
    (!anchor.is_empty()).then_some(anchor)
}

fn describe(kind: ValueKind, anchor: Option<&str>, delim: Option<char>) -> String {
    let noun = kind.noun();
    let what = match (kind, delim) {
        (ValueKind::UntilDelimiter | ValueKind::NonSpace, Some(d)) => {
            return match anchor {
                Some("^") => format!("the text at the start up to the next {}", delim_name(d)),
                Some(a) => format!("the text after `{}` up to the next {}", a, delim_name(d)),
                None => format!("the first text up to the next {}", delim_name(d)),
            };
        }
        _ => noun,
    };
    match anchor {
        Some("^") => format!("the {what} at the start"),
        Some(a) => format!("the {what} after `{a}`"),
        None => format!("the first {what}"),
    }
}

fn verify(re: &Regex, pos: &[Example], neg: &[String]) -> bool {
    pos.iter().all(|e| {
        re.captures(&e.text)
            .and_then(|c| c.get(1))
            .is_some_and(|m| m.start() == e.start && m.end() == e.end)
    }) && neg.iter().all(|n| !re.is_match(n))
}

/// Ranked extraction rules consistent with every positive example and no
/// negative row, previewed on `samples`.
pub fn suggest_extractions(pos: &[Example], neg: &[String], samples: &[String]) -> Vec<Suggestion> {
    let Some(first) = pos.first() else {
        return Vec::new();
    };
    if first.end <= first.start || first.end > first.text.len() {
        return Vec::new();
    }
    let value = &first.text[first.start..first.end];
    let left = &first.text[..first.start];
    let right = &first.text[first.end..];
    let anchors: Vec<Option<String>> = {
        let mut a = vec![None];
        if let Some(x) = left_anchor(left) {
            a.push(Some(x));
        }
        a
    };
    let mut out: Vec<(Suggestion, bool)> = Vec::new();
    for (kind, vre, delim) in value_patterns(value, right) {
        for anchor in &anchors {
            let pat = match anchor.as_deref() {
                None => format!("({vre})"),
                Some("^") => format!("^({vre})"),
                Some(a) => format!("{}({vre})", regex::escape(a)),
            };
            let Ok(re) = Regex::new(&pat) else { continue };
            if !verify(&re, pos, neg) {
                continue;
            }
            let preview: Vec<Option<String>> = samples
                .iter()
                .map(|s| {
                    re.captures(s)
                        .and_then(|c| c.get(1))
                        .map(|m| m.as_str().to_string())
                })
                .collect();
            let hits = preview.iter().filter(|p| p.is_some()).count();
            let coverage = if samples.is_empty() {
                0.0
            } else {
                hits as f32 / samples.len() as f32
            };
            let description = describe(kind, anchor.as_deref(), delim);
            out.push((
                Suggestion {
                    regex: pat,
                    group: 1,
                    kind,
                    description,
                    coverage,
                    preview,
                },
                anchor.is_some(),
            ));
        }
    }
    // most specific kind first; then real coverage; then anchored (more precise) on ties
    out.sort_by(|(a, aa), (b, ba)| {
        a.kind
            .rank()
            .cmp(&b.kind.rank())
            .then(
                b.coverage
                    .partial_cmp(&a.coverage)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(ba.cmp(aa))
    });
    let mut seen = std::collections::HashSet::new();
    out.into_iter()
        .map(|(s, _)| s)
        .filter(|s| seen.insert(s.regex.clone()))
        .take(8)
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct KvSuggestion {
    pub key: String,
    pub regex: String,
    pub coverage: f32,
}

/// `key=value` / `key: value` pairs that recur across rows.
pub fn suggest_key_values(samples: &[String]) -> Vec<KvSuggestion> {
    let re =
        Regex::new(r#"(?:^|[\s,;|])([A-Za-z_][\w.-]{0,40})\s*[=:]\s*("[^"]*"|[^\s,;|]+)"#).unwrap();
    let mut order: Vec<String> = Vec::new();
    let mut count: HashMap<String, usize> = HashMap::new();
    for s in samples {
        let mut seen = std::collections::HashSet::new();
        for c in re.captures_iter(s) {
            let k = c[1].to_string();
            if seen.insert(k.clone()) {
                if !count.contains_key(&k) {
                    order.push(k.clone());
                }
                *count.entry(k).or_insert(0) += 1;
            }
        }
    }
    let n = samples.len().max(1) as f32;
    let mut v: Vec<KvSuggestion> = order
        .into_iter()
        .map(|k| KvSuggestion {
            regex: format!(
                r#"(?:^|[\s,;|]){}\s*[=:]\s*"?([^\s,;|"]+)"#,
                regex::escape(&k)
            ),
            coverage: count[&k] as f32 / n,
            key: k,
        })
        .collect();
    v.sort_by(|a, b| {
        b.coverage
            .partial_cmp(&a.coverage)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    v
}

#[derive(Debug, Clone, PartialEq)]
pub struct SplitSuggestion {
    pub delimiter: char,
    pub fields: usize,
}

/// A delimiter that splits ≥ 90 % of rows into the same number (≥ 2) of pieces.
pub fn suggest_split(samples: &[String]) -> Option<SplitSuggestion> {
    if samples.is_empty() {
        return None;
    }
    for d in [';', ',', '\t', '|', ' '] {
        let mut freq: HashMap<usize, usize> = HashMap::new();
        for s in samples {
            *freq.entry(s.split(d).count()).or_insert(0) += 1;
        }
        let (&fields, &n) = freq.iter().max_by_key(|(f, n)| (**n, **f))?;
        if fields >= 2 && n as f32 >= samples.len() as f32 * 0.9 {
            return Some(SplitSuggestion {
                delimiter: d,
                fields,
            });
        }
    }
    None
}
