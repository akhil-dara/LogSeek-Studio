//! Extracting a part of a value from examples the user gives.
use logseek_core::extract::{
    suggest_extractions, suggest_key_values, suggest_split, Example, ValueKind,
};

fn ex(text: &str, part: &str) -> Example {
    let start = text
        .find(part)
        .unwrap_or_else(|| panic!("{part} not in {text}"));
    Example {
        text: text.into(),
        start,
        end: start + part.len(),
    }
}
fn ex_nth(text: &str, part: &str, nth: usize) -> Example {
    let start = text.match_indices(part).nth(nth).unwrap().0;
    Example {
        text: text.into(),
        start,
        end: start + part.len(),
    }
}
fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

#[test]
fn user_after_key() {
    let samples = s(&["user=carol action=x", "action=y user=dave", "no user here"]);
    let sug = suggest_extractions(&[ex("user=alice action=login", "alice")], &[], &samples);
    let best = &sug[0];
    assert!(best.description.contains("user="), "{}", best.description);
    assert_eq!(
        best.preview,
        vec![Some("carol".into()), Some("dave".into()), None]
    );
    assert!((best.coverage - 2.0 / 3.0).abs() < 1e-6);
}

#[test]
fn second_ip_needs_anchor() {
    let row = "src=10.0.0.1 dst=192.168.1.5 port=443";
    let samples = s(&[
        "src=10.9.9.9 dst=8.8.8.8 port=53",
        "src=1.1.1.1 dst=172.16.0.9 port=22",
    ]);
    let sug = suggest_extractions(&[ex(row, "192.168.1.5")], &[], &samples);
    let best = &sug[0];
    assert_eq!(best.kind, ValueKind::Ipv4);
    assert!(best.description.contains("dst="), "{}", best.description);
    assert_eq!(
        best.preview,
        vec![Some("8.8.8.8".into()), Some("172.16.0.9".into())]
    );
}

#[test]
fn until_delimiter_payload() {
    let row = r"powershell.exe -nop -enc SQBFAFgAIAAoAE4A -w hidden";
    let samples = s(&[r"powershell -enc ZQBjAGgAbwA= -w 1", "cmd.exe /c dir"]);
    let sug = suggest_extractions(&[ex(row, "SQBFAFgAIAAoAE4A")], &[], &samples);
    let best = &sug[0];
    assert!(best.description.contains("-enc"), "{}", best.description);
    assert_eq!(best.preview, vec![Some("ZQBjAGgAbwA=".into()), None]);
}

#[test]
fn guid_unanchored() {
    let row = "session 1b09bd39-56b0-4e78-bae4-ec38ff10a75b opened";
    let samples = s(&["id=c44379fb-9814-4aa4-97f9-8404001d78e6", "nothing"]);
    let sug = suggest_extractions(
        &[ex(row, "1b09bd39-56b0-4e78-bae4-ec38ff10a75b")],
        &[],
        &samples,
    );
    assert_eq!(sug[0].kind, ValueKind::Guid);
    assert_eq!(
        sug[0].preview[0].as_deref(),
        Some("c44379fb-9814-4aa4-97f9-8404001d78e6")
    );
}

#[test]
fn negative_example_forces_anchor() {
    let pos = ex("ERR 404 /index.html", "404");
    let neg = s(&["INFO 200 /ok"]);
    let sug = suggest_extractions(&[pos], &neg, &s(&["ERR 500 /x", "INFO 302 /y"]));
    let best = &sug[0];
    assert_eq!(best.preview, vec![Some("500".into()), None]);
    assert!(best.description.contains("ERR"), "{}", best.description);
}

#[test]
fn multiple_examples_must_all_match() {
    let a = ex(r"logon CORP\alice ok", r"CORP\alice");
    let b = ex(r"logoff LAB\bob.admin done", r"LAB\bob.admin");
    let sug = suggest_extractions(&[a, b], &[], &s(&[r"logon X\y.z ok"]));
    assert_eq!(sug[0].kind, ValueKind::DomainUser);
    assert_eq!(sug[0].preview[0].as_deref(), Some(r"X\y.z"));
}

#[test]
fn plain_words_description_and_formula_step() {
    let sug = suggest_extractions(&[ex("pid: 4242 started", "4242")], &[], &[]);
    let d = &sug[0].description;
    assert!(d.contains("number") && d.contains("pid:"), "{d}");
    let f = logseek_core::formula::steps_to_formula("Msg", &[sug[0].to_step()]);
    assert!(f.starts_with("regex(Msg, \""), "{f}");
    let compiled =
        logseek_core::formula::compile(&f, &[("Msg".into(), logseek_core::types::ColType::Text)])
            .unwrap();
    let v = compiled.eval(&|_| logseek_core::formula::FValue::Text("pid: 77 started".into()));
    assert_eq!(v, logseek_core::formula::FValue::Text("77".into()));
}

#[test]
fn key_value_detection() {
    let rows = s(&[
        "user=alice action=login src=10.0.0.1",
        "user=bob action=logout src=10.0.0.2 extra=1",
        "user=carol action=login src=10.0.0.3",
    ]);
    let kv = suggest_key_values(&rows);
    let keys: Vec<&str> = kv.iter().map(|k| k.key.as_str()).collect();
    assert!(keys.starts_with(&["user", "action", "src"]), "{keys:?}");
    assert!(kv.iter().find(|k| k.key == "extra").unwrap().coverage < 0.5);
    let re = regex::Regex::new(&kv[0].regex).unwrap();
    assert_eq!(&re.captures("x user=dave y").unwrap()[1], "dave");
}

#[test]
fn split_detection() {
    let rows = s(&["a;b;c;d", "1;2;3;4", "x;y;z;w"]);
    let sp = suggest_split(&rows).unwrap();
    assert_eq!(sp.delimiter, ';');
    assert_eq!(sp.fields, 4);
    assert!(suggest_split(&s(&["hello world", "plain"])).is_none());
}

#[test]
fn alternatives_are_offered() {
    let sug = suggest_extractions(&[ex("GET /api/v1/users 200 512ms", "200")], &[], &[]);
    assert!(sug.len() >= 2);
    let mut seen = std::collections::HashSet::new();
    assert!(
        sug.iter().all(|x| seen.insert(x.regex.clone())),
        "no duplicate suggestions"
    );
}

#[test]
fn second_occurrence_same_kind_example() {
    let row = "first 11 second 22 third 33";
    let sug = suggest_extractions(
        &[ex_nth(row, "22", 0)],
        &[],
        &s(&["first 1 second 2 third 3"]),
    );
    assert_eq!(sug[0].preview, vec![Some("2".into())]);
}
