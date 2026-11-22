//! What each condition of a search keeps on its own. A search is its time
//! range, the terms typed at the top (joined by "and"), and each column's
//! filter. Each of these is counted alone, so it is plain which one narrows
//! the rows, and which one keeps none and empties the result.

use crate::filters::{self, Filters};
use logseek_core::formula::{column_ref, quote};
use logseek_core::query::ast::{CmpOp, Expr, Term, ANY_FIELD};

/// One condition: how it reads, and the query that is this condition alone.
#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    pub words: String,
    pub query: String,
}

/// A term written back as query text. `None` for a kind not written here.
fn term_text(t: &Term) -> Option<String> {
    Some(match t {
        Term::FreeText(s) => quote(s),
        Term::IsError(_) => return None,
        Term::Cmp { col, op, value } => {
            let c = if col == ANY_FIELD {
                String::new()
            } else {
                column_ref(col)
            };
            match op {
                CmpOp::Eq => format!("{c}={}", quote(value)),
                CmpOp::Ne => format!("{c}!={}", quote(value)),
                CmpOp::Gt => format!("{c}>{}", quote(value)),
                CmpOp::Ge => format!("{c}>={}", quote(value)),
                CmpOp::Lt => format!("{c}<{}", quote(value)),
                CmpOp::Le => format!("{c}<={}", quote(value)),
                CmpOp::Contains | CmpOp::Glob => format!("{c}:{}", quote(value)),
                CmpOp::Regex => format!("{c}~/{value}/"),
            }
        }
    })
}

fn expr_text(e: &Expr) -> Option<String> {
    Some(match e {
        Expr::Term(t) => term_text(t)?,
        Expr::Not(x) => format!("NOT ({})", expr_text(x)?),
        Expr::And(a, b) => format!("({} {})", expr_text(a)?, expr_text(b)?),
        Expr::Or(a, b) => format!("({} OR {})", expr_text(a)?, expr_text(b)?),
    })
}

/// The conditions joined by "and" at the top of an expression, in order.
fn top_ands<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
    match e {
        Expr::And(a, b) => {
            top_ands(a, out);
            top_ands(b, out);
        }
        other => out.push(other),
    }
}

/// The typed search as its conditions, each a query of its own. Every one is
/// read back by the engine and must mean exactly what it came from; if any
/// does not, the typed search stays one condition (it is never counted wrong).
fn typed_parts(text: &str) -> Vec<Part> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let whole = || {
        vec![Part {
            words: crate::meaning::describe_query(text).unwrap_or_else(|_| text.to_string()),
            query: text.to_string(),
        }]
    };
    let Ok(parsed) = logseek_core::query::parse(text) else {
        return whole();
    };
    let Some(filter) = parsed.filter.as_ref().filter(|_| parsed.pipes.is_empty()) else {
        return whole();
    };
    let mut ands = Vec::new();
    top_ands(filter, &mut ands);
    if ands.len() < 2 {
        return whole();
    }
    let mut out = Vec::new();
    for e in ands {
        let Some(q) = expr_text(e) else {
            return whole();
        };
        let same = logseek_core::query::parse(&q)
            .ok()
            .is_some_and(|p| p.filter.as_ref() == Some(e) && p.time.is_none() && p.pipes.is_empty());
        if !same {
            return whole();
        }
        out.push(Part {
            words: crate::meaning::describe_query(&q).unwrap_or_else(|_| q.clone()),
            query: q,
        });
    }
    out
}

/// Every condition of the search on screen: the period, the typed terms, the
/// column filters that are on.
pub fn parts(global: &str, filters: &Filters) -> Vec<Part> {
    let (range, rest) = filters::split_range(global);
    let mut out = Vec::new();
    if !range.is_empty() {
        out.push(Part {
            words: crate::meaning::describe_query(range).unwrap_or_else(|_| range.to_string()),
            query: range.to_string(),
        });
    }
    out.extend(typed_parts(rest));
    for (col, f) in filters {
        if let Some(clause) = filters::column_clause(col, f) {
            out.push(Part {
                words: filters::describe(col, f),
                query: clause,
            });
        }
    }
    out
}

/// A condition whose text can be found in cells: how it reads, the text, and
/// the one column it looks in (`None`: any column).
#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    pub label: String,
    pub text: String,
    pub col: Option<String>,
}

