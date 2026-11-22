//! The formula function library.

use super::{eval, FValue, Node};
use crate::query::parse_span;
use crate::time::parse_time;
use crate::types::{civil_from_days, ColType};

pub struct FnSpec {
    pub name: &'static str,
    pub min: usize,
    pub max: Option<usize>,
    pub regex_arg: bool,
    pub help: &'static str,
}

impl FnSpec {
    pub fn arity_text(&self) -> String {
        match self.max {
            Some(m) if m == self.min => format!("{} argument{}", m, if m == 1 { "" } else { "s" }),
            Some(m) => format!("{} to {} arguments", self.min, m),
            None => format!(
                "at least {} argument{}",
                self.min,
                if self.min == 1 { "" } else { "s" }
            ),
        }
    }
}

const fn f(name: &'static str, min: usize, max: Option<usize>, help: &'static str) -> FnSpec {
    FnSpec {
        name,
        min,
        max,
        regex_arg: false,
        help,
    }
}
const fn r(name: &'static str, min: usize, max: Option<usize>, help: &'static str) -> FnSpec {
    FnSpec {
        name,
        min,
        max,
        regex_arg: true,
        help,
    }
}

pub static FUNCTIONS: &[FnSpec] = &[
    f("lower", 1, Some(1), "lower(text)"),
    f("upper", 1, Some(1), "upper(text)"),
    f("trim", 1, Some(1), "trim(text)"),
    f("len", 1, Some(1), "len(text) — characters"),
    f("left", 2, Some(2), "left(text, n)"),
    f("right", 2, Some(2), "right(text, n)"),
    f(
        "substr",
        2,
        Some(3),
        "substr(text, start, [length]) — start at 0",
    ),
    f("replace", 3, Some(3), "replace(text, find, with)"),
    f(
        "contains",
        2,
        Some(2),
        "contains(text, part) — ignores case",
    ),
    f(
        "startswith",
        2,
        Some(2),
        "startswith(text, prefix) — ignores case",
    ),
    f(
        "endswith",
        2,
        Some(2),
        "endswith(text, suffix) — ignores case",
    ),
    f(
        "split",
        3,
        Some(3),
        "split(text, separator, index) — negative index counts from the end",
    ),
    r(
        "regex",
        2,
        Some(3),
        "regex(text, \"pattern\", [group]) — first match",
    ),
    r(
        "regex_replace",
        3,
        Some(3),
        "regex_replace(text, \"pattern\", with)",
    ),
    f("concat", 1, None, "concat(a, b, c)"),
    f(
        "json",
        2,
        Some(2),
        "json(text, \"path\") — a.b, a[0], a[*].b",
    ),
    f(
        "json_len",
        1,
        Some(2),
        "json_len(text, [\"path\"]) — array/object length",
    ),
    f("num", 1, Some(1), "num(x)"),
    f("int", 1, Some(1), "int(x) — truncates"),
    f("round", 1, Some(2), "round(x, [digits])"),
    f("floor", 1, Some(1), "floor(x)"),
    f("ceil", 1, Some(1), "ceil(x)"),
    f("abs", 1, Some(1), "abs(x)"),
    f("min", 1, None, "min(a, b, c)"),
    f("max", 1, None, "max(a, b, c)"),
    f(
        "date",
        1,
        Some(2),
        "date(x, [unit]) — unit: s, ms, us, ns, filetime, webkit, cocoa, days",
    ),
    f(
        "strftime",
        2,
        Some(2),
        "strftime(time, \"%Y-%m-%d %H:%M:%S\")",
    ),
    f("year", 1, Some(1), "year(time)"),
    f("month", 1, Some(1), "month(time)"),
    f("day", 1, Some(1), "day(time)"),
    f("hour", 1, Some(1), "hour(time)"),
    f("minute", 1, Some(1), "minute(time)"),
    f(
        "weekday",
        1,
        Some(1),
        "weekday(time) — 1 = Monday to 7 = Sunday",
    ),
    f("bucket", 2, Some(2), "bucket(time, \"1h\")"),
    f("cidr", 2, Some(2), "cidr(ip, \"10.0.0.0/8\")"),
    f("is_private_ip", 1, Some(1), "is_private_ip(ip)"),
    f(
        "base64_decode",
        1,
        Some(2),
        "base64_decode(text, [\"utf16\"])",
    ),
    f("hex_decode", 1, Some(1), "hex_decode(text)"),
    f("url_host", 1, Some(1), "url_host(url)"),
    f("url_path", 1, Some(1), "url_path(url)"),
    f("url_query", 1, Some(1), "url_query(url)"),
    f("path_dir", 1, Some(1), "path_dir(path)"),
    f("path_file", 1, Some(1), "path_file(path)"),
    f("path_ext", 1, Some(1), "path_ext(path)"),
    f(
        "entropy",
        1,
        Some(1),
        "entropy(text) — Shannon bits per character",
    ),
    f("if", 3, Some(3), "if(condition, then, else)"),
    f(
        "case",
        3,
        None,
        "case(cond1, value1, cond2, value2, default)",
    ),
    f("coalesce", 1, None, "coalesce(a, b, c) — first non-null"),
    f(
        "isnull",
        1,
        Some(1),
        "isnull(x) — missing (an error is not missing)",
    ),
    f(
        "iserror",
        1,
        Some(1),
        "iserror(x) — could not be worked out",
    ),
    f(
        "iferror",
        2,
        Some(2),
        "iferror(x, instead) — instead only where x is an error",
    ),
];

