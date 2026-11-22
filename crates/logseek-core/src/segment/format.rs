//! Format-preserving rendering for typed columns.
//!
//! Typed values (epoch µs, f64, bool) lose the source's spelling. Instead of
//! storing the text again, a column remembers *how* it was written — a time
//! layout template, per-row decimal places, the bool casing — and renders the
//! value back byte-exact. Rows that don't fit keep their original text as
//! exceptions, so display and free-text search always see the source.

use crate::time::DateOrder;
use crate::types::civil_from_days;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Year,
    Month,
    Day,
    Hour,
    Hour12,
    Minute,
    Second,
    Frac,
}

/// A timestamp layout learned from a sample value, e.g. `99/99/9999 99:99:99`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeTemplate {
    /// Source text with digit runs as `9`, AM/PM as `@@`, everything else literal.
    pub mask: String,
    /// Role of each digit run, in order.
    pub roles: Vec<Role>,
    /// Fixed UTC offset written in the source (seconds east), 0 if none.
    pub offset_s: i32,
    pub upper_ampm: bool,
}

fn mask_of(raw: &str, tz_from: usize) -> String {
    let b = raw.as_bytes();
    let mut m = String::with_capacity(raw.len());
    let mut i = 0;
    while i < b.len() {
        if i >= tz_from {
            // `tz_from` is the position of an ASCII sign, so a character boundary
            m.push_str(raw.get(i..).unwrap_or(""));
            break;
        }
        let c = b[i];
        if c.is_ascii_digit() {
            m.push('9');
            i += 1;
        } else if i + 1 < b.len()
            && (b[i..i + 2].eq_ignore_ascii_case(b"am") || b[i..i + 2].eq_ignore_ascii_case(b"pm"))
        {
            m.push_str("@@");
            i += 2;
        } else if c.is_ascii() {
            m.push(c as char);
            i += 1;
        } else {
            // a multi-byte character is copied whole, never split
            let ch = raw.get(i..).and_then(|t| t.chars().next());
            match ch {
                Some(ch) => {
                    m.push(ch);
                    i += ch.len_utf8();
                }
                None => i += 1,
            }
        }
    }
    m
}

/// Where a trailing numeric UTC offset (`+05:30`, `-0100`) starts, if any.
fn tz_start(raw: &str) -> Option<usize> {
    let b = raw.as_bytes();
    // the offset sign comes after the time part (after at least one ':')
    let first_colon = raw.find(':')?;
    let pos = raw[first_colon..]
        .rfind(['+', '-'])
        .map(|p| p + first_colon)?;
    let tail = &b[pos + 1..];
    (!tail.is_empty() && tail.iter().all(|c| c.is_ascii_digit() || *c == b':')).then_some(pos)
}

fn parse_offset(s: &str) -> Option<i32> {
    let neg = s.starts_with('-');
    let digits: String = s[1..].chars().filter(|c| c.is_ascii_digit()).collect();
    let (h, m) = match digits.len() {
        2 => (digits.parse::<i32>().ok()?, 0),
        4 => (
            digits[..2].parse::<i32>().ok()?,
            digits[2..].parse::<i32>().ok()?,
        ),
        _ => return None,
    };
    let v = h * 3600 + m * 60;
    Some(if neg { -v } else { v })
}

impl TimeTemplate {
    /// Learn a template from one value that parsed as a time.
    pub fn infer(raw: &str, order: DateOrder) -> Option<TimeTemplate> {
        let tz = tz_start(raw);
        let offset_s = match tz {
            Some(p) => parse_offset(&raw[p..])?,
            // a trailing zone abbreviation (`… MDT`) stays literal in the mask
            // but still shifts the clock: render must print local time
            None => raw
                .rsplit(' ')
                .next()
                .filter(|w| w.len() >= 2 && w.bytes().all(|b| b.is_ascii_alphabetic()))
                .and_then(crate::time::zone_abbrev)
                .unwrap_or(0) as i32,
        };
        let mask = mask_of(raw, tz.unwrap_or(usize::MAX));
        let body = mask
            .get(..tz.unwrap_or(mask.len()).min(mask.len()))
            .unwrap_or(&mask);
        // digit runs (with the char before each) in the masked body
        let mut runs: Vec<(usize, char)> = Vec::new();
        let mut prev = ' ';
        let mut cur = 0usize;
        for ch in body.chars() {
            if ch == '9' {
                cur += 1;
            } else {
                if cur > 0 {
                    runs.push((cur, prev));
                    cur = 0;
                }
                prev = ch;
            }
        }
        if cur > 0 {
            runs.push((cur, prev));
        }
        if runs.len() < 3 {
            return None;
        }
        let mut roles = Vec::with_capacity(runs.len());
        if runs[0].0 == 4 {
            roles.extend([Role::Year, Role::Month, Role::Day]);
        } else {
            if runs[2].0 != 4 {
                return None;
            }
            match order {
                DateOrder::Dmy => roles.extend([Role::Day, Role::Month, Role::Year]),
                DateOrder::Mdy => roles.extend([Role::Month, Role::Day, Role::Year]),
            }
        }
        let ampm = mask.contains("@@");
        let mut time_roles = vec![
            if ampm { Role::Hour12 } else { Role::Hour },
            Role::Minute,
            Role::Second,
        ];
        let mut k = 0;
        for (w, before) in runs.iter().skip(3) {
            if (*before == '.' || *before == ',') && k >= 2 {
                if *w > 6 {
                    return None; // sub-microsecond digits can't be reproduced
                }
                roles.push(Role::Frac);
            } else if k < time_roles.len() {
                roles.push(time_roles[k]);
                k += 1;
            } else {
                return None;
            }
        }
        time_roles.clear();
        let upper_ampm = raw.contains("AM") || raw.contains("PM");
        let t = TimeTemplate {
            mask,
            roles,
            offset_s,
            upper_ampm,
        };
        // A template must prove itself: re-render its own example byte for byte.
        // Anything it cannot reproduce is kept verbatim instead (never-miss rule).
        let us = crate::time::parse_time_with(raw, order)?;
        (t.render(us) == raw).then_some(t)
    }

