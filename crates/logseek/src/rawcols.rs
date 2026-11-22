//! Columns from inside a structured cell (event XML in a raw-text column, JSON payloads):
//! which keys are there, and the formula that extracts each one. The formula
//! runs in the engine as a virtual column — the source is never rewritten.

use crate::inspect::{self, Kind};
use logseek_core::formula::{column_ref, quote};

fn re_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            o.push('\\');
        }
        o.push(c);
    }
    o
}

/// Decode the five XML entities around `inner` (a formula expression).
fn xml_unescape(inner: String) -> String {
    let mut e = inner;
    for (from, to) in [
        ("&quot;", "\""),
        ("&apos;", "'"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&amp;", "&"),
    ] {
        e = format!("replace({e}, {}, {})", quote(from), quote(to));
    }
    e
}

/// The formula that pulls `key` out of column `col`. For XML: `Tag.attr` is an
/// attribute; any other key matches an element labelled by its one attribute
/// (`<Data Name='key'>…`, `<f k='key'>…`) or `<key …>…`.
pub fn extract_formula(col: &str, kind: Kind, key: &str) -> String {
    let c = column_ref(col);
    match kind {
        Kind::Json => format!("json({c}, {})", quote(key)),
        Kind::Xml => {
            let pattern = match key.split_once('.') {
                Some((tag, attr)) => format!(
                    r#"<{}\b[^>]*\b{}=['"]([^'"]*)['"]"#,
                    re_escape(tag),
                    re_escape(attr)
                ),
                None => {
                    let k = re_escape(key);
                    // an element labelled by its one attribute (any tag, any
                    // attribute name), or an element named by the key
                    format!(r#"(?:<[\w:.-]+\s+[\w:.-]+=['"]{k}['"]\s*|<{k}(?:\s[^>]*)?)>([^<]*)<"#)
                }
            };
            xml_unescape(format!("regex({c}, {}, 1)", quote(&pattern)))
        }
    }
}

/// The fields inside a structured column, judged from the rows themselves.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggested {
    pub kind: Kind,
    /// Every field found: those worth a column first, then the rest; each
    /// group in the order rows have them (the most rows first).
    pub keys: Vec<String>,
    /// The fields one click makes columns: those that vary from row to row.
    /// Not a field with one value on every row that has it, nor an id (a
    /// long value different on nearly every row, with no spaces: free text
    /// such as a command line is kept). No names are looked for and no
    /// number is set: every field that varies is picked.
    pub pick: Vec<String>,
}

/// The fields of the samples of a structured column, if it is one.
pub fn suggest(samples: &[&str]) -> Option<Suggested> {
    let kind = samples.iter().find_map(|s| inspect::kind(s))?;
    // per field: rows with it, first position, and its values
    struct Seen {
        key: String,
        rows: usize,
        pos: usize,
        values: std::collections::HashMap<String, usize>,
    }
    let mut seen: Vec<Seen> = Vec::new();
    for s in samples.iter().filter(|s| inspect::kind(s) == Some(kind)) {
        let fields = match kind {
            Kind::Xml => inspect::xml_fields(s),
            Kind::Json => inspect::json_fields(s),
        };
        for (k, v) in fields {
            let at = match seen.iter().position(|e| e.key == k) {
                Some(i) => i,
                None => {
                    seen.push(Seen {
                        key: k.clone(),
                        rows: 0,
                        pos: seen.len(),
                        values: Default::default(),
                    });
                    seen.len() - 1
                }
            };
            let e = &mut seen[at];
            e.rows += 1;
            *e.values.entry(v).or_insert(0) += 1;
        }
    }
    if seen.is_empty() {
        return None;
    }
    let varies = |e: &Seen| -> bool {
        if e.values.len() < 2 {
            return false;
        }
        let unique = e.values.len() as f64 / e.rows as f64;
        let mut lens: Vec<usize> = e.values.keys().map(|v| v.chars().count()).collect();
        lens.sort_unstable();
        let median = lens[lens.len() / 2];
        let spaced = e.values.keys().filter(|v| v.contains(' ')).count() * 2 >= e.values.len();
        let id = e.rows >= 10 && unique > 0.9 && median >= 10 && !spaced;
        !id
    };
    seen.sort_by(|a, b| {
        varies(b)
            .cmp(&varies(a))
            .then(b.rows.cmp(&a.rows))
            .then(a.pos.cmp(&b.pos))
    });
    let pick = seen
        .iter()
        .filter(|e| varies(e))
        .map(|e| e.key.clone())
        .collect();
    Some(Suggested {
        kind,
        keys: seen.into_iter().map(|e| e.key).collect(),
        pick,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use logseek_core::formula::{compile, FValue};
    use logseek_core::types::ColType;

    const EVENT_XML: &str = "<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'><System><Provider Name='Example-Provider' Guid='{1A2B3C4D}'/><EventID>1</EventID><Version>5</Version><TimeCreated SystemTime='2016-08-24T18:27:39.1Z'/><Computer>host-a01</Computer></System><EventData><Data Name='Image'>C:\\Windows\\System32\\cmd.exe</Data><Data Name='CommandLine'>cmd /c &quot;whoami &amp; ipconfig&quot;</Data><Data Name='User'>CORP\\alice</Data></EventData></Event>";

    fn eval(formula: &str, col: &str, value: &str) -> FValue {
        let f = compile(formula, &[(col.to_string(), ColType::Text)])
            .unwrap_or_else(|e| panic!("{formula}: {}", e.msg));
        let v = value.to_string();
        f.eval(&|_| FValue::Text(v.clone()))
    }

    #[test]
    fn xml_keys_become_formulas() {
        let get = |k: &str| eval(&extract_formula("_raw", Kind::Xml, k), "_raw", EVENT_XML);
        assert_eq!(
            get("Image"),
            FValue::Text("C:\\Windows\\System32\\cmd.exe".into())
        );
        assert_eq!(
            get("CommandLine"),
            FValue::Text("cmd /c \"whoami & ipconfig\"".into()),
            "entities decoded"
        );
        assert_eq!(get("EventID"), FValue::Text("1".into()));
        assert_eq!(get("Computer"), FValue::Text("host-a01".into()));
        assert_eq!(
            get("TimeCreated.SystemTime"),
            FValue::Text("2016-08-24T18:27:39.1Z".into())
        );
        assert!(
            matches!(get("ParentImage"), FValue::Null),
            "absent key = no value"
        );
        // any tag labelled by its one attribute, whatever the attribute is called
        let g = "<rec><f k='host'>web1</f><f k='user'>alice</f></rec>";
        assert_eq!(
            eval(&extract_formula("x", Kind::Xml, "user"), "x", g),
            FValue::Text("alice".into())
        );
        let j = r#"{"user":{"name":"alice"},"n":5}"#;
        assert_eq!(
            eval(&extract_formula("p", Kind::Json, "user.name"), "p", j),
            FValue::Text("alice".into())
        );
    }

    /// An event-log-like record: a header that never changes, an id that always
    /// does, and the event's own fields.
    fn ev(i: usize) -> String {
        format!(
            "<Event><System><Provider Name='P'/><EventID>{}</EventID><Version>5</Version><Level>4</Level><Computer>host1</Computer></System><EventData><Data Name='ProcessGuid'>{{a3f{:08x}-77c2-4d1e-9b6e-0c5d{:08x}}}</Data><Data Name='Image'>C:/w/{}.exe</Data><Data Name='CommandLine'>{} /c step {}</Data></EventData></Event>",
            [1, 3, 1, 11][i % 4],
            i * 7919,
            i * 31,
            ["cmd", "net", "svchost"][i % 3],
            ["cmd", "net", "svchost"][i % 3],
            i
        )
    }

    #[test]
    fn the_fields_picked_are_the_ones_that_vary_judged_from_the_rows() {
        let rows: Vec<String> = (0..40).map(ev).collect();
        let samples: Vec<&str> = rows.iter().map(String::as_str).collect();
        let s = suggest(&samples).unwrap();
        assert_eq!(s.kind, Kind::Xml);
        // every field is listed for choosing
        for k in [
            "EventID",
            "Version",
            "Level",
            "Computer",
            "ProcessGuid",
            "Image",
            "CommandLine",
        ] {
            assert!(s.keys.iter().any(|x| x == k), "{k} listed");
        }
        // one click takes what varies, free text too
        for k in ["EventID", "Image", "CommandLine"] {
            assert!(s.pick.iter().any(|x| x == k), "{k} picked: {:?}", s.pick);
        }
        // the same on every row, or a different long id on every row: not picked
        for k in [
            "Version",
            "Level",
            "Computer",
            "Provider.Name",
            "ProcessGuid",
        ] {
            assert!(
                !s.pick.iter().any(|x| x == k),
                "{k} not picked: {:?}",
                s.pick
            );
        }
        // the picked come first in the list, in the order rows have them
        assert!(s.keys[..s.pick.len()].iter().all(|k| s.pick.contains(k)));
        assert!(suggest(&["plain", "text"]).is_none());
    }

    #[test]
    fn json_fields_are_judged_the_same_way() {
        let rows: Vec<String> = (0..30)
            .map(|i| {
                format!(
                    r#"{{"v":"1.0","user":"u{}","id":"9f1c{:012x}","msg":"login from {}"}}"#,
                    i % 4,
                    i * 104729,
                    i
                )
            })
            .collect();
        let samples: Vec<&str> = rows.iter().map(String::as_str).collect();
        let s = suggest(&samples).unwrap();
        assert_eq!(s.kind, Kind::Json);
        assert!(s.pick.contains(&"user".to_string()) && s.pick.contains(&"msg".to_string()));
        assert!(!s.pick.contains(&"v".to_string()) && !s.pick.contains(&"id".to_string()));
    }
}
