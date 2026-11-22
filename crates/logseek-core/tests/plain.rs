//! Plain words read against the open file: column names and values come
//! from the file; only the small words of the language (over, not, by, last)
//! are known in advance. Every word is accounted for.

use logseek_core::query::parse;
use logseek_core::query::plain::{recast, ColInfo, Lexicon};

fn lex() -> Lexicon {
    let cols = vec![
        ColInfo::new("order_status", false, false, None),
        ColInfo::new("billing_city", false, false, None),
        ColInfo::new("grand_total", true, false, Some((0.0, 90_000.0))),
        ColInfo::new("TargetUserName", false, false, None),
        ColInfo::new("SubjectUserName", false, false, None),
        ColInfo::new("Time", false, true, None),
        ColInfo::new("status_code", true, false, Some((100.0, 599.0))),
        ColInfo::new("region_code", true, false, Some((100.0, 999.0))),
    ];
    let mut l = Lexicon::new(cols);
    l.add_value(0, "cancelled", 412);
    l.add_value(0, "shipped", 9_000);
    l.add_value(1, "Hyderabad", 1_200);
    l.add_value(1, "Mumbai", 3_000);
    l.add_value(3, "SYSTEM", 80);
    l.add_value(4, "SYSTEM", 5_000);
    l.add_value(6, "200", 70_000);
    l.add_value(7, "200", 30);
    l.time_span = Some((
        1_794_441_600_000_000,
        1_794_441_600_000_000 + 86_400 * 1_000_000,
    ));
    l
}

fn words(text: &str) -> (Vec<String>, String) {
    let r = recast(text, &lex());
    let w: Vec<String> = r.parts.iter().map(|p| p.words.clone()).collect();
    (w, r.query())
}

#[test]
fn values_are_found_in_their_columns_without_naming_them() {
    let (w, q) = words("cancelled from hyderabad above 15k");
    assert_eq!(
        w,
        vec![
            "order_status is cancelled",
            "billing_city is Hyderabad",
            "grand_total is more than 15,000"
        ]
    );
    assert_eq!(
        q,
        "order_status=\"cancelled\" billing_city=\"Hyderabad\" grand_total>15000"
    );
    assert!(parse(&q).is_ok());
}

#[test]
fn a_column_named_in_parts_and_negations_and_empties_are_understood() {
    let (w, q) = words("orders not shipped and billing city is empty");
    assert_eq!(
        w,
        vec!["order_status is not shipped", "billing_city is empty"]
    );
    assert_eq!(q, "NOT order_status=\"shipped\" NOT billing_city:*");
    // a plural, a different case
    let (w, _) = words("Grand Totals under 500");
    assert_eq!(w, vec!["grand_total is less than 500"]);
    let (w, _) = words("grand total between 10 and 20");
    assert_eq!(w, vec!["grand_total is between 10 and 20"]);
}

#[test]
fn a_word_that_fits_two_columns_offers_both_with_the_likelier_first() {
    let r = recast("system", &lex());
    assert_eq!(r.parts.len(), 1);
    let p = &r.parts[0];
    // the value is far more common in SubjectUserName
    assert_eq!(p.words, "SubjectUserName is SYSTEM");
    assert_eq!(p.others.len(), 1);
    assert_eq!(p.others[0].words, "TargetUserName is SYSTEM");
    // excluding: the same choice, turned round
    let r = recast("without 200", &lex());
    assert_eq!(r.parts[0].words, "status_code is not 200");
    assert_eq!(r.parts[0].others[0].words, "region_code is not 200");
}

