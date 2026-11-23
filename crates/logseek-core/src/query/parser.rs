//! Lexer + recursive-descent parser with byte spans for every error.

use super::ast::*;
use crate::time::parse_time;

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    /// Byte range in the query text to underline.
    pub span: (usize, usize),
    pub msg: String,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    Str(String),
    Field(String),
    LParen,
    RParen,
    Pipe,
    Comma,
}

#[derive(Debug, Clone)]
struct T {
    tok: Tok,
    a: usize,
    b: usize,
}

fn err<X>(a: usize, b: usize, msg: impl Into<String>) -> Result<X, ParseError> {
    Err(ParseError {
        span: (a, b),
        msg: msg.into(),
    })
}

fn lex(s: &str) -> Result<Vec<T>, ParseError> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        match c {
            b'(' => {
                out.push(T {
                    tok: Tok::LParen,
                    a: i,
                    b: i + 1,
                });
                i += 1;
            }
            b')' => {
                out.push(T {
                    tok: Tok::RParen,
                    a: i,
                    b: i + 1,
                });
                i += 1;
            }
            b'|' => {
                out.push(T {
                    tok: Tok::Pipe,
                    a: i,
                    b: i + 1,
                });
                i += 1;
            }
            b',' => {
                out.push(T {
                    tok: Tok::Comma,
                    a: i,
                    b: i + 1,
                });
                i += 1;
            }
            b'"' => {
                i += 1;
                let mut v = String::new();
                let mut closed = false;
                while i < b.len() {
                    match b[i] {
                        b'\\' if i + 1 < b.len() && (b[i + 1] == b'"' || b[i + 1] == b'\\') => {
                            v.push(b[i + 1] as char);
                            i += 2;
                        }
                        b'"' => {
                            closed = true;
                            i += 1;
                            break;
                        }
                        _ => {
                            let ch = s[i..].chars().next().unwrap();
                            v.push(ch);
                            i += ch.len_utf8();
                        }
                    }
                }
                if !closed {
                    return err(start, b.len(), "missing closing quote \"");
                }
                out.push(T {
                    tok: Tok::Str(v),
                    a: start,
                    b: i,
                });
            }
            b'`' => {
                let Some((name, used)) = crate::formula::backtick_name(&s[i..]) else {
                    return err(start, b.len(), "missing closing backtick `");
                };
                out.push(T {
                    tok: Tok::Field(name),
                    a: start,
                    b: i + used,
                });
                i += used;
            }
            _ => {
                // a word; a `~/regex/` inside it is consumed whole (it may contain parens)
                while i < b.len() {
                    let ch = b[i];
                    if ch == b'~' && b.get(i + 1) == Some(&b'/') {
                        let mut j = i + 2;
                        while j < b.len() && b[j] != b'/' {
                            if b[j] == b'\\' {
                                j += 1;
                            }
                            j += 1;
                        }
                        if j >= b.len() {
                            return err(i, b.len(), "missing closing / of the regex");
                        }
                        i = j + 1;
                        continue;
                    }
                    if ch.is_ascii_whitespace()
                        || matches!(ch, b'(' | b')' | b'|' | b',' | b'"' | b'`')
                    {
                        break;
                    }
                    i += s[i..].chars().next().map_or(1, |c| c.len_utf8());
                }
                out.push(T {
                    tok: Tok::Word(s[start..i].to_string()),
                    a: start,
                    b: i,
                });
            }
        }
    }
    Ok(out)
}

const OPS: [(&str, CmpOp); 10] = [
    ("!=", CmpOp::Ne),
    (">=", CmpOp::Ge),
    ("<=", CmpOp::Le),
    // the way other tools write "is" and "is not": understood, never misread
    ("<>", CmpOp::Ne),
    ("==", CmpOp::Eq),
    ("=", CmpOp::Eq),
    (">", CmpOp::Gt),
    ("<", CmpOp::Lt),
    (":", CmpOp::Contains),
    ("~", CmpOp::Regex),
];

