//! Core value types shared by ingest, store and query.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColType {
    Text,
    Int,
    Float,
    Bool,
    /// Epoch microseconds, UTC.
    Time,
}

impl ColType {
    pub fn label(self) -> &'static str {
        match self {
            ColType::Text => "text",
            ColType::Int => "int",
            ColType::Float => "float",
            ColType::Bool => "bool",
            ColType::Time => "time",
        }
    }
}

/// `YYYY-MM-DD HH:MM:SS[.ffffff]` in UTC for epoch microseconds.
pub fn fmt_time(us: i64) -> String {
    let secs = us.div_euclid(1_000_000);
    let frac = us.rem_euclid(1_000_000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    if frac == 0 {
        format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}")
    } else if frac % 1000 == 0 {
        format!(
            "{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}.{:03}",
            frac / 1000
        )
    } else {
        format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}.{frac:06}")
    }
}

/// Howard Hinnant's days → civil date.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Civil date → days since 1970-01-01.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A borrowed cell value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value<'a> {
    Null,
    Text(&'a str),
    Int(i64),
    Float(f64),
    Bool(bool),
    /// Epoch microseconds, UTC.
    Time(i64),
}