    /// Would rendering reproduce `raw` exactly?
    pub fn matches(&self, raw: &str) -> bool {
        let tz = tz_start(raw);
        mask_of(raw, tz.unwrap_or(usize::MAX)) == self.mask
    }

    pub fn render(&self, us: i64) -> String {
        let local = us + self.offset_s as i64 * 1_000_000;
        let secs = local.div_euclid(1_000_000);
        let frac = local.rem_euclid(1_000_000);
        let (y, mo, d) = civil_from_days(secs.div_euclid(86_400));
        let sod = secs.rem_euclid(86_400);
        let (h, mi, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);
        let mut out = String::with_capacity(self.mask.len());
        let mut role = 0usize;
        let b = self.mask.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'9' {
                let mut w = 0;
                while i + w < b.len() && b[i + w] == b'9' {
                    w += 1;
                }
                let v: i64 = match self.roles.get(role) {
                    Some(Role::Year) => y,
                    Some(Role::Month) => mo as i64,
                    Some(Role::Day) => d as i64,
                    Some(Role::Hour) => h,
                    Some(Role::Hour12) => match h % 12 {
                        0 => 12,
                        x => x,
                    },
                    Some(Role::Minute) => mi,
                    Some(Role::Second) => s,
                    Some(Role::Frac) => frac / 10i64.pow(6 - w as u32),
                    None => 0,
                };
                out.push_str(&format!("{v:0w$}"));
                role += 1;
                i += w;
            } else if b[i] == b'@' && b.get(i + 1) == Some(&b'@') {
                let pm = h >= 12;
                out.push_str(match (pm, self.upper_ampm) {
                    (false, true) => "AM",
                    (true, true) => "PM",
                    (false, false) => "am",
                    (true, false) => "pm",
                });
                i += 2;
            } else {
                // literal (possibly multi-byte) char
                let ch = self.mask[i..].chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
            }
        }
        out
    }
}

/// Decimal places of a canonically written decimal (`-?(0|[1-9]\d*)(\.\d+)?`,
/// ≤ 18 significant digits — fits an i64 mantissa), or `None` to keep it verbatim.
pub fn canonical_decimals(raw: &str) -> Option<u8> {
    let b = raw.as_bytes();
    let body = if b.first() == Some(&b'-') { &b[1..] } else { b };
    if body.is_empty() {
        return None;
    }
    let (int, frac) = match body.iter().position(|&c| c == b'.') {
        Some(p) => (&body[..p], Some(&body[p + 1..])),
        None => (body, None),
    };
    if int.is_empty() || !int.iter().all(u8::is_ascii_digit) || (int.len() > 1 && int[0] == b'0') {
        return None;
    }
    let k = match frac {
        Some(f) if !f.is_empty() && f.iter().all(u8::is_ascii_digit) => f.len(),
        Some(_) => return None,
        None => 0,
    };
    let sig = int.iter().skip_while(|&&c| c == b'0').count() + k;
    (sig <= 18 && int.len() + k <= 18 && k < 255).then_some(k as u8)
}

pub fn render_decimals(v: f64, k: u8) -> String {
    format!("{v:.*}", k as usize)
}

/// The decimal written without its point: `-7.50` → -750. `None` for `-0…`
/// (the sign of zero can't be kept in an integer) or anything non-canonical.
pub fn mantissa(raw: &str) -> Option<i64> {
    let neg = raw.starts_with('-');
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    let m: i64 = digits.parse().ok()?;
    if neg && m == 0 {
        return None;
    }
    Some(if neg { -m } else { m })
}

