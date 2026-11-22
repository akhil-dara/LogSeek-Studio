//! Cheap format recognisers used for the chips on the Check before opening screen and type suggestions.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Hint {
    Timestamp,
    EpochSeconds,
    EpochMillis,
    EpochMicros,
    Ipv4,
    Ipv6,
    Guid,
    Hash,
    Url,
    Path,
    Email,
    Base64,
}

impl Hint {
    pub fn label(self) -> &'static str {
        match self {
            Hint::Timestamp => "timestamp",
            Hint::EpochSeconds => "epoch s",
            Hint::EpochMillis => "epoch ms",
            Hint::EpochMicros => "epoch µs",
            Hint::Ipv4 => "IPv4",
            Hint::Ipv6 => "IPv6",
            Hint::Guid => "GUID",
            Hint::Hash => "hash",
            Hint::Url => "URL",
            Hint::Path => "path",
            Hint::Email => "email",
            Hint::Base64 => "base64",
        }
    }
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_guid(s: &str) -> bool {
    let s = s
        .strip_prefix('{')
        .and_then(|x| x.strip_suffix('}'))
        .unwrap_or(s);
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, &c)| match i {
            8 | 13 | 18 | 23 => c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// 1990-01-01 .. 2100-01-01 in seconds.
fn plausible_epoch_secs(v: i64) -> bool {
    (631_152_000..4_102_444_800).contains(&v)
}

fn looks_date(s: &str) -> bool {
    let b = s.as_bytes();
    let d = |i: usize| b.get(i).is_some_and(|c| c.is_ascii_digit());
    (b.len() >= 10
        && d(0)
        && d(1)
        && d(2)
        && d(3)
        && b[4] == b'-'
        && d(5)
        && d(6)
        && b[7] == b'-'
        && d(8)
        && d(9))
        || (b.len() >= 10
            && d(0)
            && d(1)
            && b[2] == b'/'
            && d(3)
            && d(4)
            && b[5] == b'/'
            && d(6)
            && d(7)
            && d(8)
            && d(9))
}

fn is_base64(s: &str) -> bool {
    s.len() >= 16
        && s.len() % 4 == 0
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
        && s.bytes().any(|b| b.is_ascii_uppercase())
        && s.bytes().any(|b| b.is_ascii_lowercase())
        && (s.bytes().any(|b| b.is_ascii_digit()) || s.ends_with('='))
        && !s.trim_end_matches('=').contains('=')
}

fn is_email(s: &str) -> bool {
    let mut it = s.splitn(2, '@');
    match (it.next(), it.next()) {
        (Some(u), Some(d)) => {
            !u.is_empty()
                && d.contains('.')
                && !d.starts_with('.')
                && !d.ends_with('.')
                && !s.contains(' ')
                && !d.contains('@')
        }
        _ => false,
    }
}

/// All formats `s` looks like (usually zero or one).
pub fn detect_hints(s: &str) -> Vec<Hint> {
    let s = s.trim();
    let mut out = Vec::new();
    if s.is_empty() || s.len() > 4096 {
        return out;
    }
    if s.bytes().all(|b| b.is_ascii_digit()) {
        if let Ok(v) = s.parse::<i64>() {
            match s.len() {
                10 if plausible_epoch_secs(v) => out.push(Hint::EpochSeconds),
                13 if plausible_epoch_secs(v / 1_000) => out.push(Hint::EpochMillis),
                16 if plausible_epoch_secs(v / 1_000_000) => out.push(Hint::EpochMicros),
                _ => {}
            }
        }
        return out;
    }
    if s.parse::<std::net::Ipv4Addr>().is_ok() {
        out.push(Hint::Ipv4);
    } else if s.contains(':') && s.parse::<std::net::Ipv6Addr>().is_ok() {
        out.push(Hint::Ipv6);
    } else if is_guid(s) {
        out.push(Hint::Guid);
    } else if matches!(s.len(), 32 | 40 | 64 | 128) && is_hex(s) {
        out.push(Hint::Hash);
    } else if s.starts_with("http://") || s.starts_with("https://") || s.starts_with("ftp://") {
        out.push(Hint::Url);
    } else if s.starts_with('\\')
        || (s.len() > 2 && s.as_bytes()[1] == b':' && s.as_bytes()[2] == b'\\')
        || (s.starts_with('/') && s[1..].contains('/'))
    {
        out.push(Hint::Path);
    } else if looks_date(s) {
        out.push(Hint::Timestamp);
    } else if is_email(s) {
        out.push(Hint::Email);
    } else if is_base64(s) {
        out.push(Hint::Base64);
    }
    out
}
