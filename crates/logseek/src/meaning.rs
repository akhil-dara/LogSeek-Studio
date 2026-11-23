//! "Say what the search means": the engine's own parse of a query, read back
//! in plain words, so a substring search can never pass for a comparison.

use logseek_core::query::ast::{CmpOp, Expr, Term};
use logseek_core::types::fmt_time;

fn term(t: &Term) -> String {
    match t {
        Term::FreeText(s) => format!("any field contains \"{s}\""),
        Term::IsError(col) => format!("{col} could not be worked out"),
        Term::Cmp { col, op, value } => {
            let v = if value.parse::<f64>().is_ok() {
                value.clone()
            } else {
                format!("\"{value}\"")
            };
            match op {
                CmpOp::Eq => format!("{col} is {v}"),
                CmpOp::Ne => format!("{col} is not {v}"),
                CmpOp::Gt => format!("{col} > {v}"),
                CmpOp::Ge => format!("{col} ≥ {v}"),
                CmpOp::Lt => format!("{col} < {v}"),
                CmpOp::Le => format!("{col} ≤ {v}"),
                CmpOp::Contains => format!("{col} contains {v}"),
                CmpOp::Glob => format!("{col} matches {v}"),
                CmpOp::Regex => {
                    let who = if col == logseek_core::query::ast::ANY_FIELD {
                        "any field"
                    } else {
                        col.as_str()
                    };
                    // a shape worked out from an example reads as its pieces
                    let (whole, body) =
                        match value.strip_prefix('^').and_then(|v| v.strip_suffix('$')) {
                            Some(b) => (true, b),
                            None => (false, value.as_str()),
                        };
                    if let Some(w) = crate::builder::shape_words(body) {
                        return if whole {
                            format!("{who} is shaped like: {w}")
                        } else {
                            format!("{who} contains something shaped like: {w}")
                        };
                    }
                    // a pattern the match choices wrote reads as those choices
                    match crate::matchopts::pattern_words(value) {
                        Some((verb, text, case)) => format!(
                            "{who} {verb} \"{text}\"{}",
                            if case { " (capitals matter)" } else { "" }
                        ),
                        None => format!("{who} matches regex {v}"),
                    }
                }
            }
        }
    }
}

/// How an expression is joined to what holds it, to know when it needs
/// brackets: only where an "and" sits in an "or", or the other way round.
#[derive(Clone, Copy, PartialEq)]
enum Join {
    Top,
    And,
    Or,
}

fn expr(e: &Expr, top: bool) -> String {
    joined(e, if top { Join::Top } else { Join::And })
}

fn joined(e: &Expr, within: Join) -> String {
    match e {
        Expr::Term(t) => term(t),
        Expr::Not(x) => format!("not ({})", joined(x, Join::Top)),
        Expr::And(a, b) => {
            let s = format!("{} and {}", joined(a, Join::And), joined(b, Join::And));
            // a run of "and" reads flat; inside an "or" it is bracketed
            if within == Join::Or {
                format!("({s})")
            } else {
                s
            }
        }
        Expr::Or(a, b) => {
            let s = format!("{} or {}", joined(a, Join::Or), joined(b, Join::Or));
            if within == Join::And {
                format!("({s})")
            } else {
                s
            }
        }
    }
}

/// The plain reading of `q`, or the parser's reason it cannot run.
pub fn describe_query(q: &str) -> Result<String, String> {
    let q = q.trim();
    if q.is_empty() {
        return Ok("all rows".into());
    }
    let parsed = logseek_core::query::parse(q).map_err(|e| e.msg)?;
    let mut parts = Vec::new();
    if let Some((a, b)) = parsed.time {
        let f = |us: i64| fmt_time(us).get(..16).unwrap_or("").replace('T', " ");
        parts.push(format!("time from {} to {}", f(a), f(b)));
    }
    if let Some(e) = &parsed.filter {
        parts.push(expr(e, true));
    }
    if !parsed.pipes.is_empty() {
        parts.push(format!(
            "then {} step{}",
            parsed.pipes.len(),
            if parsed.pipes.len() == 1 { "" } else { "s" }
        ));
    }
    Ok(if parts.is_empty() {
        "all rows".into()
    } else {
        parts.join(", and ")
    })
}

/// Extra hint when a query is a quoted literal that looks like a comparison.
pub fn literal_hint(q: &str) -> Option<&'static str> {
    let t = q.trim();
    (t.starts_with('"') && t[1..].contains(['<', '>', '=']))
        .then_some("literal text — for a comparison, drop the quotes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_readings() {
        assert_eq!(
            describe_query("\"EventID>1<\"").unwrap(),
            "any field contains \"EventID>1<\""
        );
        assert_eq!(describe_query("EventID>1").unwrap(), "EventID > 1");
        assert_eq!(describe_query("host:8105").unwrap(), "host contains 8105");
        assert_eq!(
            describe_query("NOT label=anom").unwrap(),
            "not (label is \"anom\")"
        );
        assert_eq!(
            describe_query("a OR b").unwrap(),
            "any field contains \"a\" or any field contains \"b\""
        );
        assert!(describe_query("@2016-08-24..2016-08-25 host=x")
            .unwrap()
            .starts_with("time from 2016-08-24 00:00 to 2016-08-25 00:00, and host is \"x\""));
        assert_eq!(describe_query("").unwrap(), "all rows");
        assert!(describe_query("host=").is_err());
        assert_eq!(
            literal_hint("\"EventID>1<\""),
            Some("literal text — for a comparison, drop the quotes")
        );
        assert_eq!(literal_hint("powershell"), None);
    }

    #[test]
    fn a_run_of_ands_or_of_ors_reads_flat_with_brackets_only_where_they_mix() {
        assert_eq!(
            describe_query("status=failed region=ap service=checkout").unwrap(),
            "status is \"failed\" and region is \"ap\" and service is \"checkout\""
        );
        assert_eq!(
            describe_query("status=failed OR status=slow OR status=ok").unwrap(),
            "status is \"failed\" or status is \"slow\" or status is \"ok\""
        );
        // an "or" inside an "and" keeps its brackets: they change the meaning
        assert_eq!(
            describe_query("region=ap (status=failed OR status=slow)").unwrap(),
            "region is \"ap\" and (status is \"failed\" or status is \"slow\")"
        );
    }
}
