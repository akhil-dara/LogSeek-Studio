//! Text log formats (Apache, nginx, W3C/IIS, key=value, syslog) compiled to a
//! small step program, run by one byte-level executor. Formats are data: a
//! pasted `LogFormat` / `log_format` / `#Fields` line compiles to the same
//! program as a built-in, so every format is testable and fast.

use std::borrow::Cow;

/// How far a field reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Until {
    /// To the next space (or the end of the line).
    Space,
    /// To the next occurrence of this byte (which must appear).
    Char(u8),
    /// A `"…"` string; `\"` and `\\` inside are unescaped.
    Quoted,
    /// A `[…]` group (Apache `%t`).
    Bracketed,
    /// Everything left on the line.
    Rest,
    /// Space-separated tokens that continue while a token ends with `,`
    /// (a forwarded chain such as `203.0.113.66, 23.53.242.133, 10.0.0.5:37593`).
    Chain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Lit(Vec<u8>),
    Field { name: String, until: Until },
    Skip(Until),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Line,
    KeyValue,
    W3c {
        repair: Option<String>,
    },
    /// RFC 3164 (BSD) syslog: `<PRI>Mmm dd hh:mm:ss host tag[pid]: message`.
    Syslog3164,
    /// RFC 5424 syslog: `<PRI>1 TIMESTAMP HOST APP PROCID MSGID [SD] MSG`.
    Syslog5424,
}

