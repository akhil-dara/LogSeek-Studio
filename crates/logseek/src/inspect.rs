//! Reading a value properly: XML (such as a Windows event) and JSON
//! held inside one cell are unpacked into named fields, and either can be
//! pretty-printed. Tolerant by design — malformed input yields what it can.

/// What a cell holds, if it is structured.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Kind {
    Xml,
    Json,
}

pub fn kind(v: &str) -> Option<Kind> {
    let t = v.trim_start();
    if t.starts_with('<') && t.contains("</") {
        Some(Kind::Xml)
    } else if (t.starts_with('{') || t.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(t).is_ok()
    {
        Some(Kind::Json)
    } else {
        None
    }
}

fn unescape_xml(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// `key="v"` / `key='v'` pairs of a tag's attribute text.
fn attrs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let k0 = i;
        while i < b.len() && b[i] != b'=' && !b[i].is_ascii_whitespace() {
            i += 1;
        }
        let key = &s[k0..i];
        while i < b.len() && (b[i] == b'=' || b[i].is_ascii_whitespace()) {
            i += 1;
        }
        if i >= b.len() || !(b[i] == b'"' || b[i] == b'\'') {
            break;
        }
        let q = b[i];
        i += 1;
        let v0 = i;
        while i < b.len() && b[i] != q {
            i += 1;
        }
        if !key.is_empty() && !key.starts_with("xmlns") {
            out.push((key.to_string(), unescape_xml(&s[v0..i.min(s.len())])));
        }
        i += 1;
    }
    out
}

/// Flatten XML into (name, value) pairs. Sibling elements of one tag, each
/// labelled by its one attribute (`<Data Name='Image'>…</Data>`, `<f k='host'>`),
/// are named by that attribute's value, whatever it is called; other leaves by
/// their tag; attributes become `Tag.attr` (e.g. `TimeCreated.SystemTime`).
pub fn xml_fields(x: &str) -> Vec<(String, String)> {
    let (mut named, rest) = xml_fields_split(x);
    named.extend(rest);
    named
}

/// Tags whose elements are labels: an element with text and exactly one
/// attribute, its tag repeated with that same attribute holding different
/// values (tag → the attribute).
fn label_tags(x: &str) -> std::collections::HashMap<String, String> {
    let mut seen: std::collections::HashMap<String, (String, std::collections::HashSet<String>)> =
        Default::default();
    let mut odd: std::collections::HashSet<String> = Default::default();
    let mut i = 0;
    while let Some(s) = x[i..].find('<').map(|k| i + k) {
        let Some(end) = x[s..].find('>').map(|e| s + e) else {
            break;
        };
        let inner = &x[s + 1..end];
        i = end + 1;
        if inner.starts_with(['?', '!', '/']) || inner.ends_with('/') {
            continue;
        }
        let (tag, rest) = inner.split_once(char::is_whitespace).unwrap_or((inner, ""));
        let a = attrs(rest);
        match a.as_slice() {
            [(k, v)] if !v.is_empty() => {
                let e = seen
                    .entry(tag.to_string())
                    .or_insert_with(|| (k.clone(), Default::default()));
                if &e.0 == k {
                    e.1.insert(v.clone());
                } else {
                    odd.insert(tag.to_string());
                }
            }
            _ => {
                odd.insert(tag.to_string());
            }
        }
    }
    seen.into_iter()
        .filter(|(t, (_, vals))| vals.len() >= 2 && !odd.contains(t))
        .map(|(t, (k, _))| (t, k))
        .collect()
}

/// (elements labelled by their one attribute — the record's own data, first —,
/// everything else: header elements and attributes).
fn xml_fields_split(x: &str) -> (Vec<(String, String)>, Vec<(String, String)>) {
    let labels = label_tags(x);
    let mut named: Vec<(String, String)> = Vec::new();
    let mut out: Vec<(String, String)> = Vec::new();
    let mut stack: Vec<(String, Option<String>)> = Vec::new(); // (tag, Name attr)
    let mut i = 0;
    let b = x.as_bytes();
    let mut text = String::new();
    while i < b.len() {
        if b[i] == b'<' {
            let Some(end) = x[i..].find('>').map(|e| i + e) else {
                break;
            };
            let inner = &x[i + 1..end];
            if inner.starts_with('?') || inner.starts_with('!') {
                i = end + 1;
                continue;
            }
            if let Some(close) = inner.strip_prefix('/') {
                let tag = close.trim();
                if let Some((t, name)) = stack.pop() {
                    let val = unescape_xml(text.trim());
                    if !val.is_empty() && t == tag {
                        match name {
                            Some(n) => named.push((n, val)),
                            None => out.push((t, val)),
                        }
                    }
                }
                text.clear();
            } else {
                let self_close = inner.ends_with('/');
                let body = inner.trim_end_matches('/');
                let (tag, rest) = body.split_once(char::is_whitespace).unwrap_or((body, ""));
                let a = attrs(rest);
                // a label names the element's text; with no text to come
                // (self-closing) it is just an attribute like any other
                let label = labels.get(tag).filter(|_| !self_close);
                let name =
                    label.and_then(|l| a.iter().find(|(k, _)| k == l).map(|(_, v)| v.clone()));
                for (k, v) in a.iter().filter(|(k, _)| label != Some(k)) {
                    out.push((format!("{tag}.{k}"), v.clone()));
                }
                if !self_close {
                    stack.push((tag.to_string(), name));
                }
                text.clear();
            }
            i = end + 1;
        } else {
            let next = x[i..].find('<').map(|n| i + n).unwrap_or(b.len());
            text.push_str(&x[i..next]);
            i = next;
        }
    }
    (named, out)
}

