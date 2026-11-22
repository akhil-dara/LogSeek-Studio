//! Timestamp parsing to epoch microseconds (UTC). Hand-written and
//! allocation-free: ISO 8601 / RFC 3339, `YYYY/MM/DD`, `MM/DD/YYYY` and
//! `DD/MM/YYYY` (order inferred per column), AM/PM, `,fff` log4j fractions,
//! zone offsets, and epochs in s / ms / µs / ns / Windows FILETIME.

use crate::types::days_from_civil;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum DateOrder {
    #[default]
    Mdy,
    Dmy,
}

const US: i64 = 1_000_000;
const FILETIME_EPOCH_DIFF_S: i64 = 11_644_473_600;
const DOTNET_EPOCH_DIFF_S: i64 = 62_135_596_800;

fn plausible_secs(s: i64) -> bool {
    (631_152_000..4_102_444_800).contains(&s) // 1990 .. 2100
}

pub fn parse_time(s: &str) -> Option<i64> {
    parse_time_with(s, DateOrder::Mdy)
}

struct Cur<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Cur<'a> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }
    fn digits(&mut self, max: usize) -> Option<(i64, usize)> {
        let s = self.i;
        let mut v = 0i64;
        while self.i < self.b.len() && self.i - s < max && self.b[self.i].is_ascii_digit() {
            v = v * 10 + (self.b[self.i] - b'0') as i64;
            self.i += 1;
        }
        let n = self.i - s;
        if n == 0 {
            None
        } else {
            Some((v, n))
        }
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn done(&self) -> bool {
        self.i >= self.b.len()
    }
}

fn epoch(s: &str) -> Option<i64> {
    let (int, frac) = match s.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (s, None),
    };
    if int.is_empty() || !int.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if let Some(f) = frac {
        if !f.bytes().all(|c| c.is_ascii_digit()) || int.len() != 10 {
            return None;
        }
    }
    let v: i64 = int.parse().ok()?;
    let us = match int.len() {
        10 if plausible_secs(v) => {
            let mut us = v * US;
            if let Some(f) = frac {
                let mut f6 = String::from(f);
                f6.truncate(6);
                while f6.len() < 6 {
                    f6.push('0');
                }
                us += f6.parse::<i64>().ok()?;
            }
            us
        }
        13 if plausible_secs(v / 1_000) => v * 1_000,
        16 if plausible_secs(v / US) => v,
        19 if plausible_secs(v / 1_000_000_000) => v / 1_000,
        17 | 18 => {
            // Windows FILETIME (100 ns since 1601), Chrome/WebKit (µs since 1601)
            // or .NET ticks (100 ns since 0001): whichever lands in a plausible year
            if plausible_secs(v / 10_000_000 - FILETIME_EPOCH_DIFF_S) {
                (v - FILETIME_EPOCH_DIFF_S * 10_000_000) / 10
            } else if int.len() == 17 && plausible_secs(v / US - FILETIME_EPOCH_DIFF_S) {
                v - FILETIME_EPOCH_DIFF_S * US
            } else if int.len() == 18 && plausible_secs(v / 10_000_000 - DOTNET_EPOCH_DIFF_S) {
                (v - DOTNET_EPOCH_DIFF_S * 10_000_000) / 10
            } else {
                return None;
            }
        }
        _ => return None,
    };
    Some(us)
}