const SYSLOG_3164: [&str; 6] = ["priority", "time", "host", "app", "pid", "message"];
const SYSLOG_5424: [&str; 9] = [
    "priority",
    "version",
    "time",
    "host",
    "app",
    "procid",
    "msgid",
    "structured_data",
    "message",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Format {
    pub name: String,
    pub steps: Vec<Step>,
    pub kind: Kind,
}

/// The pseudo-field holding any text after the last step.
pub const REST: &str = "_rest";

impl Format {
    /// Field names in step order; line formats end with `_rest`.
    pub fn fields(&self) -> Vec<String> {
        match self.kind {
            Kind::KeyValue => return vec![REST.to_string()],
            Kind::Syslog3164 => return SYSLOG_3164.iter().map(|s| s.to_string()).collect(),
            Kind::Syslog5424 => return SYSLOG_5424.iter().map(|s| s.to_string()).collect(),
            _ => {}
        }
        let mut out: Vec<String> = self
            .steps
            .iter()
            .filter_map(|s| match s {
                Step::Field { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        if self.kind == Kind::Line {
            out.push(REST.to_string());
        }
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseStatus {
    Full,
    Partial,
    Unparsed,
}

#[derive(Debug, Clone)]
pub struct Parsed<'l> {
    /// One entry per `Format::fields()` name.
    pub values: Vec<Option<Cow<'l, str>>>,
    /// key=value pairs (key=value formats).
    pub extra: Vec<(Cow<'l, str>, Cow<'l, str>)>,
    pub status: ParseStatus,
    /// Byte offset where parsing stopped, for a line that did not fit.
    pub stopped_at: Option<usize>,
}

fn text(b: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(b)
}

/// Reads one field at `pos`; returns (value, new position) or None.
fn take<'l>(line: &'l [u8], pos: usize, until: &Until) -> Option<(Cow<'l, str>, usize)> {
    let rest = &line[pos..];
    match until {
        Until::Space => {
            if rest.is_empty() {
                return None;
            }
            let n = rest.iter().position(|&b| b == b' ').unwrap_or(rest.len());
            Some((text(&rest[..n]), pos + n))
        }
        Until::Char(c) => {
            let n = rest.iter().position(|b| b == c)?;
            Some((text(&rest[..n]), pos + n))
        }
        Until::Rest => Some((text(rest), line.len())),
        Until::Bracketed => {
            if rest.first() != Some(&b'[') {
                return None;
            }
            let n = rest.iter().position(|&b| b == b']')?;
            Some((text(&rest[1..n]), pos + n + 1))
        }
        Until::Quoted => {
            if rest.first() != Some(&b'"') {
                return None;
            }
            let mut i = 1;
            let mut escaped = false;
            while i < rest.len() {
                match rest[i] {
                    b'\\' => {
                        escaped = true;
                        i += 2;
                        continue;
                    }
                    b'"' => break,
                    _ => i += 1,
                }
            }
            if i >= rest.len() {
                return None;
            }
            let inner = &rest[1..i];
            let v = if escaped {
                let mut out = Vec::with_capacity(inner.len());
                let mut j = 0;
                while j < inner.len() {
                    if inner[j] == b'\\'
                        && j + 1 < inner.len()
                        && matches!(inner[j + 1], b'"' | b'\\')
                    {
                        out.push(inner[j + 1]);
                        j += 2;
                    } else {
                        out.push(inner[j]);
                        j += 1;
                    }
                }
                Cow::Owned(String::from_utf8_lossy(&out).into_owned())
            } else {
                text(inner)
            };
            Some((v, pos + i + 1))
        }
        Until::Chain => {
            if rest.is_empty() {
                return None;
            }
            let mut end = 0;
            loop {
                let n = rest[end..]
                    .iter()
                    .position(|&b| b == b' ')
                    .map_or(rest.len(), |k| end + k);
                let token_ends_comma = n > end && rest[n - 1] == b',';
                end = n;
                if token_ends_comma && end < rest.len() {
                    end += 1; // the space after the comma
                    continue;
                }
                break;
            }
            Some((text(&rest[..end]), pos + end))
        }
    }
}

/// Parses one line with `f`. A trailing `\r` is never part of a field.
pub fn parse_line<'l>(f: &Format, line: &'l [u8]) -> Parsed<'l> {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    match f.kind {
        Kind::KeyValue => return parse_kv(line),
        Kind::Syslog3164 => return parse_3164(line),
        Kind::Syslog5424 => return parse_5424(line),
        _ => {}
    }
    let names = f.fields();
    let mut values: Vec<Option<Cow<'l, str>>> = vec![None; names.len()];
    let total = names.len() - usize::from(f.kind == Kind::Line);
    let mut pos = 0usize;
    let mut fi = 0usize;
    let mut stopped = None;
    for step in &f.steps {
        match step {
            Step::Lit(l) => {
                if line[pos..].starts_with(l) {
                    pos += l.len();
                } else {
                    stopped = Some(pos);
                    break;
                }
            }
            Step::Field { until, .. } => match take(line, pos, until) {
                Some((v, p)) => {
                    values[fi] = Some(v);
                    fi += 1;
                    pos = p;
                }
                None => {
                    stopped = Some(pos);
                    break;
                }
            },
            Step::Skip(until) => match take(line, pos, until) {
                Some((_, p)) => pos = p,
                None => {
                    stopped = Some(pos);
                    break;
                }
            },
        }
    }
    let set = values.iter().filter(|v| v.is_some()).count();
    let status = match stopped {
        None => {
            if f.kind == Kind::Line {
                let tail = &line[pos..];
                let tail = tail.strip_prefix(b" ").unwrap_or(tail);
                if !tail.is_empty() {
                    values[names.len() - 1] = Some(text(tail));
                }
            }
            ParseStatus::Full
        }
        Some(_) if set > 0 && set * 2 >= total => ParseStatus::Partial,
        Some(_) => ParseStatus::Unparsed,
    };
    Parsed {
        values,
        extra: Vec::new(),
        status,
        stopped_at: stopped,
    }
}

fn parsed<'l>(
    values: Vec<Option<Cow<'l, str>>>,
    status: ParseStatus,
    stopped_at: Option<usize>,
) -> Parsed<'l> {
    Parsed {
        values,
        extra: Vec::new(),
        status,
        stopped_at,
    }
}

