//! A query written over several lines, as the one query the engine runs.
//!
//! * Each line is one condition; all of them must hold. A line with `OR` in
//!   it is put in parentheses, so it cannot swallow the line after it.
//! * A line that starts with `//` is a comment.
//! * A condition may run over several lines: while a parenthesis is open, or
//!   when a line starts or the one before ends with `OR`, `AND` or `NOT`.
//! * A leading time range (`@from..to`) goes first, wherever it was written.
//! * Lines that start with `|` (sort, fields, where) go last, in their order.
//! * Text inside quotes is never touched.

/// The query to run, and which line each part of it came from.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Built {
    pub query: String,
    /// (start, end) bytes in `query`, and the line (from 0) they came from.
    pub spans: Vec<(usize, usize, usize)>,
}

impl Built {
    /// The line that byte `at` of the query came from.
    pub fn line_at(&self, at: usize) -> Option<usize> {
        self.spans
            .iter()
            .find(|&&(a, b, _)| a <= at && at < b)
            .or_else(|| self.spans.iter().rev().find(|&&(a, _, _)| a <= at))
            .map(|s| s.2)
    }
}

/// What a line's text does to the count of open parentheses, whether it has
/// an `OR` that is not inside parentheses, and its first and last words.
struct Shape<'a> {
    depth: i32,
    top_or: bool,
    first: &'a str,
    last: &'a str,
}

fn note<'a>(w: &'a str, depth: i32, first: &mut &'a str, last: &mut &'a str, top_or: &mut bool) {
    if first.is_empty() {
        *first = w;
    }
    *last = w;
    if depth == 0 && w == "OR" {
        *top_or = true;
    }
}

fn shape(line: &str, mut depth: i32) -> Shape<'_> {
    let (mut in_q, mut in_b, mut esc) = (false, false, false);
    let mut top_or = false;
    let mut word_start: Option<usize> = None;
    let (mut first, mut last) = ("", "");
    for (i, ch) in line.char_indices() {
        let plain = !in_q && !in_b;
        let breaks = plain && (ch.is_whitespace() || ch == '(' || ch == ')');
        if breaks || !plain || ch == '"' || ch == '`' {
            if let Some(a) = word_start.take() {
                note(&line[a..i], depth, &mut first, &mut last, &mut top_or);
            }
        } else if word_start.is_none() {
            word_start = Some(i);
        }
        if esc {
            esc = false;
            continue;
        }
        match ch {
            '\\' if in_q => esc = true,
            '"' if !in_b => {
                in_q = !in_q;
                // a quoted value is a word of its own: never OR, AND or NOT
                last = "\"";
                if first.is_empty() {
                    first = "\"";
                }
            }
            '`' if !in_q => in_b = !in_b,
            '(' if plain => depth += 1,
            ')' if plain => depth -= 1,
            _ => {}
        }
    }
    if let Some(a) = word_start {
        note(&line[a..], depth, &mut first, &mut last, &mut top_or);
    }
    Shape {
        depth,
        top_or,
        first,
        last,
    }
}

fn joins(word: &str) -> bool {
    matches!(word, "OR" | "AND" | "NOT")
}

/// A leading `@a..b` token: (the token, the rest of the line).
fn leading_range(line: &str) -> Option<(&str, &str)> {
    let end = line.find(char::is_whitespace).unwrap_or(line.len());
    (line.starts_with('@') && line[..end].contains("..")).then(|| (&line[..end], &line[end..]))
}