#[test]
fn time_is_read_against_the_file_and_grouping_asks_for_a_summary() {
    let r = recast("cancelled in the last 2 hours", &lex());
    assert_eq!(r.parts[0].words, "order_status is cancelled");
    assert_eq!(r.parts[1].words, "the last 2 hours of the file");
    assert!(r.query().starts_with("@"), "{}", r.query());
    let p = parse(&r.query()).unwrap();
    let (a, b) = p.time.unwrap();
    assert_eq!(b - a, 2 * 3600 * 1_000_000);
    let r = recast("average grand total by billing city per hour", &lex());
    let s = r.summary.unwrap();
    assert_eq!(s.by, vec!["billing_city".to_string()]);
    assert_eq!(s.measure, "average");
    assert_eq!(s.column.as_deref(), Some("grand_total"));
    assert_eq!(s.per, Some(3_600_000_000));
    let r = recast("how many shipped by city", &lex());
    let s = r.summary.unwrap();
    assert_eq!(
        (s.measure.as_str(), s.by[0].as_str()),
        ("rows", "billing_city")
    );
}

#[test]
fn every_word_is_accounted_for() {
    let r = recast("cancelled mimikatz", &lex());
    assert_eq!(r.parts[0].words, "order_status is cancelled");
    assert_eq!(r.parts[1].words, "\"mimikatz\" anywhere");
    assert_eq!(r.query(), "order_status=\"cancelled\" \"mimikatz\"");
    // words that only join others are not looked for
    let r = recast("show me the rows where status code is 200", &lex());
    assert_eq!(r.parts.len(), 1);
    assert_eq!(r.parts[0].words, "status_code is 200");
    // a column named on its own: the rows where it has a value
    let r = recast("billing city in the last 2 hours", &lex());
    assert_eq!(r.parts[0].words, "billing_city has a value");
    assert_eq!(r.parts[0].term, "billing_city:*");
    assert!(r.parts[1].time);
    let r = recast("grand total", &lex());
    assert_eq!(r.parts[0].words, "grand_total has a value");
    // nothing the file knows: nothing to recast
    assert!(!recast("mimikatz", &lex()).understood());
    assert!(recast("cancelled", &lex()).understood());
}

#[test]
fn a_real_file_is_read_into_words_and_the_search_gives_the_right_rows() {
    use logseek_core::budget::Budget;
    use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
    use logseek_core::query::exec::{filter, ChunkCache, FormulaSet};
    use logseek_core::query::plain::build_lexicon;
    use std::sync::atomic::AtomicBool;
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("orders.csv");
    let mut text = String::from("order_status,billing_city,grand_total,order_id\n");
    let mut want = 0;
    for i in 0..9_000u32 {
        let st = ["shipped", "cancelled", "pending"][(i % 3) as usize];
        let city = ["Hyderabad", "Mumbai", "Pune", "Delhi"][(i % 4) as usize];
        let total = (i * 37) % 40_000;
        if st == "cancelled" && city == "Hyderabad" && total > 15_000 {
            want += 1;
        }
        text.push_str(&format!("{st},{city},{total},ORD-{i}\n"));
    }
    std::fs::write(&src, text).unwrap();
    let dir = d.path().join("o.lsidx");
    let job = start_ingest(&src, &dir, Budget::new(256 << 20), IngestOptions::default());
    while matches!(job.state(), JobState::Running) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let st = logseek_core::store::Store::open(&dir).unwrap();
    let cache = ChunkCache::new(Budget::new(64 << 20));
    let no = AtomicBool::new(false);
    let lex = build_lexicon(&st, &FormulaSet::default(), &cache, &no).unwrap();
    // statuses, cities and order ids are read as values (an id typed finds
    // its column); the totals are numbers, read by their range
    assert_eq!(lex.values(), 3 + 4 + 9_000);
    assert_eq!(recast("ORD-77", &lex).parts[0].words, "order_id is ORD-77");
    let r = recast("cancelled orders from hyderabad above 15k", &lex);
    assert_eq!(
        r.parts.iter().map(|p| p.words.as_str()).collect::<Vec<_>>(),
        vec![
            "order_status is cancelled",
            "billing_city is Hyderabad",
            "grand_total is more than 15,000"
        ]
    );
    let n = filter(&st, &parse(&r.query()).unwrap(), &cache, &no)
        .unwrap()
        .count();
    assert_eq!(n, want);
}