fn is_kv_sep(b: u8) -> bool {
    matches!(b, b' ' | b';' | b',' | b'\t')
}

fn is_key_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b'@')
}

/// `a=1 b="x y" c='z';d=4,e=5`: every pair into `extra`; other words into
/// `_rest`. Full when every word is a pair; partial when pairs outnumber the
/// other words (a short header then pairs); otherwise not a key=value line.
fn parse_kv(line: &[u8]) -> Parsed<'_> {
    let mut extra: Vec<(Cow<'_, str>, Cow<'_, str>)> = Vec::new();
    let mut stray: Vec<&[u8]> = Vec::new();
    let mut i = 0;
    while i < line.len() {
        while i < line.len() && is_kv_sep(line[i]) {
            i += 1;
        }
        if i >= line.len() {
            break;
        }
        let ks = i;
        while i < line.len() && is_key_char(line[i]) {
            i += 1;
        }
        let key_ok = i > ks && (line[ks].is_ascii_alphabetic() || line[ks] == b'_');
        if key_ok && i < line.len() && line[i] == b'=' {
            let key = text(&line[ks..i]);
            i += 1;
            let value = if i < line.len() && matches!(line[i], b'"' | b'\'') {
                let q = line[i];
                i += 1;
                let vs = i;
                let mut out: Vec<u8> = Vec::new();
                let mut escaped = false;
                while i < line.len() && line[i] != q {
                    if line[i] == b'\\' && i + 1 < line.len() {
                        escaped = true;
                        out.push(line[i + 1]);
                        i += 2;
                        continue;
                    }
                    out.push(line[i]);
                    i += 1;
                }
                let v = if escaped {
                    Cow::Owned(String::from_utf8_lossy(&out).into_owned())
                } else {
                    text(&line[vs..i])
                };
                i = (i + 1).min(line.len());
                v
            } else {
                let vs = i;
                while i < line.len() && !is_kv_sep(line[i]) {
                    i += 1;
                }
                text(&line[vs..i])
            };
            extra.push((key, value));
        } else {
            i = ks;
            while i < line.len() && line[i] != b' ' {
                i += 1;
            }
            stray.push(&line[ks..i]);
        }
    }
    let status = if extra.is_empty() {
        ParseStatus::Unparsed
    } else if stray.is_empty() {
        ParseStatus::Full
    } else if extra.len() > stray.len() {
        ParseStatus::Partial
    } else {
        ParseStatus::Unparsed
    };
    let rest = (!stray.is_empty()).then(|| {
        let joined: Vec<u8> = stray.join(&b' ');
        Cow::Owned(String::from_utf8_lossy(&joined).into_owned())
    });
    Parsed {
        values: vec![rest],
        extra,
        status,
        stopped_at: None,
    }
}

/// `<34>` at the start: the priority and where the rest begins.
fn priority(line: &[u8]) -> (Option<Cow<'_, str>>, usize) {
    if line.first() == Some(&b'<') {
        if let Some(e) = line.iter().take(6).position(|&b| b == b'>') {
            if e > 1 && line[1..e].iter().all(u8::is_ascii_digit) {
                return (Some(text(&line[1..e])), e + 1);
            }
        }
    }
    (None, 0)
}