/// Static result type of a function (None = depends on its arguments).
pub fn ret(name: &str) -> Option<ColType> {
    Some(match name {
        "len" | "json_len" | "num" | "round" | "floor" | "ceil" | "abs" | "min" | "max"
        | "year" | "month" | "day" | "hour" | "minute" | "weekday" | "entropy" => ColType::Float,
        "int" => ColType::Int,
        "date" | "bucket" => ColType::Time,
        "contains" | "startswith" | "endswith" | "cidr" | "is_private_ip" | "isnull"
        | "iserror" => ColType::Bool,
        "if" | "case" | "coalesce" | "iferror" => return None,
        _ => ColType::Text,
    })
}

fn txt(v: FValue) -> Option<String> {
    match v {
        // moved out, not copied
        FValue::Text(s) => Some(s),
        v => v.text(),
    }
}

/// A number from `v`: text that is not one is an error saying so; a missing
/// value stays missing.
fn number(v: FValue, f: impl Fn(f64) -> f64) -> FValue {
    match v.num() {
        Some(x) => FValue::Num(f(x)),
        None => match v {
            FValue::Text(s) if !s.trim().is_empty() => FValue::Error(format!("not a number: {s}")),
            _ => FValue::Null,
        },
    }
}

fn chars_take(s: &str, start: usize, len: Option<usize>) -> String {
    let it = s.chars().skip(start);
    match len {
        Some(n) => it.take(n).collect(),
        None => it.collect(),
    }
}

fn time_of(v: &FValue) -> Option<i64> {
    match v {
        FValue::Time(t) => Some(*t),
        FValue::Text(s) => parse_time(s),
        FValue::Num(n) => parse_time(&format!("{}", *n as i64)),
        _ => None,
    }
}

fn parts(t: i64) -> (i64, u32, u32, i64, i64, i64, i64) {
    let secs = t.div_euclid(1_000_000);
    let days = secs.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let sod = secs.rem_euclid(86_400);
    (y, m, d, sod / 3600, (sod % 3600) / 60, sod % 60, days)
}

