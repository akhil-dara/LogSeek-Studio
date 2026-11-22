//! Schema inference from a batch of records.
use logseek_core::ingest::batch::RecordBatch;
use logseek_core::ingest::sample::{sample_file, SampleOptions};
use logseek_core::schema::{
    detect_json_columns, suggest_type_key, Hint, SchemaNode, T_ARRAY, T_INT, T_STRING,
};
use serde_json::{json, Value};
use std::io::Write;

fn node(docs: &[Value]) -> SchemaNode {
    let mut n = SchemaNode::default();
    for d in docs {
        n.add(d);
    }
    n
}

#[test]
fn merge_is_associative_and_commutative() {
    let a = vec![json!({"x":1,"y":{"z":"a"}}), json!({"x":"s"})];
    let b = vec![json!({"y":{"z":2,"w":[1,2]}}), json!({"q":true})];
    let c = vec![json!({"x":1.5,"y":null}), json!([1, {"k":"v"}])];
    let (na, nb, nc) = (node(&a), node(&b), node(&c));
    let mut left = na.clone();
    left.merge(&nb);
    left.merge(&nc);
    let mut bc = nb.clone();
    bc.merge(&nc);
    let mut right = na.clone();
    right.merge(&bc);
    assert_eq!(left, right);
    let mut ab = na.clone();
    ab.merge(&nb);
    let mut ba = nb.clone();
    ba.merge(&na);
    assert_eq!(ab, ba);
}

#[test]
fn presence_counts() {
    let n = node(&[
        json!({"a":1,"b":1}),
        json!({"a":1}),
        json!({"a":2,"b":3}),
        json!({"a":0,"b":null}),
    ]);
    let p = n.paths();
    let b = p.iter().find(|x| x.path == "b").unwrap();
    assert!((b.presence - 0.75).abs() < 1e-9, "{}", b.presence);
    let a = p.iter().find(|x| x.path == "a").unwrap();
    assert!((a.presence - 1.0).abs() < 1e-9);
    assert!(a.types.has(T_INT));
}

#[test]
fn array_items_merged_with_len_stats() {
    let n = node(&[json!({"t":[1,2]}), json!({"t":["a"]}), json!({"t":[]})]);
    let t = n.child("t").unwrap();
    assert!(t.types.has(T_ARRAY));
    let items = t.items.as_ref().unwrap();
    assert!(items.types.has(T_INT) && items.types.has(T_STRING));
    assert_eq!((t.arr_min, t.arr_max), (0, 2));
}

#[test]
fn array_of_objects_paths_use_brackets() {
    let n = node(&[json!({"events":[{"type":"a","n":1},{"type":"b"}]})]);
    let paths: Vec<String> = n.paths().into_iter().map(|p| p.path).collect();
    assert!(paths.contains(&"events".to_string()));
    assert!(paths.contains(&"events[*].type".to_string()));
    assert!(paths.contains(&"events[*].n".to_string()));
}

#[test]
fn map_like_overflow_after_4096_keys() {
    let mut obj = serde_json::Map::new();
    for i in 0..5_000 {
        obj.insert(format!("10.0.{}.{}", i / 256, i % 256), json!(i));
    }
    let n = node(&[json!({ "hosts": Value::Object(obj) })]);
    let h = n.child("hosts").unwrap();
    assert!(h.children.len() <= 4_096);
    assert_eq!(h.map_like_overflow, 5_000 - 4_096);
    assert!(h.is_map_like());
}

#[test]
fn hints_detect_formats() {
    let n = node(&[json!({
        "ip":"192.168.10.65", "ip6":"fe80::1c2a:3bff:fe4d:5e6f",
        "guid":"1b09bd39-56b0-4e78-bae4-ec38ff10a75b",
        "sha":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "url":"https://example.com/a?b=1",
        "path":"\\Device\\HarddiskVolume3\\Windows\\System32\\dllhost.exe",
        "when":"2026-11-01T00:10:00Z", "epoch":"1794564646100",
        "b64":"SGVsbG8gV29ybGQhIEhlbGxvIQ==", "mail":"a@b.co", "plain":"hello"
    })]);
    let h = |k: &str| n.child(k).unwrap().hints();
    assert!(h("ip").contains(&Hint::Ipv4));
    assert!(h("ip6").contains(&Hint::Ipv6));
    assert!(h("guid").contains(&Hint::Guid));
    assert!(h("sha").contains(&Hint::Hash));
    assert!(h("url").contains(&Hint::Url));
    assert!(h("path").contains(&Hint::Path));
    assert!(h("when").contains(&Hint::Timestamp));
    assert!(h("epoch").contains(&Hint::EpochMillis));
    assert!(h("b64").contains(&Hint::Base64));
    assert!(h("mail").contains(&Hint::Email));
    assert!(h("plain").is_empty());
}