fn parse_3164(line: &[u8]) -> Parsed<'_> {
    let mut v: Vec<Option<Cow<'_, str>>> = vec![None; SYSLOG_3164.len()];
    let (pri, mut p) = priority(line);
    v[0] = pri;
    // Mmm dd hh:mm:ss (day space-padded)
    let ts = line.get(p..p + 15);
    let ok = ts.is_some_and(|t| {
        t[..3].iter().all(u8::is_ascii_alphabetic)
            && t[3] == b' '
            && (t[4] == b' ' || t[4].is_ascii_digit())
            && t[5].is_ascii_digit()
            && t[6] == b' '
            && t[9] == b':'
            && t[12] == b':'
            && [7, 8, 10, 11, 13, 14]
                .iter()
                .all(|&k| t[k].is_ascii_digit())
    });
    if !ok {
        return parsed(v, ParseStatus::Unparsed, Some(p));
    }
    v[1] = Some(text(&line[p..p + 15]));
    p += 15;
    if line.get(p) != Some(&b' ') {
        return parsed(v, ParseStatus::Partial, Some(p));
    }
    p += 1;
    let hn = line[p..]
        .iter()
        .position(|&b| b == b' ')
        .unwrap_or(line.len() - p);
    if hn == 0 {
        return parsed(v, ParseStatus::Partial, Some(p));
    }
    v[2] = Some(text(&line[p..p + hn]));
    p = (p + hn + 1).min(line.len());
    let rest = &line[p..];
    // tag[pid]: message  or  tag: message  or just a message
    let tn = rest
        .iter()
        .position(|&b| matches!(b, b':' | b'[' | b' '))
        .unwrap_or(rest.len());
    let mut m = 0;
    if tn > 0 && tn < rest.len() {
        if rest[tn] == b'[' {
            if let Some(close) = rest[tn..].iter().position(|&b| b == b']').map(|k| tn + k) {
                if rest.get(close + 1) == Some(&b':') {
                    v[3] = Some(text(&rest[..tn]));
                    v[4] = Some(text(&rest[tn + 1..close]));
                    m = close + 2;
                }
            }
        } else if rest[tn] == b':' {
            v[3] = Some(text(&rest[..tn]));
            m = tn + 1;
        }
    }
    let msg = &rest[m..];
    let msg = msg.strip_prefix(b" ").unwrap_or(msg);
    v[5] = (!msg.is_empty()).then(|| text(msg));
    parsed(v, ParseStatus::Full, None)
}

fn parse_5424(line: &[u8]) -> Parsed<'_> {
    let mut v: Vec<Option<Cow<'_, str>>> = vec![None; SYSLOG_5424.len()];
    let (pri, mut p) = priority(line);
    if pri.is_none() {
        return parsed(v, ParseStatus::Unparsed, Some(0));
    }
    v[0] = pri;
    let vn = line[p..].iter().position(|&b| b == b' ').unwrap_or(0);
    if vn == 0 || !line[p..p + vn].iter().all(u8::is_ascii_digit) {
        return parsed(v, ParseStatus::Unparsed, Some(p));
    }
    v[1] = Some(text(&line[p..p + vn]));
    p += vn + 1;
    fn nil(t: &[u8]) -> Option<Cow<'_, str>> {
        (t != b"-").then(|| text(t))
    }
    for k in 2..7 {
        let n = line[p..]
            .iter()
            .position(|&b| b == b' ')
            .unwrap_or(line.len() - p);
        if n == 0 {
            return parsed(v, ParseStatus::Partial, Some(p));
        }
        v[k] = nil(&line[p..p + n]);
        p = (p + n + 1).min(line.len());
    }
    // structured data: "-" or one or more [ … ] elements (quotes may hold "]")
    if line.get(p) == Some(&b'-') {
        p = (p + 2).min(line.len());
    } else if line.get(p) == Some(&b'[') {
        let start = p;
        while line.get(p) == Some(&b'[') {
            let mut in_q = false;
            p += 1;
            while p < line.len() {
                match line[p] {
                    b'\\' => p += 1,
                    b'"' => in_q = !in_q,
                    b']' if !in_q => break,
                    _ => {}
                }
                p += 1;
            }
            p = (p + 1).min(line.len());
        }
        v[7] = Some(text(&line[start..p]));
        p = (p + 1).min(line.len());
    }
    let msg = &line[p.min(line.len())..];
    let msg = msg.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(msg);
    v[8] = (!msg.is_empty()).then(|| text(msg));
    parsed(v, ParseStatus::Full, None)
}

