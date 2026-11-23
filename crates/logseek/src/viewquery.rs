//! A whole view as one line of query text: the rows (search and filters),
//! then how they are sorted and which columns show:
//!
//! `host:web1 status>=500 | sort -bytes | fields time, host, "command line"`
//!
//! Copy it, send it to someone, paste it into the search box: the same rows
//! in the same order with the same columns.

/// A view read back from text.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct View {
    /// The rows part (what the engine runs).
    pub query: String,
    /// Sort column and whether largest comes first.
    pub sort: Option<(String, bool)>,
    /// The columns to show, in order.
    pub fields: Option<Vec<String>>,
}

fn plain(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '#' | '@' | '[' | ']'))
}

fn name_text(name: &str) -> String {
    if plain(name) {
        name.to_string()
    } else {
        logseek_core::formula::quote(name)
    }
}

/// The view as text. `fields` only when the columns on screen are not simply all of them.
pub fn write(query: &str, sort: Option<(&str, bool)>, fields: Option<&[String]>) -> String {
    let mut out = query.trim().to_string();
    let mut add = |s: String| {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&s);
    };
    if let Some((col, desc)) = sort {
        add(format!(
            "| sort {}{}",
            if desc { "-" } else { "" },
            name_text(col)
        ));
    }
    if let Some(f) = fields.filter(|f| !f.is_empty()) {
        let names: Vec<String> = f.iter().map(|n| name_text(n)).collect();
        add(format!("| fields {}", names.join(", ")));
    }
    out
}

/// The text cut at every `|` that is not inside quotes or backticks.
fn segments(q: &str) -> Vec<&str> {
    let (mut in_q, mut in_b, mut esc) = (false, false, false);
    let mut out = Vec::new();
    let mut start = 0;
    for (i, ch) in q.char_indices() {
        if esc {
            esc = false;
            continue;
        }
        match ch {
            '\\' if in_q => esc = true,
            '"' if !in_b => in_q = !in_q,
            '`' if !in_q => in_b = !in_b,
            '|' if !in_q && !in_b => {
                out.push(&q[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&q[start..]);
    out
}

/// A list of names separated by commas; a name may be in double quotes.
fn names(list: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut in_q, mut esc, mut quoted) = (false, false, false);
    let push = |cur: &mut String, quoted: &mut bool| {
        let n = if *quoted {
            cur.clone()
        } else {
            cur.trim().to_string()
        };
        cur.clear();
        *quoted = false;
        n
    };
    for ch in list.chars() {
        if esc {
            cur.push(ch);
            esc = false;
        } else if in_q {
            match ch {
                '\\' => esc = true,
                '"' => in_q = false,
                _ => cur.push(ch),
            }
        } else if ch == '"' && cur.trim().is_empty() {
            cur.clear();
            in_q = true;
            quoted = true;
        } else if ch == ',' {
            out.push(push(&mut cur, &mut quoted));
        } else if !(quoted && ch.is_whitespace()) {
            cur.push(ch);
        }
    }
    if in_q {
        return None;
    }
    out.push(push(&mut cur, &mut quoted));
    (!out.iter().any(String::is_empty)).then_some(out)
}

/// Take the `| sort` and `| fields` steps out of a query. `None` when it has
/// neither (or they do not read as a column list).
pub fn read(text: &str) -> Option<View> {
    let segs = segments(text);
    let mut v = View {
        query: segs[0].trim().to_string(),
        ..Default::default()
    };
    let mut found = false;
    for s in &segs[1..] {
        let t = s.trim();
        let (cmd, rest) = t.split_once(char::is_whitespace).unwrap_or((t, ""));
        match cmd.to_ascii_lowercase().as_str() {
            "sort" => {
                let mut n = names(rest)?;
                // one column sorts the grid
                if n.len() != 1 {
                    return None;
                }
                let n = n.remove(0);
                v.sort = Some(match n.strip_prefix('-') {
                    Some(r) if !rest.trim_start().starts_with('"') => (r.to_string(), true),
                    _ => (n.strip_prefix('+').unwrap_or(&n).to_string(), false),
                });
                found = true;
            }
            "fields" => {
                v.fields = Some(names(rest)?);
                found = true;
            }
            // any other step stays part of the query
            _ => {
                v.query.push_str(" | ");
                v.query.push_str(t);
            }
        }
    }
    found.then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_view_is_written_as_one_line_and_read_back_the_same() {
        let fields = vec![
            "time".to_string(),
            "host".to_string(),
            "command line".to_string(),
        ];
        let t = write(
            "host:web1 status>=500",
            Some(("bytes", true)),
            Some(&fields),
        );
        assert_eq!(
            t,
            r#"host:web1 status>=500 | sort -bytes | fields time, host, "command line""#
        );
        assert_eq!(
            read(&t).unwrap(),
            View {
                query: "host:web1 status>=500".into(),
                sort: Some(("bytes".into(), true)),
                fields: Some(fields),
            }
        );
    }

    #[test]
    fn nothing_to_add_leaves_the_query_as_it_is() {
        assert_eq!(write(" host:web1 ", None, None), "host:web1");
        assert_eq!(write("", Some(("a b", false)), None), r#"| sort "a b""#);
        assert_eq!(read("host:web1"), None);
        // a | inside quotes is text, not a step
        assert_eq!(read(r#"msg:"a | sort b""#), None);
    }

    #[test]
    fn other_steps_stay_in_the_query_and_odd_names_survive() {
        let v = read(r#"x | where bytes>5 | sort "-odd, name""#).unwrap();
        assert_eq!(v.query, "x | where bytes>5");
        // a quoted name keeps its leading dash: smallest first
        assert_eq!(v.sort, Some(("-odd, name".into(), false)));
        // half-typed lists are not a view yet
        assert_eq!(read("x | fields a,"), None);
        assert_eq!(read("x | sort a, b"), None);
        assert_eq!(read(r#"x | fields "a"#), None);
    }
}
