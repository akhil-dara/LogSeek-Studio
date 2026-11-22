//! Extracting values from JSON text by path.
use logseek_core::jsonpath::{extract, extract_all};

const DOC: &str = r#"{"a":{"b":1,"c":[{"t":"x"},{"t":"y"},{"u":1}]},"event.type[0]":"start","s":"txt","n":null,"tags":["p","q"]}"#;

#[test]
fn jsonpath_dot() {
    assert_eq!(extract(DOC, "a.b").as_deref(), Some("1"));
}
#[test]
fn jsonpath_index() {
    assert_eq!(extract(DOC, "a.c[1].t").as_deref(), Some("y"));
}
#[test]
fn jsonpath_wildcard_any() {
    assert_eq!(
        extract_all(DOC, "a.c[*].t"),
        vec!["x".to_string(), "y".to_string()]
    );
    assert_eq!(extract(DOC, "a.c[*].t").as_deref(), Some(r#"["x","y"]"#));
}
#[test]
fn jsonpath_len() {
    assert_eq!(extract(DOC, "len(a.c)").as_deref(), Some("3"));
    assert_eq!(extract(DOC, "len(tags)").as_deref(), Some("2"));
}
#[test]
fn jsonpath_missing_is_null() {
    assert_eq!(extract(DOC, "a.zz"), None);
    assert_eq!(extract(DOC, "n"), None);
    assert_eq!(extract(DOC, "a.c[9].t"), None);
    assert_eq!(extract("not json", "a"), None);
}
#[test]
fn jsonpath_literal_dotted_key_first() {
    assert_eq!(extract(DOC, "event.type[0]").as_deref(), Some("start"));
}
#[test]
fn jsonpath_object_value_is_compact_json() {
    assert_eq!(extract(DOC, "a.c[0]").as_deref(), Some(r#"{"t":"x"}"#));
}
#[test]
fn jsonpath_root_array() {
    assert_eq!(extract(r#"[{"k":5}]"#, "[0].k").as_deref(), Some("5"));
}
