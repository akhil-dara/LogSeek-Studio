//! Formula columns: a small closed expression language — parsed and evaluated
//! by LogSeek, never executed as code; no file or network access. Regex uses
//! the `regex` crate (linear time, size-limited), so no pattern can hang.

mod funcs;
pub use funcs::number_as_time;

use crate::segment::fmt_float;
use crate::types::{fmt_time, ColType};
use std::sync::Arc;

pub use funcs::FUNCTIONS;

#[derive(Debug, Clone, PartialEq)]
pub enum FValue {
    Null,
    Text(String),
    Num(f64),
    Bool(bool),
    /// Epoch µs.
    Time(i64),
    /// Could not be worked out, and why (in words). Not an empty value: a
    /// missing value stays `Null`.
    Error(String),
}

impl FValue {
    pub fn text(&self) -> Option<String> {
        match self {
            FValue::Null => None,
            FValue::Text(s) => Some(s.clone()),
            FValue::Num(n) => Some(fmt_float(*n)),
            FValue::Bool(b) => Some(b.to_string()),
            FValue::Time(t) => Some(fmt_time(*t)),
            FValue::Error(_) => None,
        }
    }
    /// The reason, when this is an error.
    pub fn error(&self) -> Option<&str> {
        match self {
            FValue::Error(e) => Some(e),
            _ => None,
        }
    }
    pub fn num(&self) -> Option<f64> {
        match self {
            FValue::Num(n) => Some(*n),
            FValue::Time(t) => Some(*t as f64),
            FValue::Bool(b) => Some(*b as u8 as f64),
            FValue::Text(s) => s.trim().parse().ok(),
            FValue::Null | FValue::Error(_) => None,
        }
    }
    pub fn truthy(&self) -> bool {
        match self {
            FValue::Null => false,
            FValue::Bool(b) => *b,
            FValue::Num(n) => *n != 0.0 && !n.is_nan(),
            FValue::Text(s) => !s.is_empty(),
            FValue::Time(_) => true,
            FValue::Error(_) => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FormulaError {
    pub span: (usize, usize),
    pub msg: String,
}

fn ferr<T>(a: usize, b: usize, msg: impl Into<String>) -> Result<T, FormulaError> {
    Err(FormulaError {
        span: (a, b),
        msg: msg.into(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Cat,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Coalesce,
}

#[derive(Debug, Clone)]
pub(crate) enum Node {
    Lit(FValue),
    Col(usize),
    Neg(Box<Node>),
    Not(Box<Node>),
    Bin(Op, Box<Node>, Box<Node>),
    Call(&'static str, Vec<Node>, Option<Arc<regex::Regex>>),
}

/// A compiled formula.
#[derive(Debug, Clone)]
pub struct Formula {
    src: String,
    root: Node,
    ty: ColType,
    refs: Vec<usize>,
    /// Columns it was compiled against; `Col(i)` with `i >= base` is slot `i - base`.
    base: usize,
    /// Made columns it uses, inlined: each before any that uses it.
    slots: Vec<Slot>,
    /// Times it was worked out (shared by copies of it).
    evals: Arc<std::sync::atomic::AtomicU64>,
    /// The stored columns its names resolved to (name and type), as a hash.
    binding: u64,
}

/// A made column inlined into a formula that uses it.
#[derive(Debug, Clone)]
struct Slot {
    name: String,
    src: String,
    ty: ColType,
    node: Node,
}

impl Formula {
    pub fn source(&self) -> &str {
        &self.src
    }
    pub fn result_type(&self) -> ColType {
        self.ty
    }
    /// Columns the formula reads (indices into the column list it was compiled against).
    pub fn refs(&self) -> &[usize] {
        &self.refs
    }
    /// Evaluate for one row; `get(col)` supplies column values. Each made
    /// column it uses is worked out once for the row, however often it is named.
    pub fn eval(&self, get: &dyn Fn(usize) -> FValue) -> FValue {
        self.evals
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if self.slots.is_empty() {
            return eval(&self.root, get);
        }
        let memo = std::cell::RefCell::new(vec![None; self.slots.len()]);
        self.eval_node(&self.root, get, &memo)
    }
    fn eval_node(
        &self,
        n: &Node,
        get: &dyn Fn(usize) -> FValue,
        memo: &std::cell::RefCell<Vec<Option<FValue>>>,
    ) -> FValue {
        let g = |i: usize| -> FValue {
            if i < self.base {
                return get(i);
            }
            let k = i - self.base;
            let known = memo.borrow()[k].clone();
            if let Some(v) = known {
                return v;
            }
            let v = self.eval_node(&self.slots[k].node, get, memo);
            memo.borrow_mut()[k] = Some(v.clone());
            v
        };
        eval(n, &g)
    }
    /// How many times it has been worked out (once per row, or once per
    /// distinct input when its inputs have few values).
    pub fn evals(&self) -> u64 {
        self.evals.load(std::sync::atomic::Ordering::Relaxed)
    }
    /// Changes whenever the formula or any made column it uses changes.
    pub fn fingerprint(&self) -> u64 {
        // FNV-1a: the same on every run
        let mut h: u64 = 0xcbf29ce484222325;
        let mut eat = |b: &[u8]| {
            for &x in b {
                h ^= x as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
            h ^= 0xff;
            h = h.wrapping_mul(0x100000001b3);
        };
        eat(self.src.as_bytes());
        eat(&self.binding.to_le_bytes());
        for sl in &self.slots {
            eat(sl.name.as_bytes());
            eat(sl.src.as_bytes());
        }
        h
    }
    /// The made columns it uses, by name.
    pub fn uses(&self) -> Vec<&str> {
        self.slots.iter().map(|s| s.name.as_str()).collect()
    }
}

/// `n` with slot numbers of another formula moved to this one's (`map[j]` = new slot of old slot j).
fn remap(n: &Node, base: usize, map: &[usize]) -> Node {
    let r = |x: &Node| Box::new(remap(x, base, map));
    match n {
        Node::Col(i) if *i >= base => Node::Col(base + map[*i - base]),
        Node::Lit(_) | Node::Col(_) => n.clone(),
        Node::Neg(x) => Node::Neg(r(x)),
        Node::Not(x) => Node::Not(r(x)),
        Node::Bin(op, a, b) => Node::Bin(*op, r(a), r(b)),
        Node::Call(name, args, re) => Node::Call(
            name,
            args.iter().map(|a| remap(a, base, map)).collect(),
            re.clone(),
        ),
    }
}

/// `src` with every reference to column `old` (any case, plain or quoted)
/// written as `new`, quoted when it needs to be; texts, functions and other
/// columns as they were. Unreadable formulas come back unchanged.
pub fn rename_refs(src: &str, old: &str, new: &str) -> String {
    let Ok(t) = lex(src) else {
        return src.to_string();
    };
    let mut out = String::with_capacity(src.len());
    let mut at = 0;
    for (k, (tok, a, b)) in t.iter().enumerate() {
        let hit = match tok {
            Tok::Col(w) => w.eq_ignore_ascii_case(old),
            Tok::Ident(w) => {
                w.eq_ignore_ascii_case(old) && !matches!(t.get(k + 1), Some((Tok::LParen, ..)))
            }
            _ => false,
        };
        if hit {
            out.push_str(&src[at..*a]);
            out.push_str(&column_ref(new));
            at = *b;
        }
    }
    out.push_str(&src[at..]);
    out
}
/// Names among `candidates` that `src` refers to as columns (not as functions).
pub fn names_used<'c>(src: &str, candidates: &'c [String]) -> Vec<&'c str> {
    let Ok(t) = lex(src) else { return Vec::new() };
    let mut out: Vec<&str> = Vec::new();
    for (k, (tok, ..)) in t.iter().enumerate() {
        let w = match tok {
            Tok::Col(w) => w,
            Tok::Ident(w) if !matches!(t.get(k + 1), Some((Tok::LParen, ..))) => w,
            _ => continue,
        };
        let hit = candidates
            .iter()
            .find(|c| *c == w)
            .or_else(|| candidates.iter().find(|c| c.eq_ignore_ascii_case(w)));
        if let Some(c) = hit {
            if !out.contains(&c.as_str()) {
                out.push(c);
            }
        }
    }
    out
}

// ───────────────────────── lexer ─────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Ident(String),
    Col(String),
    Op(&'static str),
    LParen,
    RParen,
    Comma,
}

fn lex(s: &str) -> Result<Vec<(Tok, usize, usize)>, FormulaError> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let st = i;
        match c {
            b'(' => {
                out.push((Tok::LParen, i, i + 1));
                i += 1;
            }
            b')' => {
                out.push((Tok::RParen, i, i + 1));
                i += 1;
            }
            b',' => {
                out.push((Tok::Comma, i, i + 1));
                i += 1;
            }
            b'"' => {
                i += 1;
                let mut v = String::new();
                let mut closed = false;
                while i < b.len() {
                    match b[i] {
                        b'\\' if matches!(b.get(i + 1), Some(b'"') | Some(b'\\')) => {
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
                    return ferr(st, b.len(), "missing closing quote \"");
                }
                out.push((Tok::Str(v), st, i));
            }
            b'`' => {
                let Some((name, used)) = backtick_name(&s[i..]) else {
                    return ferr(st, b.len(), "missing closing backtick `");
                };
                out.push((Tok::Col(name), st, i + used));
                i += used;
            }
            b'0'..=b'9' => {
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                    i += 1;
                }
                if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
                    let mut j = i + 1;
                    if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
                        j += 1;
                    }
                    if j < b.len() && b[j].is_ascii_digit() {
                        i = j;
                        while i < b.len() && b[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                }
                let v: f64 = s[st..i].parse().map_err(|_| FormulaError {
                    span: (st, i),
                    msg: "invalid number".into(),
                })?;
                out.push((Tok::Num(v), st, i));
            }
            _ => {
                let two = s.get(i..i + 2).unwrap_or("");
                if let Some(op) = ["==", "!=", "<=", ">=", "??"].iter().find(|o| **o == two) {
                    out.push((Tok::Op(op), i, i + 2));
                    i += 2;
                    continue;
                }
                if let Some(op) = ["+", "-", "*", "/", "%", "&", "<", ">", "!", "="]
                    .iter()
                    .find(|o| o.as_bytes()[0] == c)
                {
                    let op: &'static str = if *op == "=" { "==" } else { op };
                    out.push((Tok::Op(op), i, i + 1));
                    i += 1;
                    continue;
                }
                if c.is_ascii_alphabetic() || matches!(c, b'_' | b'@' | b'#') || c >= 0x80 {
                    while i < b.len() {
                        let ch = s[i..].chars().next().unwrap();
                        if ch.is_alphanumeric() || matches!(ch, '_' | '@' | '#' | '.' | '[' | ']') {
                            i += ch.len_utf8();
                        } else {
                            break;
                        }
                    }
                    out.push((Tok::Ident(s[st..i].to_string()), st, i));
                } else {
                    return ferr(i, i + 1, format!("unexpected character {:?}", c as char));
                }
            }
        }
    }
    Ok(out)
}

// ───────────────────────── parser ─────────────────────────

pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (
        a.to_lowercase().chars().collect(),
        b.to_lowercase().chars().collect(),
    );
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            cur[j] = (prev[j] + 1)
                .min(cur[j - 1] + 1)
                .min(prev[j - 1] + usize::from(a[i - 1] != b[j - 1]));
        }
        prev = cur;
    }
    prev[b.len()]
}

struct P<'a> {
    t: Vec<(Tok, usize, usize)>,
    i: usize,
    cols: &'a [(String, ColType)],
    end: usize,
    refs: Vec<usize>,
    /// Made columns that may be named, and those named so far (inlined).
    made: &'a [(String, Formula)],
    slots: Vec<Slot>,
}

fn prec(op: &str) -> Option<(u8, Op)> {
    Some(match op {
        "??" => (1, Op::Coalesce),
        "or" => (2, Op::Or),
        "and" => (3, Op::And),
        "==" => (5, Op::Eq),
        "!=" => (5, Op::Ne),
        "<" => (5, Op::Lt),
        "<=" => (5, Op::Le),
        ">" => (5, Op::Gt),
        ">=" => (5, Op::Ge),
        "&" => (6, Op::Cat),
        "+" => (7, Op::Add),
        "-" => (7, Op::Sub),
        "*" => (8, Op::Mul),
        "/" => (8, Op::Div),
        "%" => (8, Op::Mod),
        _ => return None,
    })
}

impl<'a> P<'a> {
    fn peek_op(&self) -> Option<(u8, Op, usize)> {
        match self.t.get(self.i) {
            Some((Tok::Op(o), ..)) => prec(o).map(|(p, op)| (p, op, 1)),
            Some((Tok::Ident(w), ..)) => {
                let l = w.to_ascii_lowercase();
                if l == "and" || l == "or" {
                    prec(&l).map(|(p, op)| (p, op, 1))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn expr(&mut self, min: u8) -> Result<Node, FormulaError> {
        let mut lhs = self.prefix()?;
        while let Some((p, op, _)) = self.peek_op() {
            if p < min {
                break;
            }
            self.i += 1;
            let rhs = self.expr(p + 1)?;
            lhs = Node::Bin(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn prefix(&mut self) -> Result<Node, FormulaError> {
        let Some((tok, a, b)) = self.t.get(self.i).cloned() else {
            return ferr(self.end, self.end, "expression ends too early");
        };
        self.i += 1;
        match tok {
            Tok::Num(n) => Ok(Node::Lit(FValue::Num(n))),
            Tok::Str(s) => Ok(Node::Lit(FValue::Text(s))),
            Tok::Op("-") => Ok(Node::Neg(Box::new(self.expr(9)?))),
            Tok::Op("!") => Ok(Node::Not(Box::new(self.expr(4)?))),
            Tok::LParen => {
                let e = self.expr(0)?;
                match self.t.get(self.i) {
                    Some((Tok::RParen, ..)) => {
                        self.i += 1;
                        Ok(e)
                    }
                    _ => ferr(a, self.end, "missing closing )"),
                }
            }
            Tok::Col(name) => self.column(&name, a, b),
            Tok::Ident(w) => {
                let l = w.to_ascii_lowercase();
                match l.as_str() {
                    "true" => return Ok(Node::Lit(FValue::Bool(true))),
                    "false" => return Ok(Node::Lit(FValue::Bool(false))),
                    "null" => return Ok(Node::Lit(FValue::Null)),
                    "not" => return Ok(Node::Not(Box::new(self.expr(4)?))),
                    _ => {}
                }
                if matches!(self.t.get(self.i), Some((Tok::LParen, ..))) {
                    self.call(&l, a, b)
                } else {
                    self.column(&w, a, b)
                }
            }
            other => ferr(a, b, format!("unexpected {}", tok_name(&other))),
        }
    }

    fn column(&mut self, name: &str, a: usize, b: usize) -> Result<Node, FormulaError> {
        let i = self.cols.iter().position(|(n, _)| n == name).or_else(|| {
            self.cols
                .iter()
                .position(|(n, _)| n.eq_ignore_ascii_case(name))
        });
        if i.is_none() {
            let m = self.made.iter().position(|(n, _)| n == name).or_else(|| {
                self.made
                    .iter()
                    .position(|(n, _)| n.eq_ignore_ascii_case(name))
            });
            if let Some(m) = m {
                let k = self.import(m);
                return Ok(Node::Col(self.cols.len() + k));
            }
        }
        match i {
            Some(i) => {
                if !self.refs.contains(&i) {
                    self.refs.push(i);
                }
                Ok(Node::Col(i))
            }
            None => {
                let best = self
                    .cols
                    .iter()
                    .map(|(n, _)| n)
                    .chain(self.made.iter().map(|(n, _)| n))
                    .min_by_key(|n| edit_distance(n, name))
                    .cloned();
                ferr(
                    a,
                    b,
                    match best {
                        Some(n) => format!("unknown column {name} — did you mean {n}?"),
                        None => format!("unknown column {name}"),
                    },
                )
            }
        }
    }

    /// Inline made column `m` (and the made columns it uses) as slots; its slot.
    fn import(&mut self, m: usize) -> usize {
        let (name, f) = (&self.made[m].0, &self.made[m].1);
        if let Some(k) = self.slots.iter().position(|s| s.name == *name) {
            return k;
        }
        let base = self.cols.len();
        let mut map = Vec::with_capacity(f.slots.len());
        for sl in &f.slots {
            let k = match self.slots.iter().position(|s| s.name == sl.name) {
                Some(k) => k,
                None => {
                    self.slots.push(Slot {
                        name: sl.name.clone(),
                        src: sl.src.clone(),
                        ty: sl.ty,
                        node: remap(&sl.node, base, &map),
                    });
                    self.slots.len() - 1
                }
            };
            map.push(k);
        }
        for &r in &f.refs {
            if !self.refs.contains(&r) {
                self.refs.push(r);
            }
        }
        self.slots.push(Slot {
            name: name.clone(),
            src: f.src.clone(),
            ty: f.ty,
            node: remap(&f.root, base, &map),
        });
        self.slots.len() - 1
    }

    fn call(&mut self, name: &str, a: usize, b: usize) -> Result<Node, FormulaError> {
        let Some(spec) = FUNCTIONS.iter().find(|f| f.name == name) else {
            let best = FUNCTIONS
                .iter()
                .min_by_key(|f| edit_distance(f.name, name))
                .map(|f| f.name)
                .unwrap_or("");
            return ferr(
                a,
                b,
                format!("unknown function {name}() — did you mean {best}()?"),
            );
        };
        self.i += 1; // (
        let mut args = Vec::new();
        if !matches!(self.t.get(self.i), Some((Tok::RParen, ..))) {
            loop {
                args.push(self.expr(0)?);
                match self.t.get(self.i) {
                    Some((Tok::Comma, ..)) => self.i += 1,
                    Some((Tok::RParen, ..)) => break,
                    _ => return ferr(a, self.end, format!("missing ) after {name}(")),
                }
            }
        }
        self.i += 1; // )
        if args.len() < spec.min || spec.max.is_some_and(|m| args.len() > m) {
            return ferr(a, b, format!("{}() takes {}", name, spec.arity_text()));
        }
        let mut re = None;
        if spec.regex_arg {
            match args.get(1) {
                Some(Node::Lit(FValue::Text(p))) => {
                    let r = regex::RegexBuilder::new(p)
                        .size_limit(4 << 20)
                        .dfa_size_limit(4 << 20)
                        .build()
                        .map_err(|e| FormulaError {
                            span: (a, b),
                            msg: format!("invalid regex: {e}"),
                        })?;
                    re = Some(Arc::new(r));
                }
                _ => {
                    return ferr(
                        a,
                        b,
                        format!("{name}(): the pattern must be a quoted string"),
                    )
                }
            }
        }
        Ok(Node::Call(spec.name, args, re))
    }
}

fn tok_name(t: &Tok) -> String {
    match t {
        Tok::Num(_) => "number".into(),
        Tok::Str(_) => "string".into(),
        Tok::Ident(w) | Tok::Col(w) => format!("`{w}`"),
        Tok::Op(o) => format!("'{o}'"),
        Tok::LParen => "'('".into(),
        Tok::RParen => "')'".into(),
        Tok::Comma => "','".into(),
    }
}

fn infer_type(n: &Node, cols: &[(String, ColType)]) -> ColType {
    match n {
        Node::Lit(FValue::Num(_)) => ColType::Float,
        Node::Lit(FValue::Bool(_)) => ColType::Bool,
        Node::Lit(FValue::Time(_)) => ColType::Time,
        Node::Lit(_) => ColType::Text,
        Node::Col(i) => cols[*i].1,
        Node::Neg(_) => ColType::Float,
        Node::Not(_) => ColType::Bool,
        Node::Bin(op, l, _) => match op {
            Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Mod => ColType::Float,
            Op::Cat => ColType::Text,
            Op::Coalesce => infer_type(l, cols),
            _ => ColType::Bool,
        },
        Node::Call(name, args, _) => match funcs::ret(name) {
            Some(t) => t,
            None => args
                .get(if *name == "if" { 1 } else { 0 })
                .map_or(ColType::Text, |a| infer_type(a, cols)),
        },
    }
}

pub fn compile(src: &str, cols: &[(String, ColType)]) -> Result<Formula, FormulaError> {
    compile_with(src, cols, &[])
}

/// As [`compile`], where the names in `made` may be used like columns: each is
/// inlined (with the made columns it uses), and worked out once per row.
pub fn compile_with(
    src: &str,
    cols: &[(String, ColType)],
    made: &[(String, Formula)],
) -> Result<Formula, FormulaError> {
    if made.iter().any(|(_, f)| f.base != cols.len()) {
        return ferr(0, 0, "made columns compiled against other columns");
    }
    let t = lex(src)?;
    if t.is_empty() {
        return ferr(0, 0, "empty formula");
    }
    let mut p = P {
        t,
        i: 0,
        cols,
        end: src.len(),
        refs: Vec::new(),
        made,
        slots: Vec::new(),
    };
    let root = p.expr(0)?;
    if let Some((tok, a, b)) = p.t.get(p.i) {
        return ferr(
            *a,
            *b,
            format!("unexpected {} — missing an operator?", tok_name(tok)),
        );
    }
    // slots read as columns after the stored ones, typed as their formulas
    let mut ext: Vec<(String, ColType)> = cols.to_vec();
    ext.extend(p.slots.iter().map(|s| (s.name.clone(), s.ty)));
    let ty = infer_type(&root, &ext);
    Ok(Formula {
        src: src.to_string(),
        root,
        ty,
        base: cols.len(),
        binding: {
            // which stored columns the names landed on: the same text over other
            // columns is another formula
            let mut h: u64 = 0xcbf29ce484222325;
            for &r in &p.refs {
                let (n, t) = &cols[r];
                for b in n.bytes().chain([0xff, *t as u8, 0xfe]) {
                    h ^= b as u64;
                    h = h.wrapping_mul(0x100000001b3);
                }
            }
            h
        },
        refs: p.refs,
        slots: p.slots,
        evals: Default::default(),
    })
}

// ───────────────────────── evaluation ─────────────────────────

fn cmp_values(a: &FValue, b: &FValue) -> Option<std::cmp::Ordering> {
    // a missing value and an error compare with nothing
    let none = |v: &FValue| matches!(v, FValue::Null | FValue::Error(_));
    if none(a) || none(b) {
        return None;
    }
    let numeric = |v: &FValue| matches!(v, FValue::Num(_) | FValue::Time(_) | FValue::Bool(_));
    if numeric(a) || numeric(b) {
        if let (Some(x), Some(y)) = (a.num(), b.num()) {
            return x.partial_cmp(&y);
        }
    }
    Some(a.text()?.cmp(&b.text()?))
}

pub(crate) fn eval(n: &Node, get: &dyn Fn(usize) -> FValue) -> FValue {
    match n {
        Node::Lit(v) => v.clone(),
        Node::Col(i) => get(*i),
        Node::Neg(x) => match eval(x, get) {
            e @ FValue::Error(_) => e,
            v => v.num().map_or(FValue::Null, |v| FValue::Num(-v)),
        },
        Node::Not(x) => match eval(x, get) {
            // an error stays one; "not (x > 5)" is still true, the comparison being false
            e @ FValue::Error(_) => e,
            v => FValue::Bool(!v.truthy()),
        },
        Node::Bin(op, l, r) => {
            match op {
                // an error is passed on, unless the side before it already decided
                Op::And | Op::Or => {
                    let a = eval(l, get);
                    if let FValue::Error(_) = a {
                        return a;
                    }
                    if a.truthy() == (*op == Op::Or) {
                        return FValue::Bool(a.truthy());
                    }
                    let b = eval(r, get);
                    if let FValue::Error(_) = b {
                        return b;
                    }
                    return FValue::Bool(b.truthy());
                }
                Op::Coalesce => {
                    // the first that is neither missing nor an error
                    let v = eval(l, get);
                    return if matches!(v, FValue::Null | FValue::Error(_)) {
                        eval(r, get)
                    } else {
                        v
                    };
                }
                _ => {}
            }
            let (a, b) = (eval(l, get), eval(r, get));
            use std::cmp::Ordering as O;
            // joining and arithmetic pass an error on, reason and all; comparing
            // with one is never true
            let compares = matches!(op, Op::Eq | Op::Ne | Op::Lt | Op::Le | Op::Gt | Op::Ge);
            if !compares {
                if let FValue::Error(_) = a {
                    return a;
                }
                if let FValue::Error(_) = b {
                    return b;
                }
            }
            match op {
                Op::Cat => {
                    // the left text grows in place
                    let mut s = match a {
                        FValue::Text(s) => s,
                        other => other.text().unwrap_or_default(),
                    };
                    match &b {
                        FValue::Text(t) => s.push_str(t),
                        other => s.push_str(&other.text().unwrap_or_default()),
                    }
                    FValue::Text(s)
                }
                Op::Eq => FValue::Bool(cmp_values(&a, &b) == Some(O::Equal)),
                Op::Ne => FValue::Bool(cmp_values(&a, &b).is_some_and(|o| o != O::Equal)),
                Op::Lt => FValue::Bool(cmp_values(&a, &b) == Some(O::Less)),
                Op::Le => FValue::Bool(matches!(cmp_values(&a, &b), Some(O::Less | O::Equal))),
                Op::Gt => FValue::Bool(cmp_values(&a, &b) == Some(O::Greater)),
                Op::Ge => FValue::Bool(matches!(cmp_values(&a, &b), Some(O::Greater | O::Equal))),
                _ => {
                    let (Some(x), Some(y)) = (a.num(), b.num()) else {
                        return FValue::Null;
                    };
                    let v = match op {
                        Op::Add => x + y,
                        Op::Sub => x - y,
                        Op::Mul => x * y,
                        Op::Div if y != 0.0 => x / y,
                        Op::Mod if y != 0.0 => x % y,
                        Op::Div | Op::Mod => return FValue::Error("division by zero".into()),
                        _ => return FValue::Null,
                    };
                    FValue::Num(v)
                }
            }
        }
        Node::Call(name, args, re) => funcs::call(name, args, re.as_deref(), get),
    }
}

// ───────────────────────── steps ─────────────────────────

/// A no-code transformation step; a list of steps compiles to formula text.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Regex { pattern: String, group: usize },
    Split { sep: String, index: i64 },
    Json { path: String },
    Trim,
    Lower,
    Upper,
    Base64 { utf16: bool },
    Hex,
    Replace { from: String, to: String },
    Date { unit: String },
    UrlHost,
    PathFile,
}

pub fn quote(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        if c == '"' || c == '\\' {
            o.push('\\');
        }
        o.push(c);
    }
    o.push('"');
    o
}

pub fn column_ref(name: &str) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || matches!(c, '_' | '@' | '#'))
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '@' | '#' | '.' | '[' | ']'))
        && !matches!(
            name.to_ascii_lowercase().as_str(),
            "and" | "or" | "not" | "true" | "false" | "null"
        );
    if plain {
        name.to_string()
    } else {
        // a backtick inside the name is written twice, so the name can hold one
        format!("`{}`", name.replace('`', "``"))
    }
}

/// A column name written between backticks at the start of `s`: the name,
/// and how many bytes it took. Inside, two backticks stand for one, so any
/// name can be written. `None` when the closing backtick is missing.
pub fn backtick_name(s: &str) -> Option<(String, usize)> {
    let b = s.as_bytes();
    if b.first() != Some(&b'`') {
        return None;
    }
    let mut name = String::new();
    let mut i = 1;
    let mut from = 1;
    while i < b.len() {
        if b[i] == b'`' {
            name.push_str(&s[from..i]);
            if b.get(i + 1) == Some(&b'`') {
                name.push('`');
                i += 2;
                from = i;
                continue;
            }
            return Some((name, i + 1));
        }
        i += 1;
    }
    None
}

pub fn steps_to_formula(col: &str, steps: &[Step]) -> String {
    let mut f = column_ref(col);
    for s in steps {
        f = match s {
            Step::Regex { pattern, group } => format!("regex({f}, {}, {group})", quote(pattern)),
            Step::Split { sep, index } => format!("split({f}, {}, {index})", quote(sep)),
            Step::Json { path } => format!("json({f}, {})", quote(path)),
            Step::Trim => format!("trim({f})"),
            Step::Lower => format!("lower({f})"),
            Step::Upper => format!("upper({f})"),
            Step::Base64 { utf16: true } => format!("base64_decode({f}, \"utf16\")"),
            Step::Base64 { utf16: false } => format!("base64_decode({f})"),
            Step::Hex => format!("hex_decode({f})"),
            Step::Replace { from, to } => format!("replace({f}, {}, {})", quote(from), quote(to)),
            Step::Date { unit } => format!("date({f}, {})", quote(unit)),
            Step::UrlHost => format!("url_host({f})"),
            Step::PathFile => format!("path_file({f})"),
        };
    }
    f
}