/// Flatten JSON into dotted paths (`a.b[0].c`) → scalar text.
pub fn json_fields(j: &str) -> Vec<(String, String)> {
    fn walk(v: &serde_json::Value, path: String, out: &mut Vec<(String, String)>) {
        match v {
            serde_json::Value::Object(m) => {
                for (k, x) in m {
                    walk(
                        x,
                        if path.is_empty() {
                            k.clone()
                        } else {
                            format!("{path}.{k}")
                        },
                        out,
                    );
                }
            }
            serde_json::Value::Array(a) => {
                for (i, x) in a.iter().enumerate() {
                    walk(x, format!("{path}[{i}]"), out);
                }
            }
            serde_json::Value::String(s) => out.push((path, s.clone())),
            serde_json::Value::Null => out.push((path, "null".into())),
            other => out.push((path, other.to_string())),
        }
    }
    let mut out = Vec::new();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(j.trim()) {
        walk(&v, String::new(), &mut out);
    }
    out
}

/// Indented XML, one element per line.
pub fn pretty_xml(x: &str) -> String {
    let mut out = String::with_capacity(x.len() + x.len() / 4);
    let mut depth: usize = 0;
    let mut i = 0;
    let b = x.as_bytes();
    while i < b.len() {
        if b[i] == b'<' {
            let Some(end) = x[i..].find('>').map(|e| i + e) else {
                out.push_str(&x[i..]);
                break;
            };
            let tag = &x[i..=end];
            let closing = tag.starts_with("</");
            let self_close = tag.ends_with("/>") || tag.starts_with("<?") || tag.starts_with("<!");
            if closing {
                depth = depth.saturating_sub(1);
            }
            // keep `<a>text</a>` on one line
            let prev_was_text = !out.is_empty() && !out.ends_with('\n') && !out.ends_with('>');
            if !(closing && prev_was_text) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&"  ".repeat(depth));
            }
            out.push_str(tag);
            if !closing && !self_close {
                depth += 1;
            }
            i = end + 1;
        } else {
            let next = x[i..].find('<').map(|n| i + n).unwrap_or(b.len());
            let t = x[i..next].trim();
            if !t.is_empty() {
                out.push_str(t);
            }
            i = next;
        }
    }
    out
}

pub fn pretty_json(j: &str) -> String {
    serde_json::from_str::<serde_json::Value>(j.trim())
        .and_then(|v| serde_json::to_string_pretty(&v))
        .unwrap_or_else(|_| j.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENT_XML: &str = "<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'><System><Provider Name='Example-Provider' Guid='{1A2B3C4D}'/><EventID>1</EventID><TimeCreated SystemTime='2016-08-24T18:27:39.1Z'/><Computer>host-a01.example.local</Computer></System><EventData><Data Name='Image'>C:\\Windows\\System32\\cmd.exe</Data><Data Name='CommandLine'>cmd /c &quot;whoami &amp; ipconfig&quot;</Data><Data Name='ParentImage'>C:\\Windows\\explorer.exe</Data></EventData></Event>";

    #[test]
    fn sibling_elements_labelled_by_their_one_attribute_are_fields_whatever_it_is_called() {
        let x = "<rec><meta when='now'/><f k='host'>web1</f><f k='user'>alice</f><n>7</n></rec>";
        let f = xml_fields(x);
        let get = |k: &str| f.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("host"), Some("web1"));
        assert_eq!(get("user"), Some("alice"));
        assert_eq!(get("n"), Some("7"));
        assert_eq!(get("meta.when"), Some("now"));
        // a lone element with one attribute is no label: its tag names it
        let y = "<rec><EventID Qualifiers='0'>4624</EventID></rec>";
        let g = xml_fields(y);
        assert!(
            g.iter().any(|(n, v)| n == "EventID" && v == "4624"),
            "{g:?}"
        );
    }

    #[test]
    fn event_xml_becomes_named_fields() {
        let f = xml_fields(EVENT_XML);
        let get = |k: &str| f.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str());
        assert_eq!(get("EventID"), Some("1"));
        assert_eq!(get("Image"), Some("C:\\Windows\\System32\\cmd.exe"));
        assert_eq!(get("CommandLine"), Some("cmd /c \"whoami & ipconfig\""));
        assert_eq!(get("ParentImage"), Some("C:\\Windows\\explorer.exe"));
        assert_eq!(
            get("TimeCreated.SystemTime"),
            Some("2016-08-24T18:27:39.1Z")
        );
        assert_eq!(get("Provider.Name"), Some("Example-Provider"));
        assert_eq!(get("Computer"), Some("host-a01.example.local"));
        assert_eq!(kind(EVENT_XML), Some(Kind::Xml));
        // the event's own data comes first, before the System header
        assert_eq!(f[0].0, "Image");
    }

    #[test]
    fn json_cells_flatten_to_paths() {
        let j = r#"{"user":{"name":"alice","groups":["a","b"]},"ok":true,"n":1e5,"x":null}"#;
        assert_eq!(kind(j), Some(Kind::Json));
        let f = json_fields(j);
        assert!(f.contains(&("user.name".into(), "alice".into())));
        assert!(f.contains(&("user.groups[1]".into(), "b".into())));
        assert!(f.contains(&("ok".into(), "true".into())));
        assert!(f.contains(&("x".into(), "null".into())));
        assert_eq!(kind("plain text {not json"), None);
        assert_eq!(kind("a < b"), None);
    }

    #[test]
    fn pretty_printing_indents_and_keeps_leaves_on_one_line() {
        let p = pretty_xml("<a><b>1</b><c x='y'/><d><e>t</e></d></a>");
        assert_eq!(
            p,
            "<a>\n  <b>1</b>\n  <c x='y'/>\n  <d>\n    <e>t</e>\n  </d>\n</a>"
        );
        assert!(pretty_json(r#"{"a":1}"#).contains("\n  \"a\": 1"));
        // broken XML never panics
        let _ = xml_fields("<a><b>unclosed");
        let _ = pretty_xml("<a><b");
    }
}

/// A row's fields as the row view reads them: the ones pinned first (in pin
/// order, empty or not), then those shown in the grid (in its order), then the
/// rest that have a value (in the file's order); the empty ones apart.
pub struct Arranged<'a> {
    pub pinned: Vec<&'a (String, String)>,
    pub in_grid: Vec<&'a (String, String)>,
    pub also: Vec<&'a (String, String)>,
    /// Every column the row has no value for, in the file's order.
    pub empty: Vec<String>,
}