fn strftime(t: i64, fmt: &str) -> String {
    let (y, mo, d, h, mi, s, _) = parts(t);
    let us = t.rem_euclid(1_000_000);
    let mut o = String::new();
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        if c != '%' {
            o.push(c);
            continue;
        }
        match it.next() {
            Some('Y') => o.push_str(&format!("{y:04}")),
            Some('m') => o.push_str(&format!("{mo:02}")),
            Some('d') => o.push_str(&format!("{d:02}")),
            Some('H') => o.push_str(&format!("{h:02}")),
            Some('M') => o.push_str(&format!("{mi:02}")),
            Some('S') => o.push_str(&format!("{s:02}")),
            Some('f') => o.push_str(&format!("{us:06}")),
            Some('%') => o.push('%'),
            Some(x) => {
                o.push('%');
                o.push(x);
            }
            None => o.push('%'),
        }
    }
    o
}

fn date_with_unit(v: &FValue, unit: &str) -> Option<i64> {
    number_as_time(v.num()?, unit)
}

/// A number read as a time (UTC µs) in one of the ways times are written as
/// numbers: `s`, `ms`, `us`, `ns` since 1970; `filetime` (100 ns since 1601);
/// `webkit` (µs since 1601); `cocoa` (seconds since 2001); `days` (a
/// spreadsheet day number). `None` for an unknown kind or a time out of range.
pub fn number_as_time(n: f64, unit: &str) -> Option<i64> {
    if unit == "days" {
        // a spreadsheet day number: days since 1899-12-30, the fraction is
        // the time of day (25569 is 1970-01-01); rounded to the millisecond,
        // the finest a day number holds
        let ms = ((n - 25_569.0) * 86_400_000.0).round();
        return (ms.abs() < 9.0e15).then(|| ms as i64 * 1_000);
    }
    let n = n as i128;
    let us: i128 = match unit {
        "s" => n * 1_000_000,
        "ms" => n * 1_000,
        "us" | "µs" => n,
        "ns" => n / 1_000,
        "filetime" => n / 10 - 11_644_473_600_000_000,
        "webkit" => n - 11_644_473_600_000_000,
        "cocoa" | "mac" => (n + 978_307_200) * 1_000_000,
        _ => return None,
    };
    i64::try_from(us).ok()
}

fn ipv4(s: &str) -> Option<u32> {
    s.trim().parse::<std::net::Ipv4Addr>().ok().map(u32::from)
}

fn cidr(ip: &str, net: &str) -> Option<bool> {
    let (n, bits) = net.split_once('/').unwrap_or((net, "32"));
    let bits: u32 = bits.parse().ok()?;
    if let (Some(a), Some(b)) = (ipv4(ip), ipv4(n)) {
        let mask = if bits == 0 {
            0
        } else {
            u32::MAX << (32 - bits.min(32))
        };
        return Some(a & mask == b & mask);
    }
    let a: std::net::Ipv6Addr = ip.trim().parse().ok()?;
    let b: std::net::Ipv6Addr = n.parse().ok()?;
    let (a, b) = (u128::from(a), u128::from(b));
    let mask = if bits == 0 {
        0
    } else {
        u128::MAX << (128 - bits.min(128))
    };
    Some(a & mask == b & mask)
}

fn private_ip(s: &str) -> bool {
    if let Ok(ip) = s.trim().parse::<std::net::Ipv4Addr>() {
        return ip.is_private() || ip.is_loopback() || ip.is_link_local();
    }
    if let Ok(ip) = s.trim().parse::<std::net::Ipv6Addr>() {
        let seg = ip.segments()[0];
        return ip.is_loopback() || (seg & 0xfe00) == 0xfc00 || (seg & 0xffc0) == 0xfe80;
    }
    false
}

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut nb) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        nb += 6;
        if nb >= 8 {
            nb -= 8;
            out.push((acc >> nb) as u8);
        }
    }
    Some(out)
}