pub fn build(text: &str) -> Built {
    // conditions: each a list of (line number, text); steps: the `|` lines
    let mut conds: Vec<Vec<(usize, &str)>> = Vec::new();
    let mut steps: Vec<(usize, &str)> = Vec::new();
    let mut range: Option<(usize, &str)> = None;
    let mut depth = 0;
    let mut prev_last = "";
    for (n, raw) in text.lines().enumerate() {
        let mut line = raw.trim();
        if line.is_empty() || (depth == 0 && line.starts_with("//")) {
            continue;
        }
        if depth == 0 && line.starts_with('|') {
            steps.push((n, line));
            prev_last = "";
            continue;
        }
        if depth == 0 && range.is_none() {
            if let Some((r, rest)) = leading_range(line) {
                range = Some((n, r));
                line = rest.trim();
                if line.is_empty() {
                    continue;
                }
            }
        }
        let s = shape(line, depth);
        let goes_on = depth > 0 || joins(s.first) && s.first != "NOT" || joins(prev_last);
        match conds.last_mut() {
            Some(c) if goes_on => c.push((n, line)),
            _ => conds.push(vec![(n, line)]),
        }
        depth = s.depth.max(0);
        prev_last = s.last;
    }
    let mut out = Built::default();
    let add = |out: &mut Built, text: &str, line: Option<usize>| {
        let a = out.query.len();
        out.query.push_str(text);
        if let Some(l) = line {
            out.spans.push((a, out.query.len(), l));
        }
    };
    if let Some((n, r)) = range {
        add(&mut out, r, Some(n));
    }
    let many = conds.len() > 1;
    for c in &conds {
        if !out.query.is_empty() {
            add(&mut out, " ", None);
        }
        // only a condition with an OR of its own needs fencing off
        let mut d = 0;
        let wrap = many
            && c.iter().any(|(_, l)| {
                let s = shape(l, d);
                d = s.depth.max(0);
                s.top_or
            });
        if wrap {
            add(&mut out, "(", None);
        }
        for (k, (n, l)) in c.iter().enumerate() {
            if k > 0 {
                add(&mut out, " ", None);
            }
            add(&mut out, l, Some(*n));
        }
        if wrap {
            add(&mut out, ")", None);
        }
    }
    for (n, s) in steps {
        if !out.query.is_empty() {
            add(&mut out, " ", None);
        }
        add(&mut out, s, Some(n));
    }
    out
}

/// Why the query cannot run and the line (from 0) to look at; `None` when it can.
pub fn problem(text: &str) -> Option<(usize, String)> {
    let b = build(text);
    if b.query.is_empty() {
        return None;
    }
    match logseek_core::query::parse(&b.query) {
        Err(e) => Some((b.line_at(e.span.0).unwrap_or(0), e.msg)),
        Ok(p) => p.rows_only().err().map(|msg| {
            // a step the rows cannot show: point at the first `|` line
            let line = text
                .lines()
                .position(|l| l.trim_start().starts_with('|'))
                .or_else(|| b.query.find('|').and_then(|at| b.line_at(at)))
                .unwrap_or(0);
            (line, msg)
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(text: &str) -> String {
        build(text).query
    }

    #[test]
    fn plain_lines_join_as_they_are() {
        assert_eq!(q("host=web1\n  status>=500\n"), "host=web1 status>=500");
        assert_eq!(q("host=web1"), "host=web1");
        assert_eq!(q("  \n\n"), "");
    }

    #[test]
    fn a_line_with_or_cannot_swallow_the_next_line() {
        assert_eq!(
            q("FileName=cmd.exe OR FileName=powershell.exe\nNOT UserName:*$"),
            "(FileName=cmd.exe OR FileName=powershell.exe) NOT UserName:*$"
        );
        // alone, it needs no fence
        assert_eq!(q("a OR b"), "a OR b");
        // an OR already inside parentheses is fenced by them
        assert_eq!(q("(a OR b)\nc"), "(a OR b) c");
    }

    #[test]
    fn text_inside_quotes_is_never_touched() {
        assert_eq!(
            q("CommandLine:\"a   b OR ( c\"\nx"),
            "CommandLine:\"a   b OR ( c\" x"
        );
    }

    #[test]
    fn comments_are_only_whole_lines() {
        assert_eq!(q("// parent is the agent\nhost=web1"), "host=web1");
        assert_eq!(q("url:http://x.test/a"), "url:http://x.test/a");
    }

    #[test]
    fn a_condition_can_run_over_several_lines() {
        // an open parenthesis keeps going
        assert_eq!(q("(a\nOR b)\nc"), "(a OR b) c");
        // so does a line that starts or ends with OR / AND
        assert_eq!(q("a\nOR b\nc"), "(a OR b) c");
        assert_eq!(q("a OR\nb\nc"), "(a OR b) c");
        // NOT at the start of a line is a condition of its own
        assert_eq!(q("a\nNOT b"), "a NOT b");
    }

    #[test]
    fn the_time_range_goes_first_and_steps_go_last() {
        assert_eq!(
            q("| sort -bytes\na OR b\n@2026-11-12T10:00..2026-11-12T11:00\nc\n| fields a, b"),
            "@2026-11-12T10:00..2026-11-12T11:00 (a OR b) c | sort -bytes | fields a, b"
        );
    }

    #[test]
    fn a_problem_says_which_line() {
        assert_eq!(problem("host=web1\nstatus>=500"), None);
        assert_eq!(problem("   "), None);
        let (line, _) = problem("host=web1\n// note\nstatus=(").unwrap();
        assert_eq!(line, 2);
        // a step the rows cannot show points at its own line
        let (line, msg) = problem("host=web1\n| stats count by user").unwrap();
        assert_eq!(line, 1);
        assert!(msg.contains("Summarise"));
    }
}
