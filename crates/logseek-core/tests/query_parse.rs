//! Parsing queries into their syntax tree.
use logseek_core::query::ast::{AggFn, CmpOp, Expr, Pipe, Term};
use logseek_core::query::{parse, to_string};

fn term(e: &Expr) -> &Term {
    match e {
        Expr::Term(t) => t,
        other => panic!("not a term: {other:?}"),
    }
}

#[test]
fn free_text() {
    let q = parse("mimikatz").unwrap();
    assert_eq!(
        term(q.filter.as_ref().unwrap()),
        &Term::FreeText("mimikatz".into())
    );
}

#[test]
fn comparisons() {
    let cases = [
        ("EventID=4624", "EventID", CmpOp::Eq, "4624"),
        ("User!=SYSTEM", "User", CmpOp::Ne, "SYSTEM"),
        ("Bytes>1000", "Bytes", CmpOp::Gt, "1000"),
        ("Bytes>=10", "Bytes", CmpOp::Ge, "10"),
        ("n<5", "n", CmpOp::Lt, "5"),
        ("n<=5", "n", CmpOp::Le, "5"),
        ("Host:ws-*", "Host", CmpOp::Glob, "ws-*"),
        ("Cmd:powershell", "Cmd", CmpOp::Contains, "powershell"),
        (
            "Process~/(?i)psexec/",
            "Process",
            CmpOp::Regex,
            "(?i)psexec",
        ),
        (
            "#event_simpleName=ProcessRollup2",
            "#event_simpleName",
            CmpOp::Eq,
            "ProcessRollup2",
        ),
        ("event.type[0]=start", "event.type[0]", CmpOp::Eq, "start"),
        ("Payload.a[*].b=x", "Payload.a[*].b", CmpOp::Eq, "x"),
        ("`Dst Port`=443", "Dst Port", CmpOp::Eq, "443"),
    ];
    for (q, col, op, v) in cases {
        let p = parse(q).unwrap_or_else(|e| panic!("{q}: {e:?}"));
        assert_eq!(
            term(p.filter.as_ref().unwrap()),
            &Term::Cmp {
                col: col.into(),
                op,
                value: v.into()
            },
            "{q}"
        );
    }
}