fn utf16le_lossy(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

fn url_split(u: &str) -> (String, String, String) {
    let rest = u.split_once("://").map_or(u, |(_, r)| r);
    let (hostport, after) = match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let host = hostport.rsplit_once('@').map_or(hostport, |(_, h)| h);
    let host = if host.starts_with('[') {
        host.split(']').next().unwrap_or("").trim_start_matches('[')
    } else {
        host.split(':').next().unwrap_or("")
    };
    let (path, query) = match after.split_once('?') {
        Some((p, q)) => (p, q.split('#').next().unwrap_or("")),
        None => (after.split('#').next().unwrap_or(""), ""),
    };
    (host.to_string(), path.to_string(), query.to_string())
}

fn path_split(p: &str) -> (&str, &str) {
    match p.rfind(['\\', '/']) {
        Some(i) => (&p[..i], &p[i + 1..]),
        None => ("", p),
    }
}

fn entropy(s: &str) -> f64 {
    let mut counts = std::collections::HashMap::new();
    let mut n = 0.0;
    for c in s.chars() {
        *counts.entry(c).or_insert(0.0) += 1.0;
        n += 1.0;
    }
    if n == 0.0 {
        return 0.0;
    }
    -counts
        .values()
        .map(|&c: &f64| (c / n) * (c / n).log2())
        .sum::<f64>()
}

pub fn call(
    name: &str,
    args: &[Node],
    re: Option<&regex::Regex>,
    get: &dyn Fn(usize) -> FValue,
) -> FValue {
    // these look at their arguments one by one (an error may be what they test
    // for, or a branch not taken); every other function gets each argument
    // worked out once, and an error in any of them is its answer, reason and all
    let lazy = matches!(
        name,
        "if" | "case" | "coalesce" | "isnull" | "iserror" | "iferror"
    );
    // worked out once, held on the stack for up to four arguments, and moved
    // (not copied) to the function when it reads them
    let mut small: [Option<FValue>; 4] = [None, None, None, None];
    let mut big: Vec<Option<FValue>> = Vec::new();
    if !lazy {
        let slots: &mut [Option<FValue>] = if args.len() <= small.len() {
            &mut small[..args.len()]
        } else {
            big = (0..args.len()).map(|_| None).collect();
            &mut big[..]
        };
        for (s, n) in slots.iter_mut().zip(args) {
            let v = eval(n, get);
            if let FValue::Error(_) = v {
                return v;
            }
            *s = Some(v);
        }
    }
    let held = std::cell::RefCell::new(if args.len() <= small.len() {
        &mut small[..]
    } else {
        &mut big[..]
    });
    let a = |i: usize| {
        if !lazy {
            let v = held.borrow_mut().get_mut(i).and_then(Option::take);
            if let Some(v) = v {
                return v;
            }
        }
        // a lazy function, or an argument read a second time: worked out again
        args.get(i).map_or(FValue::Null, |n| eval(n, get))
    };
    let t = |i: usize| txt(a(i));
    let n = |i: usize| a(i).num();
    let tf = |s: Option<String>| s.map_or(FValue::Null, FValue::Text);
    let nf = |x: Option<f64>| x.map_or(FValue::Null, FValue::Num);
    let bf = FValue::Bool;
    match name {
        // the text itself when nothing changes: no new copy
        "lower" => tf(t(0).map(|s| {
            if s.is_ascii() && !s.bytes().any(|b| b.is_ascii_uppercase()) {
                s
            } else {
                s.to_lowercase()
            }
        })),
        "upper" => tf(t(0).map(|s| {
            if s.is_ascii() && !s.bytes().any(|b| b.is_ascii_lowercase()) {
                s
            } else {
                s.to_uppercase()
            }
        })),
        "trim" => tf(t(0).map(|s| {
            let t = s.trim();
            if t.len() == s.len() {
                s
            } else {
                t.to_string()
            }
        })),
        "len" => nf(t(0).map(|s| s.chars().count() as f64)),
        "left" => tf(t(0)
            .zip(n(1))
            .map(|(s, k)| chars_take(&s, 0, Some(k.max(0.0) as usize)))),
        "right" => tf(t(0).zip(n(1)).map(|(s, k)| {
            let c = s.chars().count();
            chars_take(&s, c.saturating_sub(k.max(0.0) as usize), None)
        })),
        "substr" => tf(t(0).zip(n(1)).map(|(s, st)| {
            chars_take(&s, st.max(0.0) as usize, n(2).map(|l| l.max(0.0) as usize))
        })),
        "replace" => match (t(0), t(1), t(2)) {
            (Some(s), Some(x), Some(y)) if !x.is_empty() => FValue::Text(s.replace(&x, &y)),
            (Some(s), _, _) => FValue::Text(s),
            _ => FValue::Null,
        },
        "contains" => bf(t(0)
            .zip(t(1))
            .is_some_and(|(s, x)| s.to_lowercase().contains(&x.to_lowercase()))),
        "startswith" => bf(t(0)
            .zip(t(1))
            .is_some_and(|(s, x)| s.to_lowercase().starts_with(&x.to_lowercase()))),
        "endswith" => bf(t(0)
            .zip(t(1))
            .is_some_and(|(s, x)| s.to_lowercase().ends_with(&x.to_lowercase()))),
        "split" => match (t(0), t(1), n(2)) {
            (Some(s), Some(sep), Some(i)) if !sep.is_empty() => {
                let p: Vec<&str> = s.split(sep.as_str()).collect();
                let i = i as i64;
                let idx = if i < 0 { p.len() as i64 + i } else { i };
                tf(p.get(idx.max(0) as usize)
                    .filter(|_| idx >= 0)
                    .map(|x| x.to_string()))
            }
            _ => FValue::Null,
        },
        "regex" => {
            let (Some(s), Some(re)) = (t(0), re) else {
                return FValue::Null;
            };
            let g = n(2).unwrap_or(0.0) as usize;
            tf(re
                .captures(&s)
                .and_then(|c| c.get(g))
                .map(|m| m.as_str().to_string()))
        }
        "regex_replace" => {
            let (Some(s), Some(re), Some(w)) = (t(0), re, t(2)) else {
                return FValue::Null;
            };
            FValue::Text(re.replace_all(&s, w.as_str()).into_owned())
        }
        "concat" => FValue::Text((0..args.len()).filter_map(t).collect()),
        "json" => tf(t(0)
            .zip(t(1))
            .and_then(|(s, p)| crate::jsonpath::extract(&s, &p))),
        "json_len" => {
            let Some(s) = t(0) else { return FValue::Null };
            let path = t(1).unwrap_or_default();
            let q = if path.is_empty() { String::new() } else { path };
            let v: Option<serde_json::Value> = serde_json::from_str(&s).ok();
            let target = match (&v, q.is_empty()) {
                (Some(v), true) => Some(v.clone()),
                (Some(_), false) => {
                    crate::jsonpath::extract(&s, &q).and_then(|x| serde_json::from_str(&x).ok())
                }
                _ => None,
            };
            nf(target.and_then(|v| match v {
                serde_json::Value::Array(a) => Some(a.len() as f64),
                serde_json::Value::Object(o) => Some(o.len() as f64),
                _ => None,
            }))
        }
        "num" => number(a(0), |x| x),
        "int" => number(a(0), f64::trunc),
        "round" => nf(n(0).map(|x| {
            let d = n(1).unwrap_or(0.0) as i32;
            let p = 10f64.powi(d);
            (x * p).round() / p
        })),
        "floor" => nf(n(0).map(f64::floor)),
        "ceil" => nf(n(0).map(f64::ceil)),
        "abs" => nf(n(0).map(f64::abs)),
        "min" => nf((0..args.len()).filter_map(n).reduce(f64::min)),
        "max" => nf((0..args.len()).filter_map(n).reduce(f64::max)),
        "date" => {
            let v = a(0);
            let r = match t(1) {
                Some(u) => date_with_unit(&v, &u.to_ascii_lowercase()),
                None => time_of(&v),
            };
            match (r, &v) {
                (Some(tm), _) => FValue::Time(tm),
                (None, FValue::Text(s)) if !s.trim().is_empty() => {
                    FValue::Error(format!("not a date: {s}"))
                }
                _ => FValue::Null,
            }
        }
        "strftime" => match (time_of(&a(0)), t(1)) {
            (Some(tm), Some(fm)) => FValue::Text(strftime(tm, &fm)),
            _ => FValue::Null,
        },
        "year" | "month" | "day" | "hour" | "minute" | "weekday" => {
            let Some(tm) = time_of(&a(0)) else {
                return FValue::Null;
            };
            let (y, mo, d, h, mi, _, days) = parts(tm);
            FValue::Num(match name {
                "year" => y as f64,
                "month" => mo as f64,
                "day" => d as f64,
                "hour" => h as f64,
                "minute" => mi as f64,
                _ => ((days + 3).rem_euclid(7) + 1) as f64, // 1970-01-01 was a Thursday
            })
        }
        "bucket" => match (time_of(&a(0)), t(1).and_then(|s| parse_span(&s))) {
            (Some(tm), Some(sp)) => FValue::Time(tm.div_euclid(sp) * sp),
            _ => FValue::Null,
        },
        "cidr" => match (t(0), t(1)) {
            (Some(ip), Some(net)) => bf(cidr(&ip, &net).unwrap_or(false)),
            _ => bf(false),
        },
        "is_private_ip" => bf(t(0).is_some_and(|s| private_ip(&s))),
        "base64_decode" => {
            let Some(s) = t(0).filter(|s| !s.trim().is_empty()) else {
                return FValue::Null;
            };
            let Some(bytes) = b64_decode(s.trim()) else {
                return FValue::Error(format!("not base64: {s}"));
            };
            let utf16 = t(1).is_some_and(|e| {
                e.eq_ignore_ascii_case("utf16") || e.eq_ignore_ascii_case("utf-16")
            });
            FValue::Text(if utf16 {
                utf16le_lossy(&bytes)
            } else {
                String::from_utf8_lossy(&bytes).into_owned()
            })
        }
        "hex_decode" => {
            let Some(s) = t(0).filter(|s| !s.trim().is_empty()) else {
                return FValue::Null;
            };
            let h: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
            if h.len() < 2 {
                return FValue::Error(format!("not hex: {s}"));
            }
            let bytes: Option<Vec<u8>> = (0..h.len() / 2)
                .map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).ok())
                .collect();
            tf(bytes.map(|b| String::from_utf8_lossy(&b).into_owned()))
        }
        "url_host" => tf(t(0).map(|u| url_split(&u).0)),
        "url_path" => tf(t(0).map(|u| url_split(&u).1)),
        "url_query" => tf(t(0).map(|u| url_split(&u).2)),
        "path_dir" => tf(t(0).map(|p| path_split(&p).0.to_string())),
        "path_file" => tf(t(0).map(|p| path_split(&p).1.to_string())),
        "path_ext" => tf(t(0).map(|p| {
            let f = path_split(&p).1;
            f.rsplit_once('.')
                .map_or(String::new(), |(_, e)| e.to_string())
        })),
        "entropy" => nf(t(0).map(|s| entropy(&s))),
        "if" => {
            // a condition that could not be worked out decides nothing
            let c = a(0);
            if let FValue::Error(_) = c {
                return c;
            }
            if c.truthy() {
                a(1)
            } else {
                a(2)
            }
        }
        "case" => {
            let mut i = 0;
            while i + 1 < args.len() {
                let c = a(i);
                if let FValue::Error(_) = c {
                    return c;
                }
                if c.truthy() {
                    return a(i + 1);
                }
                i += 2;
            }
            if args.len() % 2 == 1 {
                a(args.len() - 1)
            } else {
                FValue::Null
            }
        }
        "coalesce" => (0..args.len())
            .map(a)
            .find(|v| !matches!(v, FValue::Null | FValue::Error(_)))
            .unwrap_or(FValue::Null),
        "isnull" => bf(matches!(a(0), FValue::Null)),
        "iserror" => bf(matches!(a(0), FValue::Error(_))),
        "iferror" => match a(0) {
            FValue::Error(_) => a(1),
            v => v,
        },
        _ => FValue::Null,
    }
}
