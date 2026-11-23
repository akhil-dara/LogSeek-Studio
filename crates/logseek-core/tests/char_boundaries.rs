//! Timestamp text with multi-byte characters never panics: it parses, or it
//! does not, and the layout template never splits a character.

use logseek_core::segment::format::TimeTemplate;
use logseek_core::time::{parse_time, parse_time_with, DateOrder};
use std::panic::catch_unwind;

#[test]
fn parse_time_survives_multibyte_after_the_time() {
    // byte 2 of the text after the time falls inside a 3-byte character
    for s in [
        "2016-08-24 18:27 \u{20ac}",
        "2016-08-24 18:27:37 \u{20ac}\u{20ac}",
        "2016-08-24 18:27:37\u{20ac}\u{20ac}",
        "2016-08-24 18:27:37 a\u{1F600}",
        "2016-08-24 18:27:37 \u{1F600}",
        "20160824T182737\u{20ac}",
        "20160824T182737\u{1F600}x",
        "2016-08-24 18:27:37 \u{00e9}\u{00e9}\u{00e9}",
    ] {
        let r = catch_unwind(|| parse_time(s));
        assert!(r.is_ok(), "panicked on {s:?}");
        assert_eq!(r.unwrap(), None, "{s:?} is not a time");
    }
}

#[test]
fn parse_time_still_reads_ordinary_forms() {
    assert!(parse_time("2016-08-24 06:27:37 PM").is_some());
    assert!(parse_time("20160824T182737Z").is_some());
    assert!(parse_time("2016-08-24 18:27:37 UTC").is_some());
    assert_eq!(
        parse_time_with("2016-08-24 18:27:37\u{3000}", DateOrder::Mdy),
        parse_time_with("2016-08-24 18:27:37", DateOrder::Mdy)
    );
}

#[test]
fn template_survives_unicode_spaces() {
    let t = TimeTemplate::infer("2016-01-01 10:00:00", DateOrder::Mdy).unwrap();
    for raw in [
        "2016-01-01 10:00:01\u{3000}",
        "2016-01-01 10:00:01\u{00A0}",
        "\u{00A0}2016-01-01 10:00:01",
        "\u{3000}2016-01-01 10:00:01x",
        "2016-01-01 10:00:01 \u{20ac}",
    ] {
        let r = catch_unwind(|| t.matches(raw));
        assert!(r.is_ok(), "matches panicked on {raw:?}");
        assert!(!r.unwrap(), "{raw:?} is not the learned layout");
        let r = catch_unwind(|| TimeTemplate::infer(raw, DateOrder::Mdy));
        assert!(r.is_ok(), "infer panicked on {raw:?}");
    }
}

#[test]
fn template_keeps_the_exact_text_of_a_value_with_a_unicode_tail() {
    // when a template does come out, it must reproduce the cell byte for byte
    let raw = "2016-01-01 10:00:01\u{3000}";
    if let Some(t) = TimeTemplate::infer(raw, DateOrder::Mdy) {
        let us = parse_time_with(raw, DateOrder::Mdy).unwrap();
        assert_eq!(t.render(us), raw);
    }
}