#[test]
fn quoted_values_with_spaces() {
    let p = parse(r#"User="NT AUTHORITY\\SYSTEM""#).unwrap();
    assert_eq!(
        term(p.filter.as_ref().unwrap()),
        &Term::Cmp {
            col: "User".into(),
            op: CmpOp::Eq,
            value: "NT AUTHORITY\\SYSTEM".into()
        }
    );
    let p = parse(r#""failed login""#).unwrap();
    assert_eq!(
        term(p.filter.as_ref().unwrap()),
        &Term::FreeText("failed login".into())
    );
}

#[test]
fn implicit_and_and_precedence() {
    let p = parse("a b OR c AND NOT d").unwrap();
    // (a AND b) OR (c AND (NOT d))
    match p.filter.unwrap() {
        Expr::Or(l, r) => {
            assert!(matches!(*l, Expr::And(_, _)));
            match *r {
                Expr::And(_, rr) => assert!(matches!(*rr, Expr::Not(_))),
                other => panic!("{other:?}"),
            }
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn bang_negates() {
    let p = parse("!svchost").unwrap();
    assert!(matches!(p.filter.unwrap(), Expr::Not(_)));
}

#[test]
fn parentheses() {
    let p = parse("(a OR b) c").unwrap();
    match p.filter.unwrap() {
        Expr::And(l, _) => assert!(matches!(*l, Expr::Or(_, _))),
        other => panic!("{other:?}"),
    }
}

#[test]
fn pipes_stats_by() {
    let p = parse("EventID=4625 | stats count, dc(User) by Computer, Domain").unwrap();
    match &p.pipes[0] {
        Pipe::Stats { aggs, by } => {
            assert_eq!(aggs[0].func, AggFn::Count);
            assert_eq!(aggs[1].func, AggFn::Dc);
            assert_eq!(aggs[1].col.as_deref(), Some("User"));
            assert_eq!(by, &vec!["Computer".to_string(), "Domain".to_string()]);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn other_pipes() {
    let p = parse("x | top 5 Process | sort -Time, User | fields Time, User | head 20 | where n>1")
        .unwrap();
    assert_eq!(
        p.pipes[0],
        Pipe::Top {
            n: 5,
            col: "Process".into()
        }
    );
    assert_eq!(
        p.pipes[1],
        Pipe::Sort(vec![("Time".into(), true), ("User".into(), false)])
    );
    assert_eq!(p.pipes[2], Pipe::Fields(vec!["Time".into(), "User".into()]));
    assert_eq!(p.pipes[3], Pipe::Head(20));
    assert!(matches!(p.pipes[4], Pipe::Where(_)));
    assert_eq!(
        parse("| top Process").unwrap().pipes[0],
        Pipe::Top {
            n: 10,
            col: "Process".into()
        }
    );
}

#[test]
fn timechart_span_parse() {
    let p = parse("| timechart 1h count by EventID").unwrap();
    match &p.pipes[0] {
        Pipe::Timechart { span_us, agg, by } => {
            assert_eq!(*span_us, 3_600_000_000);
            assert_eq!(agg.func, AggFn::Count);
            assert_eq!(by.as_deref(), Some("EventID"));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        parse("| timechart 5m").unwrap().pipes[0],
        Pipe::Timechart {
            span_us: 300_000_000,
            ..
        }
    ));
}

#[test]
fn time_range_literal() {
    let p = parse("@2026-11-01..2026-11-03 EventID=1").unwrap();
    let (a, b) = p.time.unwrap();
    assert_eq!(b - a, 2 * 86_400 * 1_000_000);
    assert!(p.filter.is_some());
}

#[test]
fn empty_query_is_valid() {
    let p = parse("   ").unwrap();
    assert!(p.filter.is_none() && p.pipes.is_empty());
}

#[test]
fn error_has_span() {
    let e = parse("EventID= | stats").unwrap_err();
    assert!(e.span.0 <= 9 && e.span.1 >= e.span.0, "{e:?}");
    let e = parse("(a OR b").unwrap_err();
    assert!(e.msg.contains(")"), "{e:?}");
    let e = parse("x | stats bogus(y)").unwrap_err();
    assert_eq!(&"x | stats bogus(y)"[e.span.0..e.span.1], "bogus");
}

#[test]
fn roundtrip_canonical() {
    for q in [
        "mimikatz",
        "a b OR c AND NOT d",
        r#"User="NT AUTHORITY\\SYSTEM" Host:ws-* !svchost"#,
        "(a OR b) c | stats count, dc(User) by Computer | sort -count",
        "@2026-11-01..2026-11-03 Process~/(?i)psexec/ | timechart 1h count by EventID",
        "`Dst Port`=443 | fields `Dst Port`, Protocol | head 5",
    ] {
        let p1 = parse(q).unwrap();
        let s = to_string(&p1);
        let p2 = parse(&s).unwrap_or_else(|e| panic!("{s}: {e:?}"));
        assert_eq!(p1, p2, "{q} → {s}");
    }
}

#[test]
fn pipes_never_silently_dropped_for_rows() {
    use logseek_core::query::parse;
    // `| where` narrows the rows exactly like an AND
    let a = parse("EventID=4688 | where User=bob")
        .unwrap()
        .rows_only()
        .unwrap();
    let b = parse("EventID=4688 User=bob").unwrap();
    assert_eq!(a.filter, b.filter);
    assert!(a.pipes.is_empty());
    // a step the rows cannot show is an error in words, never ignored
    let e = parse("EventID=4688 | stats count by User")
        .unwrap()
        .rows_only()
        .unwrap_err();
    assert!(e.contains("stats") && e.contains("Summarise"), "{e}");
}

#[test]
fn time_range_anywhere_in_the_query() {
    use logseek_core::query::parse;
    let a = parse("@2026-11-13..2026-11-14 sudo").unwrap();
    let b = parse("sudo @2026-11-13..2026-11-14").unwrap();
    let c = parse("sudo @2026-11-13..2026-11-14 | where User=bob").unwrap();
    assert!(a.time.is_some());
    assert_eq!(a.time, b.time);
    assert_eq!(a.filter, b.filter, "the range is not searched as text");
    assert_eq!(c.time, a.time);
}

fn cmp(q: &str) -> (String, CmpOp, String) {
    let p = parse(q).unwrap_or_else(|e| panic!("{q}: {e:?}"));
    match term(p.filter.as_ref().unwrap()) {
        Term::Cmp { col, op, value } => (col.clone(), *op, value.clone()),
        other => panic!("{q}: {other:?}"),
    }
}

#[test]
fn comparisons_typed_the_way_other_tools_write_them_are_understood() {
    let is = |q: &str, col: &str, op: CmpOp, v: &str| {
        assert_eq!(cmp(q), (col.to_string(), op, v.to_string()), "{q}");
    };
    // two equals signs, and "not equal" written with angle brackets
    is("EventID==1", "EventID", CmpOp::Eq, "1");
    is("a<>b", "a", CmpOp::Ne, "b");
    // spaces around the sign, on either side or both
    is("EventID = 1", "EventID", CmpOp::Eq, "1");
    is("EventID =1", "EventID", CmpOp::Eq, "1");
    is("EventID= 1", "EventID", CmpOp::Eq, "1");
    is("Bytes >= 10", "Bytes", CmpOp::Ge, "10");
    is("EventID == 1", "EventID", CmpOp::Eq, "1");
    is("User != SYSTEM", "User", CmpOp::Ne, "SYSTEM");
    is(
        r#"User = "NT AUTHORITY""#,
        "User",
        CmpOp::Eq,
        "NT AUTHORITY",
    );
    is("`Dst Port` = 443", "Dst Port", CmpOp::Eq, "443");
    // and they still join with what follows
    let p = parse("EventID = 1 User = bob").unwrap();
    assert_eq!(p, parse("EventID=1 User=bob").unwrap());
    let p = parse("EventID = 1 OR EventID = 3").unwrap();
    assert_eq!(p, parse("EventID=1 OR EventID=3").unwrap());
}

#[test]
fn small_joining_words_are_never_searched_by_accident() {
    // lowercase "or", "and", "not" between terms: said in words, not searched
    for (q, word, caps) in [
        ("a or b", "or", "OR"),
        ("EventID=1 and User=bob", "and", "AND"),
        ("a And b", "And", "AND"),
        ("not svchost", "not", "NOT"),
        ("a not b", "not", "NOT"),
    ] {
        let e = parse(q).unwrap_err();
        assert_eq!(&q[e.span.0..e.span.1], word, "{q}: {e:?}");
        assert!(
            e.msg.contains(caps) && e.msg.contains('"'),
            "{q}: {}",
            e.msg
        );
    }
    // in quotes they are words to look for; alone they are words too
    assert!(parse(r#"a "or" b"#).is_ok());
    assert!(parse(r#""not found""#).is_ok());
    assert_eq!(
        term(parse("or").unwrap().filter.as_ref().unwrap()),
        &Term::FreeText("or".into())
    );
    // as a value they are values
    assert_eq!(
        cmp("Answer=not"),
        ("Answer".into(), CmpOp::Eq, "not".into())
    );
}
