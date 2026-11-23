//! Inferring column types and time formats from values.
use logseek_core::ingest::batch::RecordBatch;
use logseek_core::ingest::infer::{infer_types, pick_time_column, pick_time_column_with};
use logseek_core::time::{infer_date_order, parse_time, parse_time_with, DateOrder};
use logseek_core::types::ColType;

const T0: i64 = 1_794_564_646_000_000; // 2026-11-13 10:10:46 UTC, in µs

fn us(y: i64, mo: u32, d: u32, h: i64, mi: i64, s: i64) -> i64 {
    (logseek_core::types::days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s) * 1_000_000
}

#[test]
fn iso_and_rfc3339() {
    let base = us(2026, 11, 1, 0, 10, 0);
    assert_eq!(parse_time("2026-11-01T00:10:00Z"), Some(base));
    assert_eq!(parse_time("2026-11-01 00:10:00"), Some(base));
    assert_eq!(
        parse_time("2026-11-01T00:10:00.123456Z"),
        Some(base + 123_456)
    );
    assert_eq!(
        parse_time("2026-11-01T00:10:00.123456789Z"),
        Some(base + 123_456)
    );
    assert_eq!(parse_time("2026-11-01T05:40:00+05:30"), Some(base));
    assert_eq!(
        parse_time("2026-11-01T00:10:00-0100"),
        Some(base + 3_600_000_000)
    );
    assert_eq!(parse_time("2026-11-01 00:10:00,250"), Some(base + 250_000));
    assert_eq!(parse_time("2026/11/01 00:10:00"), Some(base));
    assert_eq!(parse_time("2026-11-01"), Some(us(2026, 11, 1, 0, 0, 0)));
}

#[test]
fn slash_dates_and_ampm() {
    assert_eq!(
        parse_time("11/01/2026 12:10:00 AM"),
        Some(us(2026, 11, 1, 0, 10, 0))
    );
    assert_eq!(
        parse_time("11/01/2026 01:10:00 PM"),
        Some(us(2026, 11, 1, 13, 10, 0))
    );
    // first field > 12 can only be a day
    assert_eq!(
        parse_time("14/02/2018 08:31:01"),
        Some(us(2018, 2, 14, 8, 31, 1))
    );
    assert_eq!(
        parse_time_with("02/03/2018 08:31:01", DateOrder::Dmy),
        Some(us(2018, 3, 2, 8, 31, 1))
    );
    assert_eq!(
        parse_time_with("02/03/2018 08:31:01", DateOrder::Mdy),
        Some(us(2018, 2, 3, 8, 31, 1))
    );
}

#[test]
fn date_order_inferred_from_column() {
    assert_eq!(
        infer_date_order(&["01/02/2018 1:00:00", "14/02/2018 1:00:00"]),
        DateOrder::Dmy
    );
    assert_eq!(
        infer_date_order(&["01/02/2018 1:00:00", "02/14/2018 1:00:00"]),
        DateOrder::Mdy
    );
}

#[test]
fn epochs_by_magnitude() {
    let s = 1_794_564_646i64;
    assert_eq!(parse_time("1794564646"), Some(s * 1_000_000));
    assert_eq!(parse_time("1794564646100"), Some(s * 1_000_000 + 100_000));
    assert_eq!(
        parse_time("1794564646100200"),
        Some(s * 1_000_000 + 100_200)
    );
    assert_eq!(
        parse_time("1794564646100200300"),
        Some(s * 1_000_000 + 100_200)
    );
    assert_eq!(parse_time("1794564643.826"), Some(1_794_564_643_826_000));
    // Windows FILETIME: 100 ns ticks since 1601
    let ft = (s + 11_644_473_600) * 10_000_000;
    assert_eq!(parse_time(&ft.to_string()), Some(s * 1_000_000));
    assert_eq!(parse_time("4624"), None);
    assert_eq!(parse_time("hello"), None);
    let _ = T0;
}

fn batch(cols: usize, rows: &[&[&str]]) -> RecordBatch {
    let mut b = RecordBatch::new(cols);
    for (i, r) in rows.iter().enumerate() {
        b.begin_row(i as u64);
        for (c, v) in r.iter().enumerate() {
            b.set(c, v);
        }
    }
    b
}

#[test]
fn int_float_bool_text_time_inference() {
    let mut rows: Vec<Vec<String>> = Vec::new();
    for i in 0..100 {
        rows.push(vec![
            format!("{}", 4600 + i),
            format!("{}.5", i),
            if i % 2 == 0 {
                "true".into()
            } else {
                "False".into()
            },
            format!("2026-11-01T00:{:02}:00Z", i % 60),
            format!("user{i}"),
            format!("{:03}", i),
            "".into(),
        ]);
    }
    rows[7][0] = "N/A".into(); // 1 % junk still Int (95 % rule)
    let refs: Vec<Vec<&str>> = rows
        .iter()
        .map(|r| r.iter().map(|s| s.as_str()).collect())
        .collect();
    let refs2: Vec<&[&str]> = refs.iter().map(|r| r.as_slice()).collect();
    let t = infer_types(&batch(7, &refs2));
    assert_eq!(
        t,
        vec![
            ColType::Int,
            ColType::Float,
            ColType::Bool,
            ColType::Time,
            ColType::Text,
            ColType::Text,
            ColType::Text
        ]
    );
}

#[test]
fn leading_zero_codes_stay_text() {
    let t = infer_types(&batch(
        1,
        &[&["007"], &["010"], &["123"], &["042"], &["099"]],
    ));
    assert_eq!(t, vec![ColType::Text]);
}