/// Compiles an nginx `log_format` string (`$remote_addr - $remote_user [$time_local] "$request" …`).
pub fn compile_nginx(log_format: &str) -> Result<Format, String> {
    let b = log_format.trim().trim_matches('\'').as_bytes();
    let mut steps: Vec<Step> = Vec::new();
    let mut lit: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'$' {
            lit.push(b[i]);
            i += 1;
            continue;
        }
        let (name, mut j) = if b.get(i + 1) == Some(&b'{') {
            let close = b[i..]
                .iter()
                .position(|&c| c == b'}')
                .ok_or("a ${ is not closed")?
                + i;
            (
                String::from_utf8_lossy(&b[i + 2..close]).into_owned(),
                close + 1,
            )
        } else {
            let mut j = i + 1;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            (String::from_utf8_lossy(&b[i + 1..j]).into_owned(), j)
        };
        if name.is_empty() {
            lit.push(b'$');
            i += 1;
            continue;
        }
        let quoted = lit.last() == Some(&b'"') && b.get(j) == Some(&b'"');
        if quoted {
            lit.pop();
            j += 1;
        }
        if !lit.is_empty() {
            steps.push(Step::Lit(std::mem::take(&mut lit)));
        }
        let until = if quoted {
            Until::Quoted
        } else {
            match b.get(j) {
                Some(&c) if c != b' ' && c != b'$' => Until::Char(c),
                _ if name == "remote_addr" => Until::Chain,
                _ => Until::Space,
            }
        };
        steps.push(field(&name, until));
        i = j;
    }
    if !lit.is_empty() {
        steps.push(Step::Lit(lit));
    }
    if !steps.iter().any(|s| matches!(s, Step::Field { .. })) {
        return Err("the format has no $variables".into());
    }
    Ok(Format {
        name: "nginx log_format".into(),
        steps,
        kind: Kind::Line,
    })
}

fn field(name: &str, until: Until) -> Step {
    Step::Field {
        name: name.to_string(),
        until,
    }
}