/// Split `word` at its first operator (not at position 0).
fn split_op(word: &str) -> Option<(usize, &'static str, CmpOp)> {
    let mut best: Option<(usize, &'static str, CmpOp)> = None;
    for (sym, op) in OPS {
        if let Some(i) = word[1.min(word.len())..].find(sym).map(|i| i + 1) {
            if best.is_none_or(|(bi, bs, _)| i < bi || (i == bi && sym.len() > bs.len())) {
                best = Some((i, sym, op));
            }
        }
    }
    best
}

/// An operator at the very start of `word` (used after a `backtick` field).
fn leading_op(word: &str) -> Option<(&'static str, CmpOp)> {
    OPS.iter()
        .find(|(s, _)| word.starts_with(s))
        .map(|(s, o)| (*s, *o))
}

struct P<'a> {
    src: &'a str,
    t: Vec<T>,
    i: usize,
}

fn is_kw(t: &T, kw: &str) -> bool {
    matches!(&t.tok, Tok::Word(w) if w == kw)
}

/// "or", "and", "not" written without capitals: which joining word was meant.
fn small_kw(w: &str) -> Option<&'static str> {
    ["OR", "AND", "NOT"]
        .into_iter()
        .find(|k| w != *k && w.eq_ignore_ascii_case(k))
}

/// A comparison sign standing apart from its column (`EventID = 1`,
/// `EventID =1`): the sign and whatever is written straight after it.
/// `:` and `~` are left out: `~/x/` alone is a term of its own.
fn spaced_op(w: &str) -> Option<(&'static str, CmpOp)> {
    leading_op(w).filter(|(s, _)| *s != ":" && *s != "~")
}

impl<'a> P<'a> {
    fn peek(&self) -> Option<&T> {
        self.t.get(self.i)
    }
    fn end_pos(&self) -> usize {
        self.src.len()
    }

    fn query(&mut self) -> Result<Query, ParseError> {
        let mut q = Query::default();
        // a time range anywhere before the first | applies to the whole query
        // (it is never searched as text)
        let mut k = self.i;
        while k < self.t.len() && self.t[k].tok != Tok::Pipe {
            if let Tok::Word(w) = &self.t[k].tok {
                if let Some(r) = w.strip_prefix('@').filter(|r| r.contains("..")) {
                    let (x, y) = r.split_once("..").unwrap();
                    let (Some(ta), Some(tb)) = (parse_time(x), parse_time(y)) else {
                        return err(
                            self.t[k].a,
                            self.t[k].b,
                            "time range must look like @2026-11-01..2026-11-03",
                        );
                    };
                    q.time = Some((ta.min(tb), ta.max(tb)));
                    self.t.remove(k);
                    continue;
                }
            }
            k += 1;
        }
        // leading time range(s)
        while let Some(T {
            tok: Tok::Word(w),
            a,
            b,
        }) = self.peek().cloned()
        {
            if let Some(r) = w.strip_prefix('@').filter(|r| r.contains("..")) {
                let (x, y) = r.split_once("..").unwrap();
                let (Some(ta), Some(tb)) = (parse_time(x), parse_time(y)) else {
                    return err(a, b, "time range must look like @2026-11-01..2026-11-03");
                };
                q.time = Some((ta.min(tb), ta.max(tb)));
                self.i += 1;
            } else {
                break;
            }
        }
        if self.peek().is_some_and(|t| t.tok != Tok::Pipe) {
            q.filter = Some(self.or_expr()?);
        }
        while let Some(t) = self.peek().cloned() {
            if t.tok != Tok::Pipe {
                return err(t.a, t.b, "expected | or end of query");
            }
            self.i += 1;
            q.pipes.push(self.pipe(t.b)?);
        }
        Ok(q)
    }

    fn starts_term(&self) -> bool {
        match self.peek() {
            None => false,
            Some(t) => match &t.tok {
                Tok::Word(w) => w != "OR" && w != "AND",
                Tok::Str(_) | Tok::Field(_) | Tok::LParen => true,
                _ => false,
            },
        }
    }

