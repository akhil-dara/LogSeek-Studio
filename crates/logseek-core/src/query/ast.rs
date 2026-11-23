//! Syntax tree of the app's query language.

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Query {
    pub filter: Option<Expr>,
    /// Inclusive start, exclusive end, epoch µs.
    pub time: Option<(i64, i64)>,
    pub pipes: Vec<Pipe>,
}

impl Query {
    /// The query for showing rows: each `| where` joins the filter (AND); a
    /// step rows cannot show (stats, top, sort, ...) is an error in words,
    /// never silently ignored.
    pub fn rows_only(mut self) -> Result<Query, String> {
        for p in std::mem::take(&mut self.pipes) {
            match p {
                Pipe::Where(e) => {
                    self.filter = Some(match self.filter.take() {
                        Some(f) => Expr::And(Box::new(f), Box::new(e)),
                        None => e,
                    });
                }
                other => {
                    let name = format!("{other:?}");
                    let step = name
                        .split(|c: char| !c.is_alphanumeric())
                        .next()
                        .unwrap_or("")
                        .to_lowercase();
                    return Err(format!(
                        "| {step} is not shown in the rows: use Summarise for counts and groups, and the column menu to sort"
                    ));
                }
            }
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Term(Term),
}

/// The column name that stands for "every field" in a regex term (`~/x/`).
pub const ANY_FIELD: &str = "*";

#[derive(Debug, Clone, PartialEq)]
pub enum Term {
    /// Case-insensitive substring in any column.
    FreeText(String),
    Cmp {
        col: String,
        op: CmpOp,
        value: String,
    },
    /// `iserror(col)`: a made column's value could not be worked out.
    IsError(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    /// `col:value` — case-insensitive substring.
    Contains,
    /// `col:pat*` — case-insensitive wildcard match.
    Glob,
    /// `col~/re/`
    Regex,
}

impl CmpOp {
    pub fn symbol(self) -> &'static str {
        match self {
            CmpOp::Eq => "=",
            CmpOp::Ne => "!=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Contains | CmpOp::Glob => ":",
            CmpOp::Regex => "~",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggFn {
    Count,
    Dc,
    Sum,
    Avg,
    Min,
    Max,
}

impl AggFn {
    pub fn name(self) -> &'static str {
        match self {
            AggFn::Count => "count",
            AggFn::Dc => "dc",
            AggFn::Sum => "sum",
            AggFn::Avg => "avg",
            AggFn::Min => "min",
            AggFn::Max => "max",
        }
    }
    pub fn from_name(s: &str) -> Option<AggFn> {
        Some(match s.to_ascii_lowercase().as_str() {
            "count" => AggFn::Count,
            "dc" | "distinct_count" => AggFn::Dc,
            "sum" => AggFn::Sum,
            "avg" | "mean" => AggFn::Avg,
            "min" => AggFn::Min,
            "max" => AggFn::Max,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Agg {
    pub func: AggFn,
    pub col: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pipe {
    Where(Expr),
    Stats {
        aggs: Vec<Agg>,
        by: Vec<String>,
    },
    Top {
        n: usize,
        col: String,
    },
    Timechart {
        span_us: i64,
        agg: Agg,
        by: Option<String>,
    },
    /// `(column, descending)`
    Sort(Vec<(String, bool)>),
    Fields(Vec<String>),
    Head(usize),
}