/// The conditions of the typed search that have a text to find in cells
/// (a word in any field, a column that is or contains a value), and in
/// words those that have none (a time range, more than, less than).
pub fn marks(global: &str) -> (Vec<Mark>, Vec<String>) {
    let (range, rest) = filters::split_range(global);
    let mut marks = Vec::new();
    let mut others = Vec::new();
    if !range.is_empty() {
        others.push(crate::meaning::describe_query(range).unwrap_or_else(|_| range.to_string()));
    }
    let Some(filter) = logseek_core::query::parse(rest.trim())
        .ok()
        .and_then(|p| p.filter)
    else {
        // not readable as a search: its plain words, as they are marked in the rows
        for t in filters::highlight_terms(global) {
            marks.push(Mark {
                label: format!("\"{t}\" in any field"),
                text: t,
                col: None,
            });
        }
        return (marks, others);
    };
    let mut ands = Vec::new();
    top_ands(&filter, &mut ands);
    for e in ands {
        let words = expr_text(e)
            .and_then(|q| crate::meaning::describe_query(&q).ok())
            .unwrap_or_else(|| "a condition".to_string());
        match e {
            Expr::Term(Term::FreeText(s)) => marks.push(Mark {
                label: words,
                text: s.clone(),
                col: None,
            }),
            Expr::Term(Term::Cmp { col, op: CmpOp::Eq | CmpOp::Contains, value })
                if col != ANY_FIELD && !value.trim().is_empty() =>
            {
                marks.push(Mark {
                    label: words,
                    text: value.clone(),
                    col: Some(col.clone()),
                })
            }
            _ => others.push(words),
        }
    }
    (marks, others)
}

/// "keeps 1,204 rows", "keeps no rows", and while it is counted "counting".
pub fn kept_words(n: Option<u64>) -> String {
    match n {
        None => "counting".to_string(),
        Some(0) => "keeps no rows".to_string(),
        Some(1) => "keeps 1 row".to_string(),
        Some(n) => format!("keeps {} rows", crate::moment::group(n)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queries(text: &str) -> Vec<String> {
        parts(text, &Filters::new()).into_iter().map(|p| p.query).collect()
    }

    #[test]
    fn terms_joined_by_and_are_each_a_condition() {
        assert_eq!(
            queries("status=failed region=ap"),
            vec!["status=\"failed\"", "region=\"ap\""]
        );
        assert_eq!(
            queries("mina.okoro latency_ms>5000"),
            vec!["\"mina.okoro\"", "latency_ms>\"5000\""]
        );
    }

    #[test]
    fn each_condition_reads_as_the_search_does() {
        let p = parts("status=failed tanaka", &Filters::new());
        assert_eq!(p[0].words, "status is \"failed\"");
        assert_eq!(p[1].words, "any field contains \"tanaka\"");
    }

    #[test]
    fn an_or_stays_whole_inside_its_condition() {
        let q = queries("(status=failed OR status=slow) region=ap");
        assert_eq!(q.len(), 2);
        assert_eq!(q[0], "(status=\"failed\" OR status=\"slow\")");
        // a search that is one "or" is one condition
        assert_eq!(queries("status=failed OR status=slow").len(), 1);
    }

    #[test]
    fn a_period_is_a_condition_of_its_own() {
        let q = queries("@2026-03-02..2026-03-03 status=failed region=ap");
        assert_eq!(q.len(), 3);
        assert_eq!(q[0], "@2026-03-02..2026-03-03");
    }

    #[test]
    fn one_term_or_nothing_is_not_split() {
        assert_eq!(queries("tanaka"), vec!["tanaka"]);
        assert!(queries("   ").is_empty());
        // what cannot be read stays as typed, one condition
        assert_eq!(queries("nosuch=(").len(), 1);
    }

    #[test]
    fn every_split_condition_means_what_it_came_from() {
        for text in [
            "status=failed region=ap service=checkout",
            "user:adm* latency_ms>=10 NOT status=ok",
            "\"payment declined\" amount<5",
        ] {
            let whole = logseek_core::query::parse(text).unwrap().filter.unwrap();
            let mut ands = Vec::new();
            top_ands(&whole, &mut ands);
            let got = queries(text);
            assert_eq!(got.len(), ands.len(), "{text}");
            for (q, e) in got.iter().zip(ands) {
                let back = logseek_core::query::parse(q).unwrap().filter.unwrap();
                assert_eq!(&back, e, "{text} -> {q}");
            }
        }
    }

    #[test]
    fn a_search_gives_what_can_be_marked_in_cells_and_names_the_rest() {
        let (m, o) = marks("\"mina.okoro\" status=failed latency_ms>5000");
        assert_eq!(m.len(), 2);
        assert_eq!((m[0].text.as_str(), m[0].col.as_deref()), ("mina.okoro", None));
        assert_eq!(m[0].label, "any field contains \"mina.okoro\"");
        assert_eq!((m[1].text.as_str(), m[1].col.as_deref()), ("failed", Some("status")));
        assert_eq!(o, vec!["latency_ms > 5000"]);
        // a period has no word to mark
        let (m, o) = marks("@2026-03-02..2026-03-03 tanaka");
        assert_eq!(m.len(), 1);
        assert_eq!(o.len(), 1);
        assert!(marks("  ").0.is_empty() && marks("  ").1.is_empty());
    }

    #[test]
    fn counts_are_said_in_words() {
        assert_eq!(kept_words(None), "counting");
        assert_eq!(kept_words(Some(0)), "keeps no rows");
        assert_eq!(kept_words(Some(1)), "keeps 1 row");
        assert_eq!(kept_words(Some(18_300)), "keeps 18,300 rows");
    }
}