/// Exact text of `m / 10^k`, e.g. (750, 2) → "7.50", (-25, 2) → "-0.25".
pub fn render_mk(m: i64, k: u8) -> String {
    if k == 0 {
        return m.to_string();
    }
    let k = k as usize;
    let mut d = m.unsigned_abs().to_string();
    if d.len() <= k {
        d = format!("{}{}", "0".repeat(k + 1 - d.len()), d);
    }
    let (a, b) = d.split_at(d.len() - k);
    format!("{}{a}.{b}", if m < 0 { "-" } else { "" })
}

/// Exact (correctly rounded) value of `m / 10^k`.
pub fn mk_value(m: i64, k: u8) -> f64 {
    const P: [f64; 23] = [
        1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
        1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
    ];
    // one IEEE division of two exactly representable numbers is correctly rounded
    if m.unsigned_abs() < (1u64 << 53) && (k as usize) < P.len() {
        return m as f64 / P[k as usize];
    }
    render_mk(m, k).parse().unwrap_or(f64::NAN)
}

#[cfg(test)]
mod mk_tests {
    use super::*;
    #[test]
    fn mantissa_roundtrip() {
        for raw in [
            "7.50",
            "0.0",
            "-0.25",
            "123456.789",
            "-12",
            "0",
            "100",
            "0.001",
            "0.0217391304347826",
            "-12345.678901234567",
        ] {
            let k = canonical_decimals(raw).unwrap();
            let m = mantissa(raw).unwrap();
            assert_eq!(render_mk(m, k), raw);
            assert_eq!(mk_value(m, k), raw.parse::<f64>().unwrap(), "{raw}");
        }
        assert_eq!(mantissa("-0.0"), None);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoolCase {
    Lower,
    Capital,
    Upper,
}

impl BoolCase {
    pub fn of(raw: &str) -> Option<BoolCase> {
        match raw {
            "true" | "false" => Some(BoolCase::Lower),
            "True" | "False" => Some(BoolCase::Capital),
            "TRUE" | "FALSE" => Some(BoolCase::Upper),
            _ => None,
        }
    }
    pub fn render(self, b: bool) -> &'static str {
        match (self, b) {
            (BoolCase::Lower, true) => "true",
            (BoolCase::Lower, false) => "false",
            (BoolCase::Capital, true) => "True",
            (BoolCase::Capital, false) => "False",
            (BoolCase::Upper, true) => "TRUE",
            (BoolCase::Upper, false) => "FALSE",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::{parse_time_with, DateOrder};

    fn roundtrip(raw: &str, order: DateOrder) {
        let t = TimeTemplate::infer(raw, order).unwrap_or_else(|| panic!("no template for {raw}"));
        let us = parse_time_with(raw, order).unwrap();
        assert!(t.matches(raw));
        assert_eq!(t.render(us), raw);
    }

    #[test]
    fn time_templates_roundtrip() {
        roundtrip("20/02/2018 08:31:01", DateOrder::Dmy);
        roundtrip("02/20/2018 08:31:01", DateOrder::Mdy);
        roundtrip("2026-11-01T00:10:00Z", DateOrder::Mdy);
        roundtrip("2026-11-01 00:10:00.123", DateOrder::Mdy);
        roundtrip("2026-11-01T05:40:00+05:30", DateOrder::Mdy);
        roundtrip("2026-11-01T05:40:00-0100", DateOrder::Mdy);
        roundtrip("11/01/2026 12:10:00 AM", DateOrder::Mdy);
        roundtrip("11/01/2026 01:10:00 PM", DateOrder::Mdy);
        roundtrip("2026-11-01 00:10:00,250", DateOrder::Mdy);
        roundtrip("2026/11/01 23:59:59", DateOrder::Mdy);
    }

    #[test]
    fn mismatching_layout_is_detected() {
        let t = TimeTemplate::infer("20/02/2018 08:31:01", DateOrder::Dmy).unwrap();
        assert!(!t.matches("2/3/2018 8:31:01"));
        assert!(!t.matches("2018-02-20 08:31:01"));
        let z = TimeTemplate::infer("2026-11-01T05:40:00+05:30", DateOrder::Mdy).unwrap();
        assert!(!z.matches("2026-11-01T05:40:00+01:00"));
    }

    #[test]
    fn decimals() {
        assert_eq!(canonical_decimals("7.50"), Some(2));
        assert_eq!(canonical_decimals("0.0"), Some(1));
        assert_eq!(canonical_decimals("-12"), Some(0));
        assert_eq!(canonical_decimals("007.5"), None);
        assert_eq!(canonical_decimals("1e5"), None);
        assert_eq!(canonical_decimals("0.0217391304347826"), Some(16));
        assert_eq!(canonical_decimals("1234567890123456789.5"), None);
        for raw in ["7.50", "0.0", "-0.25", "123456.789", "-12"] {
            let k = canonical_decimals(raw).unwrap();
            assert_eq!(render_decimals(raw.parse().unwrap(), k), raw);
        }
    }
}
