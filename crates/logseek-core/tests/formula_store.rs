//! Made columns evaluated against a stored index.
use logseek_core::budget::Budget;
use logseek_core::ingest::pipeline::{start_ingest, IngestOptions, JobState};
use logseek_core::query::exec::{filter_with, page_with, ChunkCache, FormulaSet};
use logseek_core::query::parse;
use logseek_core::store::Store;
use std::io::Write;
use std::sync::atomic::AtomicBool;

fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("f.csv");
    let mut f = std::fs::File::create(&src).unwrap();
    writeln!(f, "User,Bytes,Cmd").unwrap();
    for i in 0..3000 {
        let dom = ["CORP", "LAB"][i % 2];
        writeln!(
            f,
            "{dom}{}u{i},{},\"powershell -enc SQBFAFgA\"",
            '\\',
            i * 100
        )
        .unwrap();
    }
    drop(f);
    let dir = d.path().join("f.lsidx");
    let job = start_ingest(
        &src,
        &dir,
        Budget::new(128 << 20),
        IngestOptions {
            chunk_bytes: 32 * 1024,
            ..Default::default()
        },
    );
    while matches!(job.state(), JobState::Running) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    (d, Store::open(&dir).unwrap())
}

#[test]
fn formula_column_filterable_and_shown() {
    let (_d, st) = store();
    let mut fs = FormulaSet::default();
    fs.add(&st, "domain", r#"split(User, "\\", 0)"#).unwrap();
    fs.add(&st, "kb", "Bytes / 1024").unwrap();
    fs.add(
        &st,
        "decoded",
        r#"base64_decode(regex(Cmd, "-enc\s+(\S+)", 1), "utf16")"#,
    )
    .unwrap();
    let c = ChunkCache::new(Budget::new(32 << 20));
    let no = AtomicBool::new(false);
    let m = filter_with(&st, &parse("domain=CORP").unwrap(), &fs, &c, &no).unwrap();
    assert_eq!(m.count(), 1500);
    let m = filter_with(&st, &parse("kb>200 domain=lab").unwrap(), &fs, &c, &no).unwrap();
    let want = (0..3000)
        .filter(|i| i % 2 == 1 && (i * 100) as f64 / 1024.0 > 200.0)
        .count() as u64;
    assert_eq!(m.count(), want);
    let ns = st.schema().len();
    let p = page_with(&st, &m, 0, 2, &[0, ns, ns + 1, ns + 2], &fs, &c).unwrap();
    assert_eq!(p.cells[0][1].as_deref(), Some("LAB"));
    assert_eq!(p.cells[0][3].as_deref(), Some("IEX"));
    let kb: f64 = p.cells[0][2].as_deref().unwrap().parse().unwrap();
    assert!(kb > 200.0);
}

#[test]
fn bad_formula_reports_error() {
    let (_d, st) = store();
    let mut fs = FormulaSet::default();
    let e = fs.add(&st, "x", "lower(Usr)").unwrap_err();
    assert!(e.msg.contains("User"));
    let e = fs.add(&st, "User", "1").unwrap_err();
    assert!(e.msg.contains("already"), "{}", e.msg);
}

#[test]
fn formula_name_clash_ignores_case() {
    let (_d, st) = store();
    let mut fs = logseek_core::query::exec::FormulaSet::default();
    assert!(
        fs.add(&st, "user", "len(Cmd)").is_err(),
        "\"user\" would be shadowed by the stored \"User\""
    );
    assert!(fs.add(&st, "cmd_len", "len(Cmd)").is_ok());
    assert!(fs.add(&st, "CMD_LEN", "len(Cmd)").is_err());
}

#[test]
fn errors_never_match_comparisons_but_negation_does() {
    let (_d, st) = store();
    let mut fs = FormulaSet::default();
    fs.add(&st, "bad", "num(User)").unwrap();
    let c = ChunkCache::new(Budget::new(32 << 20));
    let no = AtomicBool::new(false);
    let count = |q: &str| {
        filter_with(&st, &parse(q).unwrap(), &fs, &c, &no)
            .unwrap()
            .count()
    };
    assert_eq!(count("bad>5"), 0);
    assert_eq!(count("bad=5"), 0);
    assert_eq!(count("NOT bad>5"), 3000);
    // "is not 5" is "not (is 5)" in the query language: kept, as a missing value is
    assert_eq!(count("bad!=5"), 3000);
}

#[test]
fn page_reports_error_reasons() {
    let (_d, st) = store();
    let mut fs = FormulaSet::default();
    fs.add(&st, "bad", "num(User)").unwrap();
    fs.add(&st, "kb", "Bytes / 1024").unwrap();
    let c = ChunkCache::new(Budget::new(32 << 20));
    let ns = st.schema().len();
    let all = logseek_core::query::exec::MatchSet::all(&st);
    let p = page_with(&st, &all, 0, 3, &[0, ns, ns + 1], &fs, &c).unwrap();
    // the error cell has no text; its reason is listed by page row and requested column
    assert_eq!(p.cells[1][1], None);
    assert_eq!(p.errors.len(), 3, "{:?}", p.errors);
    for (k, (row, col, why)) in p.errors.iter().enumerate() {
        assert_eq!((*row, *col), (k, 1));
        assert!(why.starts_with("not a number: "), "{why}");
    }
    assert_eq!(p.cells[1][2].as_deref(), Some("0.09765625"));
}

fn defs(d: &[(&str, &str)]) -> Vec<(String, String)> {
    d.iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

#[test]
fn made_column_uses_made_column() {
    let (_d, st) = store();
    let (fs, errs) = FormulaSet::from_defs(
        &st,
        &defs(&[
            ("label", r#"if(is_corp, "c-" & domain, "other")"#),
            ("domain", r#"split(User, "\\", 0)"#),
            ("is_corp", r#"domain == "CORP""#),
        ]),
    );
    assert!(errs.is_empty(), "{errs:?}");
    // working columns keep the order they were given in
    let names: Vec<&str> = fs.items.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["label", "domain", "is_corp"]);
    let c = ChunkCache::new(Budget::new(32 << 20));
    let ns = st.schema().len();
    let all = logseek_core::query::exec::MatchSet::all(&st);
    let p = page_with(&st, &all, 0, 2, &[ns, ns + 1, ns + 2], &fs, &c).unwrap();
    assert_eq!(p.cells[0][0].as_deref(), Some("c-CORP"));
    assert_eq!(p.cells[1][0].as_deref(), Some("other"));
    assert_eq!(p.cells[1][1].as_deref(), Some("LAB"));
    assert_eq!(p.cells[0][2].as_deref(), Some("true"));
    let no = AtomicBool::new(false);
    let m = filter_with(&st, &parse("label=other").unwrap(), &fs, &c, &no).unwrap();
    assert_eq!(m.count(), 1500);
}

#[test]
fn cycle_is_refused_by_name() {
    let (_d, st) = store();
    let (fs, errs) = FormulaSet::from_defs(
        &st,
        &defs(&[
            ("x", r#"y & "a""#),
            ("y", r#"x & "b""#),
            ("me", r#"me & "c""#),
            ("fine", "upper(User)"),
        ]),
    );
    let names: Vec<&str> = fs.items.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["fine"]);
    let why = |n: &str| {
        errs.iter()
            .find(|(c, _)| c == n)
            .map(|(_, e)| e.msg.clone())
            .unwrap_or_else(|| panic!("no error for {n}: {errs:?}"))
    };
    assert_eq!(why("x"), "x uses y, which uses x");
    assert_eq!(why("y"), "y uses x, which uses y");
    assert_eq!(why("me"), "me uses me");
}

#[test]
fn missing_input_is_an_error_not_a_crash() {
    let (_d, st) = store();
    let (fs, errs) = FormulaSet::from_defs(
        &st,
        &defs(&[
            ("broken", "nosuchfunc(User)"),
            ("uses_broken", r#"broken & "x""#),
            ("uses_gone", "upper(gone_col)"),
            ("works", "lower(User)"),
        ]),
    );
    let names: Vec<&str> = fs.items.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["works"]);
    let why = |n: &str| {
        errs.iter()
            .find(|(c, _)| c == n)
            .map(|(_, e)| e.msg.clone())
            .unwrap()
    };
    assert!(why("broken").contains("unknown function nosuchfunc"));
    assert_eq!(why("uses_broken"), "uses broken, which has an error");
    assert!(why("uses_gone").contains("unknown column gone_col"));
    let c = ChunkCache::new(Budget::new(32 << 20));
    let no = AtomicBool::new(false);
    let m = filter_with(&st, &parse(r"works=corp\u0").unwrap(), &fs, &c, &no).unwrap();
    assert_eq!(m.count(), 1);
}

#[test]
fn a_name_that_is_a_stored_column_reads_the_stored_column() {
    let (_d, st) = store();
    // "user" clashes with the stored User: refused itself, but naming it reads User
    let (fs, errs) = FormulaSet::from_defs(
        &st,
        &defs(&[("user", "upper(Cmd)"), ("lbl", r#"user & "!""#)]),
    );
    let names: Vec<&str> = fs.items.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["lbl"], "{errs:?}");
    assert!(errs[0].1.msg.contains("already exists"));
    let c = ChunkCache::new(Budget::new(32 << 20));
    let ns = st.schema().len();
    let all = logseek_core::query::exec::MatchSet::all(&st);
    let p = page_with(&st, &all, 0, 1, &[ns], &fs, &c).unwrap();
    assert_eq!(p.cells[0][0].as_deref(), Some(r"CORP\u0!"));
}

#[test]
fn an_error_is_not_empty() {
    let (_d, st) = store();
    let mut fs = FormulaSet::default();
    fs.add(&st, "bad", "num(User)").unwrap();
    fs.add(&st, "none", r#"if(false, "x", "")"#).unwrap();
    let c = ChunkCache::new(Budget::new(32 << 20));
    let no = AtomicBool::new(false);
    let count = |q: &str| {
        filter_with(&st, &parse(q).unwrap(), &fs, &c, &no)
            .unwrap()
            .count()
    };
    // "has a value" (what NOT EMPTY writes) keeps error rows; EMPTY leaves them out
    assert_eq!(count(r#"bad:"*""#), 3000);
    assert_eq!(count(r#"NOT bad:"*""#), 0);
    // a truly empty made column is still empty
    assert_eq!(count(r#"NOT none:"*""#), 3000);
}

#[test]
fn iserror_selects_the_rows_that_could_not_be_worked_out() {
    let (_d, st) = store();
    let mut fs = FormulaSet::default();
    fs.add(&st, "bad", "num(User)").unwrap();
    fs.add(&st, "kb", "Bytes / 1024").unwrap();
    fs.add(&st, "some", r#"if(Bytes > 100, num(User), 1)"#)
        .unwrap();
    let c = ChunkCache::new(Budget::new(32 << 20));
    let no = AtomicBool::new(false);
    let count = |q: &str| {
        filter_with(&st, &parse(q).unwrap(), &fs, &c, &no)
            .unwrap()
            .count()
    };
    assert_eq!(count("iserror(bad)"), 3000);
    assert_eq!(count("iserror(kb)"), 0);
    assert_eq!(count("NOT iserror(bad)"), 0);
    // Bytes = i * 100: rows 0 and 1 are not over 100
    assert_eq!(count("iserror(some)"), 2998);
    assert_eq!(count("iserror(some) domainless"), 0);
    // a stored column is never an error
    assert_eq!(count("iserror(User)"), 0);
    // written back as it was typed
    assert_eq!(
        logseek_core::query::to_string(&parse("iserror(bad) AND kb>1").unwrap()),
        "iserror(bad) kb>1"
    );
}

#[test]
fn made_dependents_lists_every_column_that_uses_one() {
    use logseek_core::query::exec::made_dependents;
    let d = defs(&[
        ("size_kb", "num(Bytes) / 1024"),
        ("size_mb", "size_kb / 1024"),
        ("big", r#"if(size_mb > 1, "big", "small")"#),
        ("label", r#"upper(User) & "!""#),
        ("both", r#"label & SIZE_KB"#),
    ]);
    // directly and through others, in the order they were made; names ignore case
    assert_eq!(made_dependents(&d, "size_kb"), ["size_mb", "big", "both"]);
    assert_eq!(made_dependents(&d, "label"), ["both"]);
    assert!(made_dependents(&d, "big").is_empty());
    // a cycle does not loop
    let c = defs(&[("x", r#"y & "a""#), ("y", r#"x & "b""#)]);
    assert_eq!(made_dependents(&c, "x"), ["y"]);
}
