//! Time and counts in words, for the time spine and the windows: a time as
//! short as its range allows, a bucket's length, a count with its thousands.

use logseek_core::types::fmt_time;

/// A time as short as the range it sits in allows: the day for a range of
/// days, the month-day and minute otherwise.
pub(crate) fn short_time(us: i64, range_us: i64) -> String {
    let full = fmt_time(us); // "YYYY-MM-DD HH:MM:SS…"
    let day = 86_400_000_000i64;
    if range_us > 3 * day {
        full.get(..10).unwrap_or(&full).to_string()
    } else {
        full.get(5..16).unwrap_or(&full).replace('T', " ")
    }
}

/// A tick on the time rail, `step_us` from the next: the date when ticks are a
/// day or more apart, else the date and hour (two ticks never read the same).
pub(crate) fn tick_label(us: i64, step_us: i64) -> String {
    let full = fmt_time(us);
    if step_us >= 86_400_000_000 {
        full.get(..10).unwrap_or(&full).to_string()
    } else {
        full.get(5..16).unwrap_or(&full).replace('T', " ")
    }
}

/// 3_600_000_000 µs → "hour".
pub(crate) fn span_label(us: i64) -> String {
    const S: i64 = 1_000_000;
    match us {
        x if x >= 7 * 86_400 * S => format!("{} days", x / (86_400 * S)),
        x if x >= 86_400 * S => "day".into(),
        x if x >= 3600 * S => {
            if x == 3600 * S {
                "hour".into()
            } else {
                format!("{} hours", x / (3600 * S))
            }
        }
        x if x >= 60 * S => {
            if x == 60 * S {
                "minute".into()
            } else {
                format!("{} minutes", x / (60 * S))
            }
        }
        x if x >= S => format!("{} s", x / S),
        x => format!("{} ms", x / 1000),
    }
}

/// 1234567 → "1,234,567".
pub fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn rail_ticks_less_than_a_day_apart_say_the_hour() {
        let day = 86_400_000_000i64;
        // 2026-11-12 12:00 UTC
        let noon = 1_794_484_800_000_000i64;
        // ticks a day or more apart: the date is enough
        assert_eq!(super::tick_label(noon, day), "2026-11-12");
        // closer than a day: two ticks on the same date must differ
        assert_eq!(super::tick_label(noon, day / 2), "11-12 12:00");
        assert_ne!(
            super::tick_label(noon - day / 2, day / 2),
            super::tick_label(noon, day / 2)
        );
    }
    #[test]
    fn grouping() {
        assert_eq!(super::group(0), "0");
        assert_eq!(super::group(1234), "1,234");
        assert_eq!(super::group(45767021), "45,767,021");
    }
}