#[test]
fn pick_time_column_goes_by_the_data_never_the_name() {
    // two time columns: the first in the file, whatever they are called
    let names = ["id", "created", "Timestamp", "msg"].map(String::from);
    let types = [ColType::Int, ColType::Time, ColType::Time, ColType::Text];
    assert_eq!(pick_time_column(&names, &types), Some(1));
    // the fuller one wins over a name anyone might expect
    let counts = [100, 10, 100, 100];
    assert_eq!(pick_time_column_with(&names, &types, &counts), Some(2));
    let names2 = ["a", "when"].map(String::from);
    assert_eq!(
        pick_time_column(&names2, &[ColType::Text, ColType::Time]),
        Some(1)
    );
    assert_eq!(
        pick_time_column(&names2, &[ColType::Text, ColType::Text]),
        None
    );
}

#[test]
fn time_zone_abbreviations_at_the_end_of_a_time_parse() {
    // A CSV export with "_time" = "2016-08-28 17:58:59.000 MDT" (MDT = UTC-6)
    assert_eq!(
        parse_time("2016-08-28 17:58:59.000 MDT"),
        Some(us(2016, 8, 28, 23, 58, 59))
    );
    assert_eq!(
        parse_time("2016-08-28 17:58:59 EST"),
        Some(us(2016, 8, 28, 22, 58, 59))
    );
    assert_eq!(
        parse_time("2016-08-28 17:58:59 PDT"),
        Some(us(2016, 8, 29, 0, 58, 59))
    );
    assert_eq!(
        parse_time("2016-08-28 17:58:59 CEST"),
        Some(us(2016, 8, 28, 15, 58, 59))
    );
    assert_eq!(
        parse_time("2016-08-28 17:58:59 utc"),
        Some(us(2016, 8, 28, 17, 58, 59))
    );
    // unknown words are still rejected, never guessed
    assert_eq!(parse_time("2016-08-28 17:58:59 XYZ"), None);
    assert_eq!(parse_time("2016-08-28 17:58:59 hello"), None);
}

#[test]
fn pick_time_column_prefers_whole_word_top_level_well_populated_columns() {
    use logseek_core::ingest::infer::pick_time_column_with;
    // GH Archive: "updated_at" must not match "date" by substring, and the
    // top-level, always-present "created_at" beats a sparse nested one.
    let names = [
        "id",
        "payload.pull_request.updated_at",
        "created_at",
        "payload.pull_request.created_at",
    ]
    .map(String::from);
    let types = [ColType::Text, ColType::Time, ColType::Time, ColType::Time];
    assert_eq!(
        pick_time_column_with(&names, &types, &[1000, 40, 1000, 40]),
        Some(2)
    );
    // a sparse column loses to a well-populated one even with a better name
    let names2 = ["timestamp", "date"].map(String::from);
    assert_eq!(
        pick_time_column_with(&names2, &[ColType::Time, ColType::Time], &[30, 1000]),
        Some(1)
    );
    // A "_time" column is an exact "time"
    let names3 = ["_serial", "_time", "_indextime"].map(String::from);
    assert_eq!(
        pick_time_column_with(
            &names3,
            &[ColType::Int, ColType::Time, ColType::Time],
            &[9, 9, 9]
        ),
        Some(1)
    );
}

#[test]
fn month_name_timestamps_parse() {
    // nginx/Apache access logs (CLF)
    assert_eq!(
        parse_time("17/May/2015:08:05:32 +0000"),
        Some(us(2015, 5, 17, 8, 5, 32))
    );
    assert_eq!(
        parse_time("17/May/2015:10:05:32 +0200"),
        Some(us(2015, 5, 17, 8, 5, 32))
    );
    // RFC 2822 / email / HTTP dates
    assert_eq!(
        parse_time("Sun, 17 May 2015 08:05:32 GMT"),
        Some(us(2015, 5, 17, 8, 5, 32))
    );
    assert_eq!(
        parse_time("17 May 2015 08:05:32 +0000"),
        Some(us(2015, 5, 17, 8, 5, 32))
    );
    // US long form and ctime
    assert_eq!(
        parse_time("May 17, 2015 08:05:32"),
        Some(us(2015, 5, 17, 8, 5, 32))
    );
    assert_eq!(
        parse_time("September 3, 2024"),
        Some(us(2024, 9, 3, 0, 0, 0))
    );
    assert_eq!(
        parse_time("Sun May 17 08:05:32 2015"),
        Some(us(2015, 5, 17, 8, 5, 32))
    );
    // not dates
    assert_eq!(parse_time("May the force be with you"), None);
    assert_eq!(parse_time("17/Foo/2015:08:05:32 +0000"), None);
}

#[test]
fn a_day_month_name_year_with_dashes_reads_as_a_time() {
    // how spreadsheets and many exports write a date: 01-Mar-2026 08:00
    assert_eq!(
        parse_time("01-Mar-2026 08:00"),
        parse_time("2026-03-01 08:00:00")
    );
    assert_eq!(
        parse_time("01-Mar-2026 08:00:22.798"),
        parse_time("2026-03-01 08:00:22.798")
    );
    assert_eq!(parse_time("01-Mar-2026"), parse_time("2026-03-01"));
    assert!(parse_time("2026-03-01 08:00:00").is_some());
    // a word that is not a month stays text
    assert_eq!(parse_time("01-Foo-2026 08:00"), None);
    assert_eq!(parse_time("CODE-0331-2026"), None);
}
