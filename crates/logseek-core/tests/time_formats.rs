//! Every common timestamp shape an analyst meets parses to the same instant,
//! with its full precision (microseconds stored; finer digits floored).

use logseek_core::time::parse_time;

const T: i64 = 1_472_063_257_000_000; // 2016-08-24 18:27:37 UTC

#[test]
fn many_formats_reach_the_same_instant() {
    let cases: &[(&str, i64)] = &[
        ("2016-08-24T18:27:37Z", T),
        ("2016-08-24T18:27:37.123456Z", T + 123_456),
        ("2016-08-24T18:27:37.123456789Z", T + 123_456),
        ("2016-08-24T18:27:37.1234567Z", T + 123_456),
        ("2016-08-24 18:27:37,123", T + 123_000),
        ("2016-08-24T12:27:37-06:00", T),
        ("2016-08-24T12:27:37-0600", T),
        ("2016-08-24 18:27:37 UTC", T),
        ("2016-08-24 12:27:37.000 MDT", T),
        ("2016/08/24 18:27:37", T),
        ("08/24/2016 06:27:37 PM", T),
        ("24.08.2016 18:27:37", T),
        ("24/Aug/2016:18:27:37 +0000", T),
        ("Wed, 24 Aug 2016 18:27:37 +0000", T),
        ("Aug 24 2016 18:27:37", T),
        ("Wednesday, August 24, 2016 6:27:37 PM", T),
        ("20160824T182737Z", T),
        ("1472063257", T),
        ("1472063257.123456", T + 123_456),
        ("1472063257123", T + 123_000),
        ("1472063257123456", T + 123_456),
        ("1472063257123456789", T + 123_456),
        ("131165368571234567", T + 123_456),
        ("13116536857123456", T + 123_456),
        ("636076600571234567", T + 123_456),
        ("20160824T182737.123456Z", T + 123_456),
    ];
    let mut bad = Vec::new();
    for (text, want) in cases {
        let got = parse_time(text);
        if got != Some(*want) {
            bad.push(format!("{text:?}: got {got:?}, want {want}"));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} formats wrong:\n{}",
        bad.len(),
        cases.len(),
        bad.join("\n")
    );
}

#[test]
fn not_set_values_are_not_times() {
    for text in ["0", "-1", "", "  ", "not a time", "12345"] {
        assert_eq!(parse_time(text), None, "{text:?}");
    }
}

#[test]
fn a_comma_after_the_date_and_a_12_hour_clock_read_as_a_time() {
    // as Azure Log Analytics exports write them
    assert_eq!(parse_time("8/24/2016, 6:27:37.000 PM"), Some(T));
    assert_eq!(parse_time("8/24/2016, 6:27:37.454 PM"), Some(T + 454_000));
    assert_eq!(
        parse_time("8/24/2016, 12:05:00.000 AM"),
        parse_time("2016-08-24 00:05:00")
    );
    // a comma alone, with no time after it, is not a time
    assert_eq!(parse_time("8/24/2016,"), None);
}
