//! Made columns: the formula language and its functions.
use logseek_core::formula::{compile, steps_to_formula, FValue, Step};
use logseek_core::types::ColType;

fn cols() -> Vec<(String, ColType)> {
    [
        ("CommandLine", ColType::Text),
        ("User", ColType::Text),
        ("Bytes", ColType::Int),
        ("Payload", ColType::Text),
        ("ts", ColType::Int),
        ("ip", ColType::Text),
        ("Time", ColType::Time),
        ("Dst Port", ColType::Int),
    ]
    .iter()
    .map(|(a, b)| (a.to_string(), *b))
    .collect()
}

fn row(i: usize) -> FValue {
    match i {
        0 => FValue::Text(r"powershell.exe -nop -enc SQBFAFgA -w hidden".into()),
        1 => FValue::Text(r"CORP\alice".into()),
        2 => FValue::Num(1_500_000.0),
        3 => FValue::Text(
            r#"{"process":{"parent":{"name":"winword.exe"}},"tags":["a","b","c"]}"#.into(),
        ),
        4 => FValue::Num(1_794_564_646_100.0),
        5 => FValue::Text("10.1.2.3".into()),
        6 => FValue::Time(1_794_564_646_000_000),
        7 => FValue::Num(443.0),
        _ => FValue::Null,
    }
}

fn eval(src: &str) -> FValue {
    let f = compile(src, &cols()).unwrap_or_else(|e| panic!("{src}: {e:?}"));
    f.eval(&row)
}

fn text(src: &str) -> String {
    match eval(src) {
        FValue::Text(s) => s,
        other => panic!("{src} → {other:?}"),
    }
}
fn num(src: &str) -> f64 {
    match eval(src) {
        FValue::Num(n) => n,
        other => panic!("{src} → {other:?}"),
    }
}

#[test]
fn arith_precedence() {
    assert_eq!(num("1 + 2 * 3"), 7.0);
    assert_eq!(num("(1 + 2) * 3"), 9.0);
    assert_eq!(num("-2 * 3 + 10 % 4"), -4.0);
    assert_eq!(num("Bytes / 1000"), 1500.0);
    assert_eq!(num("round(Bytes / 1048576, 2)"), 1.43);
    assert_eq!(eval("1 / 0"), FValue::Error("division by zero".into()));
}

#[test]
fn comparisons_and_logic() {
    assert!(matches!(
        eval("Bytes > 1e6 and `Dst Port` == 443"),
        FValue::Bool(true)
    ));
    assert!(matches!(
        eval("not (User == \"x\") or false"),
        FValue::Bool(true)
    ));
}