#[test]
fn examples_are_bounded_and_deterministic() {
    let docs: Vec<Value> = (0..100)
        .map(|i| json!({ "v": format!("val{i}") }))
        .collect();
    let n1 = node(&docs);
    let mut rev = docs.clone();
    rev.reverse();
    let n2 = node(&rev);
    let e = &n1.child("v").unwrap().examples;
    assert_eq!(e.len(), 8);
    assert_eq!(e, &n2.child("v").unwrap().examples);
}

#[test]
fn csv_payload_column_detected_as_json() {
    let mut b = RecordBatch::new(3);
    for i in 0..50 {
        b.begin_row(i);
        b.set(0, "2026-01-01");
        b.set(1, &format!("{{\"user\":\"u{i}\",\"tags\":[1,2]}}"));
        b.set(2, if i % 2 == 0 { "[1]" } else { "plain" });
    }
    assert_eq!(detect_json_columns(&b), vec![1]);
}

#[test]
fn type_key_suggested_without_knowing_the_format() {
    let mut docs = Vec::new();
    for i in 0..300 {
        docs.push(match i % 3 {
            0 => json!({"kind":"ProcessStart","ts":i,"pid":i,"cmd":"x","image":"y"}),
            1 => json!({"kind":"NetConnect","ts":i,"src":"1.1.1.1","dst":"2.2.2.2","port":443}),
            _ => json!({"kind":"FileWrite","ts":i,"file":"f","size":10}),
        });
    }
    let s = suggest_type_key(&docs).expect("a type key");
    assert_eq!(s.key, "kind");
    assert_eq!(s.types.len(), 3);
    let flat: Vec<Value> = (0..300)
        .map(|i| {
            let lv = ["info", "warn"][i % 2];
            json!({"level": lv, "msg":"m", "ts": i})
        })
        .collect();
    assert!(suggest_type_key(&flat).is_none());
}

#[test]
fn sample_slices_realigned_and_find_late_keys() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("big.ndjson");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&p).unwrap());
    let total = 400_000u64;
    let pad = "x".repeat(80);
    for i in 0..total {
        if i > total * 99 / 100 {
            writeln!(f, "{{\"i\":{i},\"late_key\":true}}").unwrap();
        } else {
            writeln!(f, "{{\"i\":{i},\"pad\":\"{pad}\"}}").unwrap();
        }
    }
    drop(f);
    let s = sample_file(&p, &SampleOptions::default()).unwrap();
    assert!(!s.slices.is_empty());
    for (off, bytes) in &s.slices {
        assert!(bytes.starts_with(b"{"), "slice at {off} not aligned");
        assert!(bytes.ends_with(b"\n"));
    }
    let all: Vec<u8> = s.slices.iter().flat_map(|(_, b)| b.clone()).collect();
    assert!(all.windows(8).any(|w| w == b"late_key"));
}

#[test]
fn small_file_sample_is_whole_file() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("s.csv");
    std::fs::write(&p, b"a,b\n1,2\n").unwrap();
    let s = sample_file(&p, &SampleOptions::default()).unwrap();
    assert_eq!(s.head, b"a,b\n1,2\n");
    assert!(s.slices.is_empty());
}

#[test]
fn record_path_suggested_for_wrapper_documents() {
    use logseek_core::schema::suggest_record_path;
    assert_eq!(
        suggest_record_path(br#"{"Records":[{"a":1},{"a":2}]}"#).as_deref(),
        Some("Records")
    );
    assert_eq!(
        suggest_record_path(br#"{"meta":{"n":2},"items":[{"a":1}]}"#).as_deref(),
        Some("items")
    );
    assert_eq!(
        suggest_record_path(br#"{"tags":[1,2],"rows":[{"x":1}"#).as_deref(),
        Some("rows")
    );
    assert_eq!(suggest_record_path(br#"{"a":1}"#), None);
    assert_eq!(suggest_record_path(b"[{\"a\":1}]"), None);
}

#[test]
fn type_key_ignores_identifiers_and_nested_fields_count() {
    // unique ids would "explain" every field trivially; they must not win
    let mut docs = Vec::new();
    for i in 0..200 {
        let (name, params) = match i % 4 {
            0 => ("GetObject", json!({"bucketName":"b","key":"k"})),
            1 => ("ListBuckets", Value::Null),
            2 => ("RunInstances", json!({"instanceType":"t2","imageId":"ami"})),
            _ => ("AssumeRole", json!({"roleArn":"arn","roleSessionName":"s"})),
        };
        docs.push(json!({"eventID": format!("id-{i}"), "eventName": name, "requestParameters": params, "userAgent": format!("ua{}", i % 50)}));
    }
    let s = suggest_type_key(&docs).expect("suggestion");
    assert_eq!(s.key, "eventName");
}
