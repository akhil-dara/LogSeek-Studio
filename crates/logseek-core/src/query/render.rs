//! Canonical text for a query: `parse(to_string(q)) == q`.

use super::ast::*;
use crate::types::fmt_time;

fn bare_ok(s: &str) -> bool {
    !s.is_empty()
        && !matches!(s, "OR" | "AND" | "NOT" | "by" | "!")
        && !s.starts_with('!')
        && !s.starts_with('@')
        && !s.starts_with('-')
        && s.chars().all(|c| {
            !c.is_whitespace()
                && !matches!(
                    c,
                    '(' | ')' | '|' | ',' | '"' | '`' | '=' | '<' | '>' | ':' | '~'
                )
        })
}

fn quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            o.push('\\');
        }
        o.push(c);
    }
    o.push('"');
    o
}

/// A column name as it must be typed.
pub fn field_text(f: &str) -> String {
    let plain = !f.is_empty()
        && !f.starts_with('-')
        && !f.starts_with('!')
        && !matches!(f, "OR" | "AND" | "NOT" | "by")
        && f.chars().all(|c| {
            !c.is_whitespace()
                && !matches!(
                    c,
                    '(' | ')' | '|' | ',' | '"' | '`' | '=' | '<' | '>' | ':' | '~'
                )
        });
    if plain {
        f.to_string()
    } else {
        format!("`{f}`")
    }
}

/// A value as it must be typed after an operator.
pub fn value_text(v: &str) -> String {
    if !v.is_empty()
        && v.chars()
            .all(|c| !c.is_whitespace() && !matches!(c, '(' | ')' | '|' | ',' | '"' | '`'))
    {
        v.to_string()
    } else {
        quote(v)
    }
}

fn term(t: &Term) -> String {
    match t {
        Term::FreeText(s) => {
            if bare_ok(s) {
                s.clone()
            } else {
                quote(s)
            }
        }
        Term::Cmp { col, op, value } => match op {
            CmpOp::Regex if col == ANY_FIELD => format!("~/{value}/"),
            CmpOp::Regex => format!("{}~/{}/", field_text(col), value),
            _ => format!("{}{}{}", field_text(col), op.symbol(), value_text(value)),
        },
        Term::IsError(col) => format!("iserror({})", field_text(col)),
    }
}

fn expr(e: &Expr) -> String {
    match e {
        Expr::Term(t) => term(t),
        Expr::Not(x) => match **x {
            Expr::Term(_) | Expr::Not(_) => format!("NOT {}", expr(x)),
            _ => format!("NOT ({})", expr(x)),
        },
        Expr::And(l, r) => {
            let ls = match **l {
                Expr::Or(..) => format!("({})", expr(l)),
                _ => expr(l),
            };
            let rs = match **r {
                Expr::Or(..) | Expr::And(..) => format!("({})", expr(r)),
                _ => expr(r),
            };
            format!("{ls} {rs}")
        }
        Expr::Or(l, r) => {
            let rs = match **r {
                Expr::Or(..) => format!("({})", expr(r)),
                _ => expr(r),
            };
            format!("{} OR {}", expr(l), rs)
        }
    }
}

fn agg(a: &Agg) -> String {
    match &a.col {
        Some(c) => format!("{}({})", a.func.name(), field_text(c)),
        None => a.func.name().to_string(),
    }
}

fn span(us: i64) -> String {
    for (unit, n) in [
        ("w", 604_800_000_000i64),
        ("d", 86_400_000_000),
        ("h", 3_600_000_000),
        ("m", 60_000_000),
        ("s", 1_000_000),
    ] {
        if us % n == 0 {
            return format!("{}{unit}", us / n);
        }
    }
    format!("{}ms", us / 1000)
}

fn time_lit(us: i64) -> String {
    let s = fmt_time(us);
    match s.strip_suffix(" 00:00:00") {
        Some(d) => d.to_string(),
        None => s.replace(' ', "T"),
    }
}

pub fn to_string(q: &Query) -> String {
    let mut parts = Vec::new();
    if let Some((a, b)) = q.time {
        parts.push(format!("@{}..{}", time_lit(a), time_lit(b)));
    }
    if let Some(f) = &q.filter {
        parts.push(expr(f));
    }
    let mut s = parts.join(" ");
    for p in &q.pipes {
        let t = match p {
            Pipe::Where(e) => format!("where {}", expr(e)),
            Pipe::Stats { aggs, by } => {
                let mut t = format!(
                    "stats {}",
                    aggs.iter().map(agg).collect::<Vec<_>>().join(", ")
                );
                if !by.is_empty() {
                    t.push_str(" by ");
                    t.push_str(
                        &by.iter()
                            .map(|f| field_text(f))
                            .collect::<Vec<_>>()
                            .join(", "),
                    );
                }
                t
            }
            Pipe::Top { n, col } => format!("top {n} {}", field_text(col)),
            Pipe::Timechart {
                span_us,
                agg: a,
                by,
            } => {
                let mut t = format!("timechart {} {}", span(*span_us), agg(a));
                if let Some(b) = by {
                    t.push_str(&format!(" by {}", field_text(b)));
                }
                t
            }
            Pipe::Sort(keys) => format!(
                "sort {}",
                keys.iter()
                    .map(|(c, d)| format!("{}{}", if *d { "-" } else { "" }, field_text(c)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Pipe::Fields(f) => format!(
                "fields {}",
                f.iter()
                    .map(|x| field_text(x))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Pipe::Head(n) => format!("head {n}"),
        };
        if s.is_empty() {
            s = format!("| {t}");
        } else {
            s = format!("{s} | {t}");
        }
    }
    s
}