#[test]
fn string_concat_and_coalesce() {
    assert_eq!(text(r#"User & " → " & lower("ABC")"#), "CORP\\alice → abc");
    assert_eq!(
        text(r#"Missing ?? "default""#.replace("Missing", "json(Payload, \"nope\")").as_str()),
        "default"
    );
    assert_eq!(
        text(r#"coalesce(json(Payload, "nope"), User)"#),
        "CORP\\alice"
    );
}

#[test]
fn regex_group_extract() {
    assert_eq!(text(r#"regex(CommandLine, "-enc\s+(\S+)", 1)"#), "SQBFAFgA");
    assert_eq!(
        text(r#"regex(CommandLine, "(?i)POWERSHELL")"#),
        "powershell"
    );
    assert!(matches!(
        eval(r#"regex(CommandLine, "zzz(\d+)", 1)"#),
        FValue::Null
    ));
    assert_eq!(text(r#"regex_replace(User, "^.*\\\\", "")"#), "alice");
}

#[test]
fn split_index_and_text_functions() {
    assert_eq!(text(r#"split(User, "\\", 1)"#), "alice");
    assert_eq!(text(r#"split(User, "\\", -1)"#), "alice");
    assert_eq!(text("upper(left(User, 4))"), "CORP");
    assert_eq!(text("right(User, 5)"), "alice");
    assert_eq!(text("substr(User, 5, 3)"), "ali");
    assert_eq!(num("len(User)"), 10.0);
    assert!(matches!(
        eval(r#"contains(CommandLine, "HIDDEN")"#),
        FValue::Bool(true)
    ));
    assert!(matches!(
        eval(r#"startswith(User, "corp")"#),
        FValue::Bool(true)
    ));
    assert_eq!(text(r#"replace(User, "CORP", "ACME")"#), "ACME\\alice");
    assert_eq!(text(r#"trim("  x  ")"#), "x");
}

#[test]
fn json_path_on_text() {
    assert_eq!(
        text(r#"json(Payload, "process.parent.name")"#),
        "winword.exe"
    );
    assert_eq!(num(r#"json_len(Payload, "tags")"#), 3.0);
    assert_eq!(text(r#"json(Payload, "tags[1]")"#), "b");
}

#[test]
fn date_functions() {
    // epoch milliseconds column → time
    let t = eval(r#"date(ts, "ms")"#);
    assert_eq!(t, FValue::Time(1_794_564_646_100_000));
    assert_eq!(
        text(r#"strftime(date(ts, "ms"), "%Y-%m-%d %H:%M")"#).len(),
        16
    );
    let ft = (1_794_564_646i64 + 11_644_473_600) * 10_000_000;
    assert_eq!(
        eval(&format!(r#"date("{ft}", "filetime")"#)),
        FValue::Time(1_794_564_646_000_000)
    );
    assert_eq!(
        eval(r#"date("2026-11-01T00:10:00Z")"#),
        FValue::Time(1_793_491_800_000_000)
    );
    assert_eq!(
        num("hour(Time)") as i64,
        ((1_794_564_646i64 % 86_400) / 3600)
    );
    assert_eq!(
        eval(r#"bucket(Time, "1h")"#),
        FValue::Time(1_794_564_646_000_000 / 3_600_000_000 * 3_600_000_000)
    );
}

#[test]
fn network_and_encoding() {
    assert!(matches!(
        eval(r#"cidr(ip, "10.0.0.0/8")"#),
        FValue::Bool(true)
    ));
    assert!(matches!(
        eval(r#"cidr(ip, "192.168.0.0/16")"#),
        FValue::Bool(false)
    ));
    assert!(matches!(eval("is_private_ip(ip)"), FValue::Bool(true)));
    assert!(matches!(
        eval(r#"is_private_ip("8.8.8.8")"#),
        FValue::Bool(false)
    ));
    assert_eq!(text(r#"base64_decode("aGVsbG8gd29ybGQ=")"#), "hello world");
    // Encoded command lines are often UTF-16LE: lossy decode keeps readable text
    assert_eq!(text(r#"base64_decode("SQBFAFgA", "utf16")"#), "IEX");
    assert_eq!(text(r#"hex_decode("414243")"#), "ABC");
    assert_eq!(
        text(r#"url_host("https://evil.example.com:8443/a/b?x=1")"#),
        "evil.example.com"
    );
    assert_eq!(
        text(r#"url_path("https://evil.example.com:8443/a/b?x=1")"#),
        "/a/b"
    );
    assert_eq!(
        text(r#"path_file("C:\Windows\System32\cmd.exe")"#),
        "cmd.exe"
    );
    assert_eq!(text(r#"path_ext("C:\Windows\System32\cmd.exe")"#), "exe");
    assert!(num(r#"entropy("aaaaaaaa")"#) < 0.01);
    assert!(num(r#"entropy("Xk9#qL2!vB7$")"#) > 3.0);
}

#[test]
fn if_case() {
    assert_eq!(text(r#"if(Bytes > 1e6, "big", "small")"#), "big");
    assert_eq!(
        text(r#"case(`Dst Port` == 80, "http", `Dst Port` == 443, "https", "other")"#),
        "https"
    );
    assert!(matches!(
        eval("isnull(json(Payload, \"zz\"))"),
        FValue::Bool(true)
    ));
}

#[test]
fn errors_have_spans_and_suggestions() {
    let e = compile("lowr(User)", &cols()).unwrap_err();
    assert_eq!(e.span, (0, 4));
    assert!(e.msg.contains("lower"), "{}", e.msg);
    let e = compile("upper(Usr)", &cols()).unwrap_err();
    assert!(e.msg.contains("User"), "{}", e.msg);
    let e = compile("1 +", &cols()).unwrap_err();
    assert!(e.span.0 <= 3);
    let e = compile(r#"regex(User, "(unclosed")"#, &cols()).unwrap_err();
    assert!(e.msg.to_lowercase().contains("regex"), "{}", e.msg);
}

#[test]
fn result_types() {
    assert_eq!(
        compile("Bytes / 2", &cols()).unwrap().result_type(),
        ColType::Float
    );
    assert_eq!(
        compile("lower(User)", &cols()).unwrap().result_type(),
        ColType::Text
    );
    assert_eq!(
        compile(r#"date(ts, "ms")"#, &cols()).unwrap().result_type(),
        ColType::Time
    );
    assert_eq!(
        compile("Bytes > 1", &cols()).unwrap().result_type(),
        ColType::Bool
    );
}

#[test]
fn steps_compile_to_formula_text() {
    let f = steps_to_formula(
        "CommandLine",
        &[
            Step::Regex {
                pattern: r"-enc\s+(\S+)".into(),
                group: 1,
            },
            Step::Base64 { utf16: true },
            Step::Lower,
        ],
    );
    assert_eq!(
        f,
        r#"lower(base64_decode(regex(CommandLine, "-enc\\s+(\\S+)", 1), "utf16"))"#
    );
    assert_eq!(text(&f), "iex");
    assert_eq!(
        steps_to_formula("Dst Port", &[Step::Trim]),
        "trim(`Dst Port`)"
    );
}

fn error(src: &str) -> String {
    match eval(src) {
        FValue::Error(s) => s,
        other => panic!("{src} gave {other:?}, not an error"),
    }
}

#[test]
fn failed_conversions_are_errors_with_reasons() {
    assert!(error(r#"num("abc")"#).contains("not a number: abc"));
    assert!(error(r#"int("12x")"#).contains("not a number: 12x"));
    assert_eq!(error("1/0"), "division by zero");
    assert_eq!(error("Bytes % 0"), "division by zero");
    assert!(error(r#"date("yesterday-ish")"#).contains("not a date: yesterday-ish"));
    assert!(error(r#"base64_decode("@@@")"#).contains("not base64"));
    assert!(error(r#"hex_decode("zz")"#).contains("not hex"));
    // an error is not an empty value
    assert_eq!(eval(r#"isnull(num("abc"))"#), FValue::Bool(false));
    assert_eq!(eval(r#"iserror(num("abc"))"#), FValue::Bool(true));
    assert_eq!(eval(r#"iserror(num("12"))"#), FValue::Bool(false));
    assert_eq!(eval(r#"iserror(num(""))"#), FValue::Bool(false));
    // only iferror and coalesce replace it
    assert_eq!(num(r#"iferror(num("abc"), 0)"#), 0.0);
    assert_eq!(num(r#"iferror(num("12"), 0)"#), 12.0);
    assert_eq!(num(r#"coalesce(num("abc"), 7)"#), 7.0);
    assert_eq!(num(r#"num("abc") ?? 7"#), 7.0);
    // everything else passes it on with its reason
    assert!(error(r#"round(num("abc") * 2, 1)"#).contains("not a number: abc"));
    assert!(error(r#"upper(hex_decode("zz"))"#).contains("not hex"));
    // an error never compares, a missing value still is missing
    assert_eq!(eval(r#"num("abc") > 5"#), FValue::Bool(false));
    assert_eq!(eval(r#"num("abc") != 5"#), FValue::Bool(false));
    assert_eq!(eval(r#"not (num("abc") > 5)"#), FValue::Bool(true));
    assert_eq!(eval(r#"num("")"#), FValue::Null);
}

#[test]
fn shared_input_evaluated_once_per_row() {
    use logseek_core::formula::compile_with;
    let shared = compile("upper(User)", &cols()).unwrap();
    let f = compile_with(
        "concat(m, \"-\", m, \"-\", lower(m))",
        &cols(),
        &[("m".to_string(), shared)],
    )
    .unwrap();
    let reads = std::cell::Cell::new(0);
    let get = |i: usize| {
        if i == 1 {
            reads.set(reads.get() + 1);
        }
        row(i)
    };
    assert_eq!(
        f.eval(&get),
        FValue::Text(r"CORP\ALICE-CORP\ALICE-corp\alice".into())
    );
    assert_eq!(
        reads.get(),
        1,
        "the shared column was worked out more than once"
    );
    // the fingerprint covers the formula and what it was made from
    let other = compile_with(
        "concat(m, \"-\", m, \"-\", lower(m))",
        &cols(),
        &[("m".to_string(), compile("lower(User)", &cols()).unwrap())],
    )
    .unwrap();
    assert_ne!(f.fingerprint(), other.fingerprint());
    assert_eq!(f.refs(), &[1]);
}

#[test]
fn not_and_or_pass_an_error_on() {
    assert!(error(r#"not num("abc")"#).contains("not a number: abc"));
    // a comparison with an error is false, so negating it is true
    assert_eq!(eval(r#"not (num("abc") > 5)"#), FValue::Bool(true));
    // the error itself, as a condition, is passed on
    assert!(error(r#"num("abc") and true"#).contains("not a number"));
    assert!(error(r#"true and num("abc")"#).contains("not a number"));
    // unless the side before it decided already
    assert_eq!(eval(r#"false and num("abc")"#), FValue::Bool(false));
    assert_eq!(eval(r#"true or num("abc")"#), FValue::Bool(true));
}

#[test]
fn the_fingerprint_follows_the_columns_names_land_on() {
    let a: Vec<(String, ColType)> = vec![("foo".into(), ColType::Text)];
    let b: Vec<(String, ColType)> =
        vec![("Foo".into(), ColType::Text), ("foo".into(), ColType::Text)];
    let fa = compile("upper(Foo)", &a).unwrap();
    let fb = compile("upper(Foo)", &b).unwrap();
    assert_ne!(fa.fingerprint(), fb.fingerprint());
    let c: Vec<(String, ColType)> = vec![("foo".into(), ColType::Int)];
    assert_ne!(
        fa.fingerprint(),
        compile("upper(Foo)", &c).unwrap().fingerprint()
    );
    assert_eq!(
        fa.fingerprint(),
        compile("upper(Foo)", &a).unwrap().fingerprint()
    );
}

#[test]
fn renaming_a_column_rewrites_only_its_references() {
    use logseek_core::formula::rename_refs;
    // every reference, whatever its case; a name with spaces is quoted
    assert_eq!(
        rename_refs("size_kb / 1024 + SIZE_KB", "size_kb", "size in kb"),
        "`size in kb` / 1024 + `size in kb`"
    );
    // a text that says the name, another column, a function: untouched
    assert_eq!(
        rename_refs(
            r#"concat("size_kb", size_kb_raw, upper(size_kb))"#,
            "size_kb",
            "kb"
        ),
        r#"concat("size_kb", size_kb_raw, upper(kb))"#
    );
    // a quoted reference is renamed too
    assert_eq!(rename_refs("`size kb` * 2", "size kb", "kb"), "kb * 2");
    // nothing to rename: the text as it was
    assert_eq!(rename_refs("lower(User)", "size_kb", "kb"), "lower(User)");
    // the result compiles against the new name
    let cols: Vec<(String, ColType)> = vec![("size in kb".into(), ColType::Float)];
    compile(
        &rename_refs("size_kb / 1024", "size_kb", "size in kb"),
        &cols,
    )
    .unwrap();
}

#[test]
fn a_spreadsheet_day_number_reads_as_a_time() {
    // days since 1899-12-30, the fraction is the time of day:
    // 46082.33359719907 is 2026-03-01 08:00:22.798
    assert_eq!(
        eval(r#"date("46082.33359719907", "days")"#),
        eval(r#"date("2026-03-01 08:00:22.798")"#)
    );
    // a whole number is midnight
    assert_eq!(eval(r#"date("25569", "days")"#), eval(r#"date("0", "s")"#));
}

#[test]
fn a_column_name_with_a_backtick_is_written_and_read_back_whole() {
    use logseek_core::formula::{backtick_name, column_ref};
    // a plain name needs no quoting; one with a space is quoted
    assert_eq!(column_ref("status"), "status");
    assert_eq!(column_ref("user name"), "`user name`");
    // a backtick in the name is doubled, and comes back as one
    let odd = "a` OR status=ok OR `b";
    let written = column_ref(odd);
    assert_eq!(written, "`a`` OR status=ok OR ``b`");
    assert_eq!(backtick_name(&written), Some((odd.to_string(), written.len())));
    // the name ends at its own closing backtick, not at one inside it
    assert_eq!(backtick_name("`x``y` rest"), Some(("x`y".to_string(), 6)));
    assert_eq!(backtick_name("`never closed"), None);
}
