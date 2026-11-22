//! Add-column recipes: each small form compiles to formula text, so a new
//! column is a virtual column the engine evaluates (the file is unchanged).

use crate::colors::RuleOp;
use logseek_core::formula::{column_ref, quote, FValue};
use logseek_core::types::ColType;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Piece {
    First,
    Last,
    /// 1-based, counted from the start.
    Nth(u32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Case {
    Keep,
    Upper,
    Lower,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DatePartKind {
    Year,
    Month,
    Day,
    Weekday,
    Hour,
    Minute,
    Date,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Decode {
    Base64,
    Base64Utf16,
    Hex,
    UrlHost,
    FileName,
    Folder,
    Extension,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MathOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Operand {
    Number(f64),
    Column(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Measure {
    Length,
    Entropy,
}

/// One add-column recipe, as the form describes it.
#[derive(Clone, Debug, PartialEq)]
pub enum Recipe {
    Split {
        col: String,
        sep: String,
        piece: Piece,
    },
    Combine {
        cols: Vec<String>,
        sep: String,
    },
    Clean {
        col: String,
        trim: bool,
        case: Case,
        replace: Option<(String, String)>,
    },
    /// Rows of (column, condition, value, label); the first rule that matches gives the label.
    Classify {
        rules: Vec<(String, RuleOp, String, String)>,
        otherwise: String,
    },
    DatePart {
        col: String,
        part: DatePartKind,
    },
    Decode {
        col: String,
        how: Decode,
    },
    Maths {
        col: String,
        op: MathOp,
        other: Operand,
        round: Option<u8>,
    },
    Measure {
        col: String,
        what: Measure,
    },
    ToDate {
        col: String,
        unit: String,
    },
    FirstNonEmpty {
        cols: Vec<String>,
    },
    /// One field of a column that holds JSON text (`a.b`, `[0].c`, `[*].c`).
    Json {
        col: String,
        path: String,
    },
}

fn num_lit(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// The condition of one Classify row, as a formula expression.
fn condition(c: &str, op: RuleOp, value: &str) -> String {
    let v = value.trim();
    let is = |v: &str| match v.parse::<f64>() {
        Ok(n) => format!("num({c}) == {}", num_lit(n)),
        Err(_) => format!("lower({c}) == {}", quote(&v.to_lowercase())),
    };
    let any = |f: &dyn Fn(&str) -> String| {
        let parts: Vec<String> = v
            .split(',')
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .map(f)
            .collect();
        if parts.is_empty() {
            "false".to_string()
        } else {
            format!("({})", parts.join(" or "))
        }
    };
    match op {
        RuleOp::Contains => format!("contains({c}, {})", quote(v)),
        RuleOp::ContainsAny => any(&|w| format!("contains({c}, {})", quote(w))),
        RuleOp::Is => is(v),
        RuleOp::IsAny => any(&is),
        RuleOp::StartsWith => format!("startswith({c}, {})", quote(v)),
        RuleOp::Regex => format!(
            "if(isnull(regex({c}, {})), false, true)",
            quote(&format!("(?i){v}"))
        ),
        RuleOp::Gt => format!(
            "num({c}) > {}",
            v.parse::<f64>()
                .map(num_lit)
                .unwrap_or_else(|_| "null".into())
        ),
        RuleOp::Lt => format!(
            "num({c}) < {}",
            v.parse::<f64>()
                .map(num_lit)
                .unwrap_or_else(|_| "null".into())
        ),
    }
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

impl Recipe {
    /// The formula with plain column names (shown to the user).
    pub fn formula(&self) -> String {
        self.formula_with(&|c| column_ref(c))
    }

    /// The formula, with each column reference written by `r` (so a column
    /// that is itself a formula can be written in place for the engine).
    pub fn formula_with(&self, r: &dyn Fn(&str) -> String) -> String {
        match self {
            Recipe::Split { col, sep, piece } => {
                let i: i64 = match piece {
                    Piece::First => 0,
                    Piece::Last => -1,
                    Piece::Nth(n) => (*n as i64 - 1).max(0),
                };
                format!("split({}, {}, {i})", r(col), quote(sep))
            }
            Recipe::Combine { cols, sep } => {
                let parts: Vec<String> = cols.iter().map(|c| r(c)).collect();
                format!("concat({})", parts.join(&format!(", {}, ", quote(sep))))
            }
            Recipe::Clean {
                col,
                trim,
                case,
                replace,
            } => {
                let mut f = r(col);
                if *trim {
                    f = format!("trim({f})");
                }
                f = match case {
                    Case::Keep => f,
                    Case::Upper => format!("upper({f})"),
                    Case::Lower => format!("lower({f})"),
                };
                if let Some((a, b)) = replace.as_ref().filter(|(a, _)| !a.is_empty()) {
                    f = format!("replace({f}, {}, {})", quote(a), quote(b));
                }
                f
            }
            Recipe::Classify { rules, otherwise } => {
                if rules.is_empty() {
                    return quote(otherwise);
                }
                let mut args: Vec<String> = Vec::new();
                for (c, op, v, label) in rules {
                    args.push(condition(&r(c), *op, v));
                    args.push(quote(label));
                }
                args.push(quote(otherwise));
                format!("case({})", args.join(", "))
            }
            Recipe::DatePart { col, part } => {
                let c = r(col);
                match part {
                    DatePartKind::Year => format!("year({c})"),
                    DatePartKind::Month => format!("month({c})"),
                    DatePartKind::Day => format!("day({c})"),
                    DatePartKind::Hour => format!("hour({c})"),
                    DatePartKind::Minute => format!("minute({c})"),
                    DatePartKind::Date => format!("strftime({c}, \"%Y-%m-%d\")"),
                    DatePartKind::Weekday => {
                        let names = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
                        let mut args: Vec<String> = Vec::new();
                        for (i, n) in names.iter().enumerate() {
                            args.push(format!("weekday({c}) == {}", i + 1));
                            args.push(quote(n));
                        }
                        args.push("null".into());
                        format!("case({})", args.join(", "))
                    }
                }
            }
            Recipe::Decode { col, how } => {
                let c = r(col);
                match how {
                    Decode::Base64 => format!("base64_decode({c})"),
                    Decode::Base64Utf16 => format!("base64_decode({c}, \"utf16\")"),
                    Decode::Hex => format!("hex_decode({c})"),
                    Decode::UrlHost => format!("url_host({c})"),
                    Decode::FileName => format!("path_file({c})"),
                    Decode::Folder => format!("path_dir({c})"),
                    Decode::Extension => format!("path_ext({c})"),
                }
            }
            Recipe::Maths {
                col,
                op,
                other,
                round,
            } => {
                let a = format!("num({})", r(col));
                let b = match other {
                    Operand::Number(n) => num_lit(*n),
                    Operand::Column(c) => format!("num({})", r(c)),
                };
                let sym = match op {
                    MathOp::Add => "+",
                    MathOp::Sub => "-",
                    MathOp::Mul => "*",
                    MathOp::Div => "/",
                };
                let e = format!("{a} {sym} {b}");
                match round {
                    Some(d) => format!("round({e}, {d})"),
                    None => e,
                }
            }
            Recipe::Measure { col, what } => match what {
                Measure::Length => format!("len({})", r(col)),
                Measure::Entropy => format!("round(entropy({}), 2)", r(col)),
            },
            Recipe::ToDate { col, unit } => format!("date({}, {})", r(col), quote(unit)),
            Recipe::Json { col, path } => format!("json({}, {})", r(col), quote(path)),
            Recipe::FirstNonEmpty { cols } => {
                let parts: Vec<String> = cols
                    .iter()
                    .map(|c| {
                        let rc = r(c);
                        format!("if(len(trim({rc})) > 0, {rc}, null)")
                    })
                    .collect();
                format!("coalesce({})", parts.join(", "))
            }
        }
    }

    /// A suggested name for the new column.
    pub fn default_name(&self) -> String {
        match self {
            Recipe::Split { col, piece, .. } => format!(
                "{}_{}",
                slug(col),
                match piece {
                    Piece::First => "first".to_string(),
                    Piece::Last => "last".to_string(),
                    Piece::Nth(n) => format!("part{n}"),
                }
            ),
            Recipe::Combine { cols, .. } => {
                cols.iter().map(|c| slug(c)).collect::<Vec<_>>().join("_")
            }
            Recipe::Clean { col, .. } => format!("{}_clean", slug(col)),
            Recipe::Classify { .. } => "category".into(),
            Recipe::DatePart { col, part } => {
                format!("{}_{}", slug(col), format!("{part:?}").to_lowercase())
            }
            Recipe::Decode { col, how } => format!(
                "{}_{}",
                slug(col),
                match how {
                    Decode::Base64 => "decoded",
                    Decode::Base64Utf16 => "decoded",
                    Decode::Hex => "hex_decoded",
                    Decode::UrlHost => "host",
                    Decode::FileName => "file",
                    Decode::Folder => "folder",
                    Decode::Extension => "ext",
                }
            ),
            Recipe::Maths { col, op, .. } => {
                format!("{}_{}", slug(col), format!("{op:?}").to_lowercase())
            }
            Recipe::Measure { col, what } => {
                format!("{}_{}", slug(col), format!("{what:?}").to_lowercase())
            }
            Recipe::ToDate { col, .. } => format!("{}_time", slug(col)),
            Recipe::Json { col, path } => {
                // named after the field: its last name, with what holds it when short
                let last = path
                    .rsplit('.')
                    .next()
                    .unwrap_or(path)
                    .split('[')
                    .next()
                    .unwrap_or_default();
                if last.is_empty() {
                    format!("{}_item", slug(col))
                } else {
                    format!("{}_{}", slug(col), slug(last))
                }
            }
            Recipe::FirstNonEmpty { cols } => format!(
                "first_of_{}",
                cols.first().map(|c| slug(c)).unwrap_or_default()
            ),
        }
    }
}

/// The formula's result for sample rows (the live preview), or why it cannot run.
pub fn preview(
    f: &str,
    cols: &[(String, ColType)],
    rows: &[Vec<Option<String>>],
) -> Result<Vec<String>, String> {
    let formula = logseek_core::formula::compile(f, cols).map_err(|e| e.msg)?;
    Ok(rows
        .iter()
        .map(|row| {
            let get = |i: usize| -> FValue {
                let Some(Some(v)) = row.get(i) else {
                    return FValue::Null;
                };
                match cols.get(i).map(|c| c.1) {
                    Some(ColType::Int) | Some(ColType::Float) => v
                        .trim()
                        .parse()
                        .map(FValue::Num)
                        .unwrap_or_else(|_| FValue::Text(v.clone())),
                    Some(ColType::Time) => logseek_core::time::parse_time(v)
                        .map(FValue::Time)
                        .unwrap_or_else(|| FValue::Text(v.clone())),
                    _ => FValue::Text(v.clone()),
                }
            };
            formula.eval(&get).text().unwrap_or_default()
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use logseek_core::types::ColType;

    fn cols() -> Vec<(String, ColType)> {
        [
            ("Image", ColType::Text),
            ("host", ColType::Text),
            ("user name", ColType::Text),
            ("EventID", ColType::Text),
            ("_time", ColType::Time),
            ("bytes", ColType::Int),
            ("blob", ColType::Text),
            ("epoch", ColType::Text),
        ]
        .iter()
        .map(|(n, t)| (n.to_string(), *t))
        .collect()
    }

    fn row(v: &[&str]) -> Vec<Option<String>> {
        v.iter()
            .map(|s| (!s.is_empty()).then(|| s.to_string()))
            .collect()
    }

    fn one(r: &Recipe, values: &[&str]) -> String {
        preview(&r.formula(), &cols(), &[row(values)]).unwrap_or_else(|e| panic!("{r:?}: {e}"))[0]
            .clone()
    }

    const ROW: [&str; 8] = [
        r"C:\a\b\cmd.exe",
        "srv-b0201",
        "  bob  ",
        "4624",
        "2016-08-24 18:27:37",
        "1536",
        "aABpAA==",
        "1472063257",
    ];

    #[test]
    fn every_recipe_compiles_and_evaluates() {
        let s = |x: &str| x.to_string();
        let cases: Vec<(Recipe, &str)> = vec![
            (
                Recipe::Split {
                    col: s("Image"),
                    sep: s("\\"),
                    piece: Piece::Last,
                },
                "cmd.exe",
            ),
            (
                Recipe::Split {
                    col: s("Image"),
                    sep: s("\\"),
                    piece: Piece::Nth(2),
                },
                "a",
            ),
            (
                Recipe::Combine {
                    cols: vec![s("host"), s("EventID")],
                    sep: s(" / "),
                },
                "srv-b0201 / 4624",
            ),
            (
                Recipe::Clean {
                    col: s("user name"),
                    trim: true,
                    case: Case::Upper,
                    replace: Some((s("B"), s("R"))),
                },
                "ROR",
            ),
            (
                Recipe::DatePart {
                    col: s("_time"),
                    part: DatePartKind::Hour,
                },
                "18",
            ),
            (
                Recipe::DatePart {
                    col: s("_time"),
                    part: DatePartKind::Weekday,
                },
                "Wed",
            ),
            (
                Recipe::DatePart {
                    col: s("_time"),
                    part: DatePartKind::Date,
                },
                "2016-08-24",
            ),
            (
                Recipe::Decode {
                    col: s("blob"),
                    how: Decode::Base64Utf16,
                },
                "hi",
            ),
            (
                Recipe::Decode {
                    col: s("Image"),
                    how: Decode::FileName,
                },
                "cmd.exe",
            ),
            (
                Recipe::Decode {
                    col: s("Image"),
                    how: Decode::Extension,
                },
                "exe",
            ),
            (
                Recipe::Maths {
                    col: s("bytes"),
                    op: MathOp::Div,
                    other: Operand::Number(1024.0),
                    round: Some(1),
                },
                "1.5",
            ),
            (
                Recipe::Measure {
                    col: s("host"),
                    what: Measure::Length,
                },
                "9",
            ),
            (
                Recipe::ToDate {
                    col: s("epoch"),
                    unit: s("s"),
                },
                "2016-08-24 18:27:37",
            ),
            (
                Recipe::FirstNonEmpty {
                    cols: vec![s("blob"), s("host")],
                },
                "aABpAA==",
            ),
        ];
        for (r, want) in &cases {
            assert_eq!(one(r, &ROW), *want, "{r:?} → {}", r.formula());
            assert!(!r.default_name().is_empty());
        }
        // first non-empty skips empty cells
        assert_eq!(
            one(
                &Recipe::FirstNonEmpty {
                    cols: vec![s("blob"), s("host")]
                },
                &["", "h", "", "", "", "", "", ""]
            ),
            "h"
        );
    }

    #[test]
    fn classify_first_rule_wins_and_otherwise_applies() {
        use crate::colors::RuleOp;
        let s = |x: &str| x.to_string();
        let r = Recipe::Classify {
            rules: vec![
                (s("EventID"), RuleOp::Is, s("4624"), s("Logon")),
                (s("EventID"), RuleOp::Is, s("4634"), s("Logoff")),
                (s("EventID"), RuleOp::Contains, s("4625"), s("Failure")),
                (s("EventID"), RuleOp::Contains, s("46"), s("Other 46")),
            ],
            otherwise: s("Other"),
        };
        let ev = |id: &str| one(&r, &["", "", "", id, "", "", "", ""]);
        assert_eq!(
            ev("4624"),
            "Logon",
            "Is beats the later overlapping Contains"
        );
        assert_eq!(ev("4634"), "Logoff");
        assert_eq!(ev("4625"), "Failure");
        assert_eq!(ev("4688"), "Other 46");
        assert_eq!(ev("1"), "Other");
    }

    #[test]
    fn entropy_spots_encoded_text() {
        let r = Recipe::Measure {
            col: "blob".into(),
            what: Measure::Entropy,
        };
        let random = one(
            &r,
            &[
                "",
                "",
                "",
                "",
                "",
                "",
                "U29tZVJhbmRvbUJhc2U2NFN0cmluZ1dpdGhWYXJpZXR5MTIzNDU2Nzg5",
                "",
            ],
        );
        let plain = one(&r, &["", "", "", "", "", "", "aaaaaaaaaaaaaaaaaaaa", ""]);
        assert!(random.parse::<f64>().unwrap() > 4.0, "{random}");
        assert!(plain.parse::<f64>().unwrap() < 1.0, "{plain}");
    }

    #[test]
    fn columns_made_by_formulas_are_written_in_place() {
        // "Image" is an extracted column; the engine only knows the file's own columns
        let stored = vec![("_raw".to_string(), ColType::Text)];
        let made = "regex(_raw, \"<Image>([^<]*)<\", 1)";
        let r = Recipe::Split {
            col: "Image".into(),
            sep: "\\".into(),
            piece: Piece::Last,
        };
        let f = r.formula_with(&|c: &str| {
            if c == "Image" {
                format!("({made})")
            } else {
                logseek_core::formula::column_ref(c)
            }
        });
        assert!(!f.contains("Image,"), "{f}");
        let raw = vec![Some(
            r"<Event><Image>C:\Windows\cmd.exe</Image></Event>".to_string(),
        )];
        assert_eq!(preview(&f, &stored, &[raw]).unwrap()[0], "cmd.exe");
    }

    #[test]
    fn names_with_spaces_and_bad_formulas() {
        // a column name with a space is quoted in the formula
        let r = Recipe::Measure {
            col: "user name".into(),
            what: Measure::Length,
        };
        assert_eq!(one(&r, &ROW), "7");
        assert!(
            preview("upper(", &cols(), &[row(&ROW)]).is_err(),
            "a broken formula is reported"
        );
        let names: std::collections::BTreeSet<String> = [
            Recipe::Split {
                col: "Image".into(),
                sep: "\\".into(),
                piece: Piece::Last,
            },
            Recipe::Decode {
                col: "Image".into(),
                how: Decode::FileName,
            },
            Recipe::Measure {
                col: "Image".into(),
                what: Measure::Length,
            },
        ]
        .iter()
        .map(Recipe::default_name)
        .collect();
        assert_eq!(names.len(), 3, "different recipes suggest different names");
    }
}
