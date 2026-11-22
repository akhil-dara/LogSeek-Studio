//! Paths into JSON text: `a.b`, `a.b[0]`, `a.b[*].c` (any element),
//! `len(a.b)`. Object keys that themselves contain dots or brackets (e.g.
//! `event.type[0]`) match literally first — the longest key that exists wins.

use serde_json::Value;

/// Text of the value at `path`, or `None` when missing/null/not JSON.
/// Wildcard paths return a compact JSON array of all matches.
pub fn extract(json: &str, path: &str) -> Option<String> {
    let v: Value = serde_json::from_str(json).ok()?;
    let path = path.trim();
    if let Some(inner) = path.strip_prefix("len(").and_then(|p| p.strip_suffix(')')) {
        let hits = resolve(&v, inner.trim());
        return match hits.as_slice() {
            [Value::Array(a)] => Some(a.len().to_string()),
            [Value::Object(o)] => Some(o.len().to_string()),
            [Value::String(s)] => Some(s.chars().count().to_string()),
            _ => None,
        };
    }
    let hits: Vec<&Value> = resolve(&v, path)
        .into_iter()
        .filter(|v| !v.is_null())
        .collect();
    if path.contains("[*]") {
        if hits.is_empty() {
            return None;
        }
        return serde_json::to_string(&hits).ok();
    }
    hits.first().map(|v| text(v))
}

/// Text of every match (wildcards expanded), nulls skipped.
pub fn extract_all(json: &str, path: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    resolve(&v, path.trim())
        .into_iter()
        .filter(|v| !v.is_null())
        .map(text)
        .collect()
}

pub fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn resolve<'a>(v: &'a Value, rest: &str) -> Vec<&'a Value> {
    if rest.is_empty() {
        return vec![v];
    }
    if let Some(after) = rest.strip_prefix('[') {
        let Some(close) = after.find(']') else {
            return Vec::new();
        };
        let idx = &after[..close];
        let mut next = &after[close + 1..];
        if let Some(n) = next.strip_prefix('.') {
            next = n;
        }
        let Value::Array(a) = v else {
            return Vec::new();
        };
        if idx == "*" {
            return a.iter().flat_map(|x| resolve(x, next)).collect();
        }
        return match idx.trim().parse::<usize>() {
            Ok(i) => a.get(i).map(|x| resolve(x, next)).unwrap_or_default(),
            Err(_) => Vec::new(),
        };
    }
    let Value::Object(o) = v else {
        return Vec::new();
    };
    // Candidate key ends: every '.' or '[' boundary, plus the end. Longest first.
    let mut ends: Vec<usize> = rest
        .char_indices()
        .filter(|(_, c)| *c == '.' || *c == '[')
        .map(|(i, _)| i)
        .filter(|&i| i > 0)
        .collect();
    ends.push(rest.len());
    for &e in ends.iter().rev() {
        if let Some(child) = o.get(&rest[..e]) {
            let mut next = &rest[e..];
            if let Some(n) = next.strip_prefix('.') {
                next = n;
            }
            let hits = resolve(child, next);
            if !hits.is_empty() {
                return hits;
            }
        }
    }
    Vec::new()
}