impl Arranged<'_> {
    /// Fields with a value.
    pub fn filled(&self) -> usize {
        self.pinned
            .iter()
            .filter(|f| !f.1.trim().is_empty())
            .count()
            + self.in_grid.len()
            + self.also.len()
    }
}

pub fn arrange<'a>(
    fields: &'a [(String, String)],
    shown: &[String],
    pins: &[String],
    all: &[String],
) -> Arranged<'a> {
    let has = |f: &(String, String)| !f.1.trim().is_empty();
    let pinned: Vec<&(String, String)> = pins
        .iter()
        .filter_map(|p| fields.iter().find(|f| &f.0 == p))
        .collect();
    let is_pinned = |k: &str| pins.iter().any(|p| p == k);
    let in_grid: Vec<&(String, String)> = shown
        .iter()
        .filter(|n| !is_pinned(n))
        .filter_map(|n| fields.iter().find(|f| f.0 == **n && has(f)))
        .collect();
    let also: Vec<&(String, String)> = fields
        .iter()
        .filter(|f| has(f) && !is_pinned(&f.0) && !shown.contains(&f.0))
        .collect();
    let empty: Vec<String> = all
        .iter()
        .filter(|k| !is_pinned(k))
        .filter(|k| fields.iter().find(|f| &f.0 == *k).is_none_or(|f| !has(f)))
        .cloned()
        .collect();
    Arranged {
        pinned,
        in_grid,
        also,
        empty,
    }
}

#[cfg(test)]
mod arrange_tests {
    use super::*;

    fn f(k: &str, v: &str) -> (String, String) {
        (k.to_string(), v.to_string())
    }

    #[test]
    fn a_row_reads_pinned_then_the_grid_then_the_rest_and_empties_apart() {
        let fields = vec![
            f("time", "10:00"),
            f("user", "alice"),
            f("blank", ""),
            f("host", "web1"),
            f("space", "   "),
            f("pid", "42"),
            f("note", "hello"),
        ];
        let shown = vec!["host".to_string(), "time".to_string(), "blank".to_string()];
        let pins = vec!["pid".to_string(), "blank".to_string(), "gone".to_string()];
        // every column of the file: one the row has no value for is empty too
        let all: Vec<String> = [
            "time", "user", "blank", "host", "space", "pid", "note", "absent",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let a = arrange(&fields, &shown, &pins, &all);
        fn names<'b>(v: &[&'b (String, String)]) -> Vec<&'b str> {
            v.iter().map(|x| x.0.as_str()).collect()
        }
        // pinned in pin order, an empty one included, one not in this row left out
        assert_eq!(names(&a.pinned), ["pid", "blank"]);
        // the grid's order, values only, nothing pinned twice
        assert_eq!(names(&a.in_grid), ["host", "time"]);
        // the rest with a value, in the file's order
        assert_eq!(names(&a.also), ["user", "note"]);
        // empty ones (blanks count as empty), not pinned
        assert_eq!(a.empty, ["space", "absent"]);
        assert_eq!(a.filled(), 5);
    }
}