/// Compiles an Apache `LogFormat` string (`%h %l %u %t "%r" %>s %b …`).
pub fn compile_apache(log_format: &str) -> Result<Format, String> {
    let b = log_format.trim().as_bytes();
    let mut steps: Vec<Step> = Vec::new();
    let mut lit: Vec<u8> = Vec::new();
    let mut used: Vec<String> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        // a directive inside quotes reads as one quoted field
        let quoted = b[i] == b'"' && b.get(i + 1) == Some(&b'%');
        let start = if quoted { i + 1 } else { i };
        if b[start] != b'%' {
            lit.push(b[i]);
            i += 1;
            continue;
        }
        if b.get(start + 1) == Some(&b'%') {
            lit.push(b'%');
            i = start + 2;
            continue;
        }
        // %[<>][{arg}]X
        let mut j = start + 1;
        while j < b.len() && matches!(b[j], b'<' | b'>') {
            j += 1;
        }
        let mut arg = None;
        if b.get(j) == Some(&b'{') {
            let close = b[j..]
                .iter()
                .position(|&c| c == b'}')
                .ok_or("a %{ is not closed")?
                + j;
            arg = Some(String::from_utf8_lossy(&b[j + 1..close]).into_owned());
            j = close + 1;
        }
        let Some(&d) = b.get(j) else {
            return Err("a format ends with %".into());
        };
        j += 1;
        let header = |a: &Option<String>, prefix: &str| {
            let a = a
                .clone()
                .unwrap_or_default()
                .to_ascii_lowercase()
                .replace('-', "_");
            format!("{prefix}{a}")
        };
        let (name, natural) = match d {
            b'h' => ("client".to_string(), Until::Chain),
            b'a' => ("client_ip".to_string(), Until::Space),
            b'l' => ("ident".to_string(), Until::Space),
            b'u' => ("user".to_string(), Until::Space),
            b't' => ("time".to_string(), Until::Bracketed),
            b'r' => ("request".to_string(), Until::Space),
            b's' => ("status".to_string(), Until::Space),
            b'b' | b'B' | b'O' => ("bytes".to_string(), Until::Space),
            b'I' => ("bytes_in".to_string(), Until::Space),
            b'D' => ("duration_us".to_string(), Until::Space),
            b'T' => ("duration_s".to_string(), Until::Space),
            b'v' => ("vhost".to_string(), Until::Space),
            b'V' => ("server_name".to_string(), Until::Space),
            b'p' => ("port".to_string(), Until::Space),
            b'm' => ("method".to_string(), Until::Space),
            b'U' => ("path".to_string(), Until::Space),
            b'q' => ("query".to_string(), Until::Space),
            b'H' => ("protocol".to_string(), Until::Space),
            b'i' => (header(&arg, ""), Until::Space),
            b'o' => (header(&arg, "resp_"), Until::Space),
            b'e' => (header(&arg, "env_"), Until::Space),
            other => {
                return Err(format!(
                    "%{} is not an Apache LogFormat directive LogSeek knows",
                    other as char
                ))
            }
        };
        let mut name = name;
        let mut k = 2;
        while used.contains(&name) {
            name = format!(
                "{}_{k}",
                name.trim_end_matches(char::is_numeric)
                    .trim_end_matches('_')
            );
            k += 1;
        }
        used.push(name.clone());
        if !lit.is_empty() {
            steps.push(Step::Lit(std::mem::take(&mut lit)));
        }
        let until = if quoted {
            if b.get(j) != Some(&b'"') {
                return Err("a quoted directive must close its quote, like \"%r\"".into());
            }
            j += 1;
            Until::Quoted
        } else if natural == Until::Space {
            // a following literal other than a space bounds the field (e.g. %v:%p)
            match b.get(j) {
                Some(&c) if c != b' ' && c != b'%' && c != b'"' => Until::Char(c),
                _ => Until::Space,
            }
        } else {
            natural
        };
        steps.push(field(&name, until));
        i = j;
    }
    if !lit.is_empty() {
        steps.push(Step::Lit(lit));
    }
    if !steps.iter().any(|s| matches!(s, Step::Field { .. })) {
        return Err("the format has no %directives".into());
    }
    Ok(Format {
        name: "Apache LogFormat".into(),
        steps,
        kind: Kind::Line,
    })
}

/// A built-in format by name.
pub fn builtin(name: &str) -> Option<Format> {
    let (src, label) = match name {
        "apache_common" => (r#"%h %l %u %t "%r" %>s %b"#, "Apache common"),
        "apache_combined" => (
            r#"%h %l %u %t "%r" %>s %b "%{Referer}i" "%{User-Agent}i""#,
            "Apache combined",
        ),
        "apache_vhost_combined" => (
            r#"%v:%p %h %l %u %t "%r" %>s %O "%{Referer}i" "%{User-Agent}i""#,
            "Apache vhost combined",
        ),
        "nginx_combined" => {
            let mut f = compile_nginx(
                r#"$remote_addr - $remote_user [$time_local] "$request" $status $body_bytes_sent "$http_referer" "$http_user_agent""#,
            )
            .ok()?;
            f.name = "nginx combined".into();
            return Some(f);
        }
        "kv" => {
            return Some(Format {
                name: "key=value pairs".into(),
                steps: Vec::new(),
                kind: Kind::KeyValue,
            })
        }
        "syslog_3164" => {
            return Some(Format {
                name: "syslog (BSD, RFC 3164)".into(),
                steps: Vec::new(),
                kind: Kind::Syslog3164,
            })
        }
        "syslog_5424" => {
            return Some(Format {
                name: "syslog (RFC 5424)".into(),
                steps: Vec::new(),
                kind: Kind::Syslog5424,
            })
        }
        _ => return None,
    };
    let mut f = compile_apache(src).ok()?;
    f.name = label.to_string();
    Some(f)
}