pub fn parse_time_with(s: &str, order: DateOrder) -> Option<i64> {
    let s = s.trim();
    if s.len() < 8 {
        return None;
    }
    // ISO 8601 basic form: 20160824T182737[.fff][Z|±hhmm]
    let b = s.as_bytes();
    if b.len() >= 15
        && b[8] == b'T'
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[9..15].iter().all(u8::is_ascii_digit)
    {
        let ext = format!(
            "{}-{}-{}T{}:{}:{}{}",
            &s[0..4],
            &s[4..6],
            &s[6..8],
            &s[9..11],
            &s[11..13],
            &s[13..15],
            s.get(15..)?
        );
        return parse_time_with(&ext, order);
    }
    if s.as_bytes()[0].is_ascii_digit() && s.bytes().all(|c| c.is_ascii_digit() || c == b'.') {
        return epoch(s);
    }
    if s.bytes().any(|b| b.is_ascii_alphabetic())
        && !s.as_bytes()[..4.min(s.len())]
            .iter()
            .all(|b| b.is_ascii_digit())
    {
        if let Some(iso) = month_name_form(s) {
            return parse_time_with(&iso, order);
        }
    }
    let mut c = Cur {
        b: s.as_bytes(),
        i: 0,
    };
    let (a, an) = c.digits(4)?;
    let sep = c.peek()?;
    if sep != b'-' && sep != b'/' && sep != b'.' {
        return None;
    }
    c.i += 1;
    let (b, _) = c.digits(2)?;
    if !c.eat(sep) {
        return None;
    }
    let (y, m, d);
    if an == 4 {
        let (dd, _) = c.digits(2)?;
        (y, m, d) = (a, b, dd);
    } else {
        let (yy, yn) = c.digits(4)?;
        if yn != 4 {
            return None;
        }
        let dmy = a > 12 || (order == DateOrder::Dmy && b <= 12);
        if dmy {
            (y, m, d) = (yy, b, a);
        } else {
            (y, m, d) = (yy, a, b);
        }
    }
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || !(1900..=2200).contains(&y) {
        return None;
    }
    let mut secs = days_from_civil(y, m as u32, d as u32) * 86_400;
    let mut frac_us = 0i64;
    if !c.done() {
        // "8/24/2016, 6:27:37 PM": a comma may come between the date and the time
        let comma = c.eat(b',');
        if !(c.eat(b'T') || c.eat(b' ')) || (comma && c.done()) {
            return None;
        }
        while c.eat(b' ') {}
        let (hh, _) = c.digits(2)?;
        if !c.eat(b':') {
            return None;
        }
        let (mm, _) = c.digits(2)?;
        let mut ss = 0;
        if c.eat(b':') {
            ss = c.digits(2)?.0;
        }
        if c.peek() == Some(b'.') || c.peek() == Some(b',') {
            c.i += 1;
            let st = c.i;
            let (v, n) = c.digits(18)?;
            let _ = st;
            frac_us = if n >= 6 {
                v / 10i64.pow((n - 6) as u32)
            } else {
                v * 10i64.pow((6 - n) as u32)
            };
        }
        let mut hh = hh;
        let save = c.i;
        while c.eat(b' ') {}
        // bytes, not a str slice: byte 2 may fall inside a multi-byte character
        let rest = s.as_bytes().get(c.i..).unwrap_or(&[]);
        if rest.len() >= 2
            && (rest[..2].eq_ignore_ascii_case(b"AM") || rest[..2].eq_ignore_ascii_case(b"PM"))
        {
            let pm = rest[..2].eq_ignore_ascii_case(b"PM");
            if !(1..=12).contains(&hh) {
                return None;
            }
            hh = match (pm, hh) {
                (false, 12) => 0,
                (true, 12) => 12,
                (true, h) => h + 12,
                (false, h) => h,
            };
            c.i += 2;
        } else {
            c.i = save;
        }
        if hh > 23 || mm > 59 || ss > 60 {
            return None;
        }
        secs += hh * 3600 + mm * 60 + ss;
        while c.eat(b' ') {}
        if !c.done() {
            if c.eat(b'Z') {
            } else if let Some(off) = s.get(c.i..).and_then(zone_abbrev) {
                secs -= off;
                c.i = s.len();
            } else if c.peek() == Some(b'+') || c.peek() == Some(b'-') {
                let neg = c.peek() == Some(b'-');
                c.i += 1;
                let (oh, n) = c.digits(4)?;
                let (oh, om) = if n == 4 {
                    (oh / 100, oh % 100)
                } else {
                    c.eat(b':');
                    let om = c.digits(2).map(|x| x.0).unwrap_or(0);
                    (oh, om)
                };
                let off = oh * 3600 + om * 60;
                secs += if neg { off } else { -off };
            } else {
                return None;
            }
        }
        if !c.done() {
            return None;
        }
    }
    Some(secs * US + frac_us)
}