    fn or_expr(&mut self) -> Result<Expr, ParseError> {
        let mut l = self.and_expr()?;
        while self.peek().is_some_and(|t| is_kw(t, "OR")) {
            self.i += 1;
            let r = self.and_expr()?;
            l = Expr::Or(Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    /// Can a term start at token `k`?
    fn term_at(&self, k: usize) -> bool {
        self.t.get(k).is_some_and(|t| {
            matches!(
                t.tok,
                Tok::Word(_) | Tok::Str(_) | Tok::Field(_) | Tok::LParen
            )
        })
    }

    /// A lowercase "or" / "and" / "not" standing where the joining word
    /// would: said in words. It is never quietly searched as a word, and
    /// never quietly taken as the joining word either.
    fn small_kw_here(&self, only_not: bool) -> Result<(), ParseError> {
        let Some(T {
            tok: Tok::Word(w),
            a,
            b,
        }) = self.peek()
        else {
            return Ok(());
        };
        let Some(k) = small_kw(w) else { return Ok(()) };
        if (k == "NOT") != only_not || !self.term_at(self.i + 1) {
            return Ok(());
        }
        let what = match k {
            "OR" => "either side",
            "AND" => "both sides",
            _ => "leave out what follows",
        };
        err(
            *a,
            *b,
            format!(
                "write {k} in capitals to mean \"{what}\"; to look for the word itself, put it in quotes: \"{}\"",
                w
            ),
        )
    }

    fn and_expr(&mut self) -> Result<Expr, ParseError> {
        let mut l = self.not_expr()?;
        loop {
            self.small_kw_here(false)?;
            if self.peek().is_some_and(|t| is_kw(t, "AND")) {
                self.i += 1;
            } else if !self.starts_term() {
                break;
            }
            let r = self.not_expr()?;
            l = Expr::And(Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn not_expr(&mut self) -> Result<Expr, ParseError> {
        self.small_kw_here(true)?;
        match self.peek().cloned() {
            Some(t) if is_kw(&t, "NOT") || matches!(&t.tok, Tok::Word(w) if w == "!") => {
                self.i += 1;
                Ok(Expr::Not(Box::new(self.not_expr()?)))
            }
            _ => self.atom(),
        }
    }

    fn atom(&mut self) -> Result<Expr, ParseError> {
        let Some(t) = self.peek().cloned() else {
            return err(self.end_pos(), self.end_pos(), "expected a search term");
        };
        self.i += 1;
        match t.tok {
            Tok::LParen => {
                let e = self.or_expr()?;
                match self.peek() {
                    Some(T {
                        tok: Tok::RParen, ..
                    }) => {
                        self.i += 1;
                        Ok(e)
                    }
                    _ => err(t.a, self.end_pos(), "missing closing )"),
                }
            }
            Tok::Str(s) => Ok(Expr::Term(Term::FreeText(s))),
            Tok::Field(col) => {
                let Some(n) = self.peek().cloned() else {
                    return err(
                        t.a,
                        t.b,
                        "expected an operator after the column, e.g. `Col`=value",
                    );
                };
                let Tok::Word(w) = &n.tok else {
                    return err(n.a, n.b, "expected an operator after the column");
                };
                let Some((sym, op)) = leading_op(w) else {
                    return err(n.a, n.b, "expected =, !=, >, <, :, or ~");
                };
                self.i += 1;
                let value = w[sym.len()..].to_string();
                self.finish_cmp(col, op, value, t.a, n.b)
            }
            // `iserror(col)`, the bracket against the word: rows where a made
            // column could not be worked out ("iserror (a OR b)" stays a search)
            Tok::Word(w)
                if w.eq_ignore_ascii_case("iserror")
                    && matches!(self.peek(), Some(T { tok: Tok::LParen, a, .. }) if *a == t.b) =>
            {
                self.i += 1;
                let col = match self.peek().cloned() {
                    Some(T {
                        tok: Tok::Word(c) | Tok::Field(c) | Tok::Str(c),
                        ..
                    }) => c,
                    _ => return err(t.a, self.end_pos(), "iserror( needs a column name"),
                };
                self.i += 1;
                match self.peek() {
                    Some(T {
                        tok: Tok::RParen, ..
                    }) => {
                        self.i += 1;
                        Ok(Expr::Term(Term::IsError(col)))
                    }
                    _ => err(
                        t.a,
                        self.end_pos(),
                        "missing closing ) after iserror(column",
                    ),
                }
            }
            Tok::Word(w) => {
                if let Some(rest) = w
                    .strip_prefix('!')
                    .filter(|r| !r.is_empty() && !r.starts_with('='))
                {
                    let inner = self.word_term(rest.to_string(), t.a + 1, t.b)?;
                    return Ok(Expr::Not(Box::new(inner)));
                }
                self.word_term(w, t.a, t.b)
            }
            Tok::RParen => err(t.a, t.b, "unexpected )"),
            Tok::Pipe => err(t.a, t.b, "expected a search term before |"),
            Tok::Comma => err(t.a, t.b, "unexpected ,"),
        }
    }

    fn word_term(&mut self, w: String, a: usize, b: usize) -> Result<Expr, ParseError> {
        // `~/pattern/` on its own: the pattern is looked for in every field
        if let Some(p) = w
            .strip_prefix("~/")
            .and_then(|r| r.strip_suffix('/'))
            .filter(|p| !p.is_empty())
        {
            if let Err(e) = regex_syntax_ok(p) {
                return err(a, b, format!("invalid regex: {e}"));
            }
            return Ok(Expr::Term(Term::Cmp {
                col: ANY_FIELD.to_string(),
                op: CmpOp::Regex,
                value: p.to_string(),
            }));
        }
        match split_op(&w) {
            Some((i, sym, op)) => {
                let col = w[..i].to_string();
                let value = w[i + sym.len()..].to_string();
                self.finish_cmp(col, op, value, a, b)
            }
            None => {
                // `EventID = 1`, `EventID =1`: the sign stands apart from the column
                if let Some(T {
                    tok: Tok::Word(n),
                    b: nb,
                    ..
                }) = self.peek().cloned()
                {
                    if let Some((sym, op)) = spaced_op(&n) {
                        if w != "OR" && w != "AND" && w != "NOT" {
                            self.i += 1;
                            return self.finish_cmp(w, op, n[sym.len()..].to_string(), a, nb);
                        }
                    }
                }
                Ok(Expr::Term(Term::FreeText(w)))
            }
        }
    }

    fn finish_cmp(
        &mut self,
        col: String,
        mut op: CmpOp,
        mut value: String,
        a: usize,
        b: usize,
    ) -> Result<Expr, ParseError> {
        if value.is_empty() {
            match self.peek().cloned() {
                Some(T {
                    tok: Tok::Str(s), ..
                }) => {
                    self.i += 1;
                    value = s;
                }
                // `EventID= 1`: the value stands apart from the sign
                Some(T {
                    tok: Tok::Word(s), ..
                }) if s != "OR" && s != "AND" && s != "NOT" && leading_op(&s).is_none() => {
                    self.i += 1;
                    value = s;
                }
                _ => return err(a, b, format!("missing value after {}{}", col, op.symbol())),
            }
        } else if op == CmpOp::Regex {
            match value.strip_prefix('/').and_then(|v| v.strip_suffix('/')) {
                Some(v) => value = v.to_string(),
                None => return err(a, b, "regex must look like col~/pattern/"),
            }
            if let Err(e) = regex_syntax_ok(&value) {
                return err(a, b, format!("invalid regex: {e}"));
            }
        }
        if op == CmpOp::Contains && value.contains('*') {
            op = CmpOp::Glob;
        }
        Ok(Expr::Term(Term::Cmp { col, op, value }))
    }

    fn word(&mut self, what: &str, after: usize) -> Result<(String, usize, usize), ParseError> {
        match self.peek().cloned() {
            Some(T {
                tok: Tok::Word(w),
                a,
                b,
            }) => {
                self.i += 1;
                Ok((w, a, b))
            }
            Some(T {
                tok: Tok::Field(w),
                a,
                b,
            }) => {
                self.i += 1;
                Ok((w, a, b))
            }
            Some(t) => err(t.a, t.b, format!("expected {what}")),
            None => err(after, self.end_pos().max(after), format!("expected {what}")),
        }
    }

    fn field_list(&mut self, after: usize) -> Result<Vec<String>, ParseError> {
        let mut v = vec![self.word("a column name", after)?.0];
        while self.peek().is_some_and(|t| t.tok == Tok::Comma) {
            self.i += 1;
            v.push(self.word("a column name", after)?.0);
        }
        Ok(v)
    }

    fn agg(&mut self, after: usize) -> Result<Agg, ParseError> {
        let (name, a, b) = self.word("an aggregate like count or dc(col)", after)?;
        let Some(func) = AggFn::from_name(&name) else {
            return err(
                a,
                b,
                "unknown aggregate (use count, dc, sum, avg, min, max)",
            );
        };
        let mut col = None;
        if self.peek().is_some_and(|t| t.tok == Tok::LParen) {
            self.i += 1;
            col = Some(self.word("a column name", b)?.0);
            match self.peek() {
                Some(T {
                    tok: Tok::RParen, ..
                }) => self.i += 1,
                _ => return err(a, self.end_pos(), "missing closing )"),
            }
        }
        if func != AggFn::Count && col.is_none() {
            return err(a, b, format!("{}(column) needs a column", func.name()));
        }
        Ok(Agg { func, col })
    }

    fn pipe(&mut self, after: usize) -> Result<Pipe, ParseError> {
        let (cmd, a, b) = self.word(
            "a command after | (where, stats, top, timechart, sort, fields, head)",
            after,
        )?;
        match cmd.to_ascii_lowercase().as_str() {
            "where" => Ok(Pipe::Where(self.or_expr()?)),
            "stats" => {
                let mut aggs = vec![self.agg(b)?];
                while self.peek().is_some_and(|t| t.tok == Tok::Comma) {
                    self.i += 1;
                    aggs.push(self.agg(b)?);
                }
                let mut by = Vec::new();
                if self.peek().is_some_and(|t| is_kw(t, "by")) {
                    let bb = self.peek().unwrap().b;
                    self.i += 1;
                    by = self.field_list(bb)?;
                }
                Ok(Pipe::Stats { aggs, by })
            }
            "top" => {
                let mut n = 10;
                if let Some(T {
                    tok: Tok::Word(w), ..
                }) = self.peek()
                {
                    if let Ok(k) = w.parse::<usize>() {
                        n = k;
                        self.i += 1;
                    }
                }
                Ok(Pipe::Top {
                    n,
                    col: self.word("a column name", b)?.0,
                })
            }
            "timechart" => {
                let (sp, sa, sb) = self.word("a span like 1h, 5m, 30s or 1d", b)?;
                let Some(span_us) = parse_span(&sp) else {
                    return err(sa, sb, "span must look like 30s, 5m, 1h or 1d");
                };
                let mut agg = Agg {
                    func: AggFn::Count,
                    col: None,
                };
                if self.peek().is_some_and(
                    |t| matches!(&t.tok, Tok::Word(w) if AggFn::from_name(w).is_some()),
                ) {
                    agg = self.agg(sb)?;
                }
                let mut by = None;
                if self.peek().is_some_and(|t| is_kw(t, "by")) {
                    let bb = self.peek().unwrap().b;
                    self.i += 1;
                    by = Some(self.word("a column name", bb)?.0);
                }
                Ok(Pipe::Timechart { span_us, agg, by })
            }
            "sort" => {
                let fields = self.field_list(b)?;
                Ok(Pipe::Sort(
                    fields
                        .into_iter()
                        .map(|f| match f.strip_prefix('-') {
                            Some(r) => (r.to_string(), true),
                            None => (f.strip_prefix('+').unwrap_or(&f).to_string(), false),
                        })
                        .collect(),
                ))
            }
            "fields" => Ok(Pipe::Fields(self.field_list(b)?)),
            "head" => {
                let (n, na, nb) = self.word("a number", b)?;
                n.parse()
                    .map(Pipe::Head)
                    .or_else(|_| err(na, nb, "head needs a number"))
            }
            _ => err(
                a,
                b,
                "unknown command (use where, stats, top, timechart, sort, fields, head)",
            ),
        }
    }
}

pub fn parse_span(s: &str) -> Option<i64> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
    let n: i64 = num.parse().ok()?;
    let mult = match unit {
        "ms" => 1_000,
        "s" | "sec" => 1_000_000,
        "m" | "min" => 60_000_000,
        "h" | "hr" => 3_600_000_000,
        "d" | "day" => 86_400_000_000,
        "w" => 7 * 86_400_000_000,
        _ => return None,
    };
    (n > 0).then_some(n * mult)
}

/// Minimal syntax check so obvious regex typos are underlined while typing;
/// full compilation (with size limits) happens in the executor.
fn regex_syntax_ok(p: &str) -> Result<(), String> {
    let mut depth = 0i32;
    let mut cls = false;
    let mut esc = false;
    for c in p.chars() {
        if esc {
            esc = false;
            continue;
        }
        match c {
            '\\' => esc = true,
            '[' if !cls => cls = true,
            ']' if cls => cls = false,
            '(' if !cls => depth += 1,
            ')' if !cls => {
                depth -= 1;
                if depth < 0 {
                    return Err("unbalanced )".into());
                }
            }
            _ => {}
        }
    }
    if depth != 0 {
        Err("unbalanced (".into())
    } else if cls {
        Err("unclosed [".into())
    } else {
        Ok(())
    }
}

pub fn parse(src: &str) -> Result<Query, ParseError> {
    let t = lex(src)?;
    let mut p = P { src, t, i: 0 };
    p.query()
}