/// Decide DD/MM vs MM/DD from a column's samples (default MM/DD).
pub fn infer_date_order(samples: &[&str]) -> DateOrder {
    let (mut dmy, mut mdy) = (false, false);
    for s in samples {
        let s = s.trim();
        let mut parts = s.splitn(3, '/');
        let (Some(a), Some(b), Some(_)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if a.len() > 2 {
            continue;
        }
        let (Ok(a), Ok(b)) = (a.parse::<u32>(), b.parse::<u32>()) else {
            continue;
        };
        if a > 12 {
            dmy = true;
        }
        if b > 12 {
            mdy = true;
        }
    }
    if dmy && !mdy {
        DateOrder::Dmy
    } else {
        DateOrder::Mdy
    }
}

/// Offset in seconds east of UTC for a time-zone abbreviation at the end of a
/// timestamp (as syslog and many exporters write them). Only the
/// common, widely agreed readings are accepted; anything else is rejected
/// rather than guessed. The Check before opening screen shows the reading it used.
pub(crate) fn zone_abbrev(rest: &str) -> Option<i64> {
    const H: i64 = 3600;
    let z = rest.trim();
    if z.len() > 5 || !z.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    let off = match z.to_ascii_uppercase().as_str() {
        "UTC" | "GMT" | "UT" | "Z" | "WET" => 0,
        "EST" => -5 * H,
        "EDT" => -4 * H,
        "CST" => -6 * H,
        "CDT" => -5 * H,
        "MST" => -7 * H,
        "MDT" => -6 * H,
        "PST" => -8 * H,
        "PDT" => -7 * H,
        "AKST" => -9 * H,
        "AKDT" => -8 * H,
        "HST" => -10 * H,
        "WEST" | "BST" | "CET" => H,
        "CEST" | "EET" => 2 * H,
        "EEST" | "MSK" => 3 * H,
        "IST" => 5 * H + 1800,
        "SGT" | "HKT" | "AWST" => 8 * H,
        "JST" | "KST" => 9 * H,
        "AEST" => 10 * H,
        "AEDT" => 11 * H,
        "NZST" => 12 * H,
        "NZDT" => 13 * H,
        _ => return None,
    };
    Some(off)
}

fn month_number(w: &str) -> Option<u32> {
    const M: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    const FULL: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    let l = w.to_ascii_lowercase();
    let l = l.trim_end_matches('.');
    M.iter()
        .position(|m| *m == l)
        .or_else(|| FULL.iter().position(|m| *m == l))
        .or_else(|| (l == "sept").then_some(8))
        .map(|i| i as u32 + 1)
}

fn is_weekday(w: &str) -> bool {
    let l = w.trim_end_matches(',').to_ascii_lowercase();
    [
        "mon",
        "tue",
        "wed",
        "thu",
        "fri",
        "sat",
        "sun",
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
    ]
    .contains(&l.as_str())
}

fn day_year(d: &str, y: &str) -> Option<(u32, i64)> {
    let d: u32 = d.trim_end_matches(',').parse().ok()?;
    let y = y.trim_end_matches(',');
    if y.len() != 4 {
        return None;
    }
    Some((d, y.parse().ok()?))
}

/// Rewrites month-name dates into `YYYY-MM-DD[ rest]` for the numeric parser:
/// CLF `17/May/2015:08:05:32 +0000`, RFC 2822 `Sun, 17 May 2015 08:05:32 GMT`,
/// `17 May 2015 …`, `May 17, 2015 …`, ctime `Sun May 17 08:05:32 2015`,
/// and `17-May-2015 08:05`.
fn month_name_form(s: &str) -> Option<String> {
    let iso = |y: i64, m: u32, d: u32, rest: &str| {
        let rest = rest.trim();
        if rest.is_empty() {
            format!("{y:04}-{m:02}-{d:02}")
        } else {
            format!("{y:04}-{m:02}-{d:02} {rest}")
        }
    };
    // CLF: dd/Mon/yyyy:HH:MM:SS zone
    if let [d, mo, rest] = s.splitn(3, '/').collect::<Vec<_>>()[..] {
        let m = month_number(mo)?;
        let (y, time) = rest.split_once(':').unwrap_or((rest, ""));
        let (d, y) = day_year(d, y)?;
        return Some(iso(y, m, d, time));
    }
    // dd-Mon-yyyy[ time]: how spreadsheets and many exports write a date
    if let [d, mo, rest] = s.splitn(3, '-').collect::<Vec<_>>()[..] {
        if let Some(m) = month_number(mo) {
            let (y, time) = rest.split_once([' ', 'T']).unwrap_or((rest, ""));
            let (d, y) = day_year(d, y)?;
            return Some(iso(y, m, d, time));
        }
    }
    let mut w: Vec<&str> = s.split_whitespace().collect();
    if w.first().is_some_and(|x| is_weekday(x)) {
        w.remove(0);
    }
    if w.len() < 3 {
        return None;
    }
    // 17 May 2015 [time…]
    if let Some(m) = month_number(w[1]) {
        let (d, y) = day_year(w[0], w[2])?;
        return Some(iso(y, m, d, &w[3..].join(" ")));
    }
    let m = month_number(w[0])?;
    // May 17, 2015 [time…]
    if let Some((d, y)) = day_year(w[1], w[2]) {
        return Some(iso(y, m, d, &w[3..].join(" ")));
    }
    // ctime: May 17 08:05:32 2015
    if w.len() >= 4 && w[2].contains(':') {
        let (d, y) = day_year(w[1], w[3])?;
        let mut rest = vec![w[2]];
        rest.extend_from_slice(&w[4..]);
        return Some(iso(y, m, d, &rest.join(" ")));
    }
    None
}
