//! The condition builder: choose a test and a value, typed to the column.
//! Every choice is written as filter-row text (the same language the filter
//! row accepts), so the builder, the filter row and the query text agree.

use logseek_core::types::ColType;

/// What kind of value a column holds, which decides the operators offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColKind {
    Text,
    Number,
    Time,
}

impl ColKind {
    pub fn of(t: Option<&ColType>, is_time_col: bool) -> ColKind {
        match t {
            _ if is_time_col => ColKind::Time,
            Some(ColType::Time) => ColKind::Time,
            Some(ColType::Int) | Some(ColType::Float) => ColKind::Number,
            _ => ColKind::Text,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Minutes,
    Hours,
    Days,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Contains,
    NotContains,
    StartsWith,
    EndsWith,
    Is,
    IsNot,
    OneOf,
    NotOneOf,
    Pattern,
    Regex,
    Empty,
    NotEmpty,
    Gt,
    Ge,
    Lt,
    Le,
    Between,
    After,
    Before,
    OnDay,
    LastN(Unit),
}

/// The operators that make sense for a column, most used first.
pub fn ops_for(kind: ColKind) -> &'static [Op] {
    use Op::*;
    match kind {
        ColKind::Text => &[
            Contains,
            NotContains,
            StartsWith,
            EndsWith,
            Is,
            IsNot,
            OneOf,
            NotOneOf,
            Pattern,
            Regex,
            Empty,
            NotEmpty,
        ],
        ColKind::Number => &[
            Is, IsNot, Gt, Ge, Lt, Le, Between, OneOf, NotOneOf, Empty, NotEmpty,
        ],
        ColKind::Time => &[
            After,
            Before,
            OnDay,
            Between,
            LastN(Unit::Minutes),
            LastN(Unit::Hours),
            LastN(Unit::Days),
            Empty,
            NotEmpty,
        ],
    }
}

/// The operator in words, as the dropdown shows it.
pub fn op_label(op: Op) -> &'static str {
    match op {
        Op::Contains => "contains",
        Op::NotContains => "does not contain",
        Op::StartsWith => "starts with",
        Op::EndsWith => "ends with",
        Op::Is => "is exactly",
        Op::IsNot => "is not",
        Op::OneOf => "is one of",
        Op::NotOneOf => "is none of",
        Op::Pattern => "matches the wildcard pattern",
        Op::Regex => "matches the regular expression",
        Op::Empty => "has no value",
        Op::NotEmpty => "has a value",
        Op::Gt => "is greater than",
        Op::Ge => "is at least",
        Op::Lt => "is less than",
        Op::Le => "is at most",
        Op::Between => "is between",
        Op::After => "is on or after",
        Op::Before => "is before",
        Op::OnDay => "is on the day",
        Op::LastN(Unit::Minutes) => "is in the last N minutes",
        Op::LastN(Unit::Hours) => "is in the last N hours",
        Op::LastN(Unit::Days) => "is in the last N days",
    }
}

/// How many values the operator needs (0, 1 or 2).
pub fn values_needed(op: Op) -> usize {
    match op {
        Op::Empty | Op::NotEmpty => 0,
        Op::Between => 2,
        _ => 1,
    }
}

/// A hint for the value box.
pub fn value_hint(op: Op) -> &'static str {
    match op {
        Op::OneOf | Op::NotOneOf => "values, separated by commas",
        Op::Pattern => "* any text, ? one character",
        Op::Regex => "a regular expression",
        Op::After | Op::Before | Op::OnDay | Op::Between => "2016-08-24 or 2016-08-24 13:00",
        Op::LastN(_) => "how many",
        Op::Gt | Op::Ge | Op::Lt | Op::Le => "a number",
        _ => "text",
    }
}

/// "does not" style operators keep rows that have no value at all.
pub fn keeps_empty_rows(op: Op) -> bool {
    // "is not" is a comparison, and comparisons skip rows with no value
    matches!(op, Op::NotContains | Op::NotOneOf)
}

/// Text that the filter row would read as syntax rather than as words.
fn reads_as_syntax(a: &str) -> bool {
    let up = a.to_ascii_uppercase();
    a.contains(['%', '*', '?', '~', '{', '}', '"', '/'])
        || a.starts_with(['!', '^', '$', '=', '<', '>', '('])
        || ["IN", "NOT", "EMPTY", "NULL"].iter().any(|k| {
            up == *k || up.starts_with(&format!("{k} ")) || up.starts_with(&format!("{k}("))
        })
}

fn regex_literal(a: &str) -> String {
    let mut out = String::new();
    for ch in a.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn list_item(v: &str) -> String {
    let v = v.trim();
    if v.parse::<f64>().is_ok()
        || v.chars()
            .all(|c| c.is_alphanumeric() || "_.-:@".contains(c))
    {
        v.to_string()
    } else {
        logseek_core::formula::quote(v)
    }
}

fn time_value(a: &str) -> Result<String, String> {
    let a = a.trim();
    logseek_core::time::parse_time(a)
        .map(|_| format!("\"{a}\""))
        .ok_or_else(|| format!("\"{a}\" is not a date or time: try 2016-08-24 or 2016-08-24 13:00"))
}

/// One condition as filter-row text. `newest_us` is the newest time in the
/// source, used by "is in the last N hours".
pub fn to_filter_text(op: Op, a: &str, b: &str, newest_us: Option<i64>) -> Result<String, String> {
    let (a, b) = (a.trim(), b.trim());
    if values_needed(op) >= 1 && a.is_empty() {
        return Err(format!("\"{}\" needs a value", op_label(op)));
    }
    if values_needed(op) == 2 && b.is_empty() {
        return Err("\"is between\" needs two values".into());
    }
    let number = |v: &str| -> Result<String, String> {
        v.parse::<f64>()
            .map(|_| v.to_string())
            .map_err(|_| format!("\"{v}\" is not a number"))
    };
    Ok(match op {
        Op::Contains if reads_as_syntax(a) => format!("/{}/i", regex_literal(a)),
        Op::Contains => a.to_string(),
        Op::NotContains => format!("!{a}"),
        Op::StartsWith => format!("^={a}"),
        Op::EndsWith => format!("$={a}"),
        Op::Is => format!("={}", logseek_core::formula::quote(a)),
        Op::IsNot => format!("!={}", logseek_core::formula::quote(a)),
        Op::OneOf | Op::NotOneOf => {
            let items: Vec<String> = a
                .split(',')
                .filter(|v| !v.trim().is_empty())
                .map(list_item)
                .collect();
            if items.is_empty() {
                return Err("list the values, separated by commas".into());
            }
            let not = if op == Op::NotOneOf { "NOT " } else { "" };
            format!("{not}IN ({})", items.join(", "))
        }
        Op::Pattern => a.to_string(),
        Op::Regex => format!("/{a}/i"),
        Op::Empty => "EMPTY".into(),
        Op::NotEmpty => "NOT EMPTY".into(),
        Op::Gt => format!(">{}", number(a)?),
        Op::Ge => format!(">={}", number(a)?),
        Op::Lt => format!("<{}", number(a)?),
        Op::Le => format!("<={}", number(a)?),
        Op::Between if a.parse::<f64>().is_ok() && b.parse::<f64>().is_ok() => format!("{a}~{b}"),
        Op::Between => format!("{{>={}}} AND {{<={}}}", time_value(a)?, time_value(b)?),
        Op::After => format!(">={}", time_value(a)?),
        Op::Before => format!("<{}", time_value(a)?),
        Op::OnDay => {
            let us = logseek_core::time::parse_time(a)
                .ok_or_else(|| format!("\"{a}\" is not a day: try 2016-08-24"))?;
            let day = |t: i64| {
                let f = logseek_core::types::fmt_time(t);
                f.get(..10).unwrap_or(&f).to_string()
            };
            let start = us.div_euclid(86_400_000_000) * 86_400_000_000;
            format!(
                "{{>=\"{}\"}} AND {{<\"{}\"}}",
                day(start),
                day(start + 86_400_000_000)
            )
        }
        Op::LastN(unit) => {
            let n: i64 = a
                .parse()
                .map_err(|_| format!("\"{a}\" is not a whole number"))?;
            let newest = newest_us.ok_or("this column has no times to count back from")?;
            let step: i64 = match unit {
                Unit::Minutes => 60_000_000,
                Unit::Hours => 3_600_000_000,
                Unit::Days => 86_400_000_000,
            };
            format!(">=\"{}\"", logseek_core::types::fmt_time(newest - n * step))
        }
    })
}

/// The condition in plain words: "host starts with we".
pub fn reading(col: &str, op: Op, a: &str, b: &str) -> String {
    let (a, b) = (a.trim(), b.trim());
    let mut out = match op {
        Op::LastN(unit) => {
            let u = match unit {
                Unit::Minutes => "minutes",
                Unit::Hours => "hours",
                Unit::Days => "days",
            };
            format!("{col} is in the last {a} {u}")
        }
        Op::Between => format!("{col} is between {a} and {b}"),
        _ if values_needed(op) == 0 => format!("{col} {}", op_label(op)),
        _ => format!("{col} {} {a}", op_label(op)),
    };
    if keeps_empty_rows(op) {
        out.push_str("  (rows with no value are kept too)");
    }
    out
}

/// Folds one more condition onto what is built so far, left to right.
pub fn combine(x: &str, and: bool, y: &str) -> String {
    let joiner = if and { "AND" } else { "OR" };
    if x.starts_with('{') {
        format!("{x} {joiner} {{{y}}}")
    } else {
        format!("{{{x}}} {joiner} {{{y}}}")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn one_of_keeps_backslashes() {
        let t = to_filter_text(
            Op::OneOf,
            r"C:\Windows\System32\cmd.exe, C:\Windows\SysWOW64\cmd.exe",
            "",
            None,
        )
        .unwrap();
        let clause = crate::filters::text_clause("Image", &t).unwrap().unwrap();
        assert!(
            clause.contains(&logseek_core::formula::quote(
                r"C:\Windows\System32\cmd.exe"
            )),
            "{clause}"
        );
    }

    #[test]
    fn is_exactly_keeps_quotes_and_backslashes() {
        let v = r#"say "hi" C:\x"#;
        let t = to_filter_text(Op::Is, v, "", None).unwrap();
        let clause = crate::filters::text_clause("Image", &t).unwrap().unwrap();
        assert!(
            clause.contains(&format!("Image={}", logseek_core::formula::quote(v))),
            "{clause}"
        );
        // "is not" compares, and comparisons skip rows with no value
        assert!(!keeps_empty_rows(Op::IsNot));
    }

    use super::*;

    fn accepted(t: &str) {
        assert!(
            matches!(crate::filters::text_clause("host", t), Ok(Some(_))),
            "the filter row rejects {t:?}: {:?}",
            crate::filters::text_clause("host", t)
        );
    }

    #[test]
    fn conditions_write_the_filter_language() {
        let cases = [
            (Op::Contains, "8105", "", "8105"),
            (Op::NotContains, "8105", "", "!8105"),
            (Op::StartsWith, "we", "", "^=we"),
            (Op::EndsWith, "srv", "", "$=srv"),
            (Op::Is, "host-a01", "", "=\"host-a01\""),
            (Op::IsNot, "host-a01", "", "!=\"host-a01\""),
            (Op::Between, "5", "10", "5~10"),
            (Op::OneOf, "a, b c", "", "IN (a, \"b c\")"),
            (Op::NotOneOf, "a, b c", "", "NOT IN (a, \"b c\")"),
            (Op::Empty, "", "", "EMPTY"),
            (Op::NotEmpty, "", "", "NOT EMPTY"),
            (Op::Gt, "100", "", ">100"),
            (Op::Ge, "100", "", ">=100"),
            (Op::Lt, "100", "", "<100"),
            (Op::Le, "100", "", "<=100"),
            (Op::Pattern, "cmd*", "", "cmd*"),
            (Op::Regex, "ab+c", "", "/ab+c/i"),
        ];
        for (op, a, b, want) in cases {
            let got = to_filter_text(op, a, b, None).unwrap();
            assert_eq!(got, want, "{op:?}");
            accepted(&got);
        }
        let c = combine("^=we", true, "!srv");
        assert_eq!(c, "{^=we} AND {!srv}");
        accepted(&c);
        let c3 = combine(&c, false, "EMPTY");
        assert_eq!(c3, "{^=we} AND {!srv} OR {EMPTY}");
        accepted(&c3);
    }

    #[test]
    fn contains_with_special_characters_stays_literal() {
        // text that would read as syntax is matched literally, any case
        for a in ["!important", "50%", "a*b", "{x}", "/tmp/x", ">3", "IN (x)"] {
            let got = to_filter_text(Op::Contains, a, "", None).unwrap();
            accepted(&got);
            assert!(got.starts_with('/') && got.ends_with("/i"), "{a:?} → {got}");
        }
    }

    #[test]
    fn time_conditions_write_ranges() {
        assert_eq!(
            to_filter_text(Op::After, "2016-08-24", "", None).unwrap(),
            ">=\"2016-08-24\""
        );
        assert_eq!(
            to_filter_text(Op::Before, "2016-08-24 12:00", "", None).unwrap(),
            "<\"2016-08-24 12:00\""
        );
        let day = to_filter_text(Op::OnDay, "2016-08-31", "", None).unwrap();
        assert_eq!(day, "{>=\"2016-08-31\"} AND {<\"2016-09-01\"}");
        accepted(&day);
        let between = to_filter_text(Op::Between, "2016-08-01", "2016-08-02", None).unwrap();
        assert_eq!(between, "{>=\"2016-08-01\"} AND {<=\"2016-08-02\"}");
        accepted(&between);
        // newest row 2016-08-24 18:00 UTC; last 6 hours → from 12:00
        let newest = logseek_core::time::parse_time("2016-08-24 18:00:00").unwrap();
        let last = to_filter_text(Op::LastN(Unit::Hours), "6", "", Some(newest)).unwrap();
        assert_eq!(last, ">=\"2016-08-24 12:00:00\"");
        accepted(&last);
        assert!(to_filter_text(Op::LastN(Unit::Days), "x", "", Some(newest)).is_err());
        assert!(to_filter_text(Op::OnDay, "not a day", "", None).is_err());
    }

    #[test]
    fn missing_values_are_reported_not_guessed() {
        assert!(to_filter_text(Op::Contains, "  ", "", None).is_err());
        assert!(to_filter_text(Op::Between, "5", "", None).is_err());
        assert!(to_filter_text(Op::Gt, "abc", "", None).is_err());
    }

    #[test]
    fn each_condition_reads_as_a_sentence() {
        assert_eq!(
            reading("host", Op::StartsWith, "we", ""),
            "host starts with we"
        );
        assert_eq!(
            reading("_time", Op::LastN(Unit::Hours), "6", ""),
            "_time is in the last 6 hours"
        );
        assert_eq!(
            reading("bytes", Op::Between, "5", "10"),
            "bytes is between 5 and 10"
        );
        assert_eq!(reading("user", Op::Empty, "", ""), "user has no value");
        assert_eq!(
            reading("host", Op::NotContains, "desk", ""),
            "host does not contain desk  (rows with no value are kept too)"
        );
    }

    #[test]
    fn operators_fit_the_column_kind() {
        assert!(ops_for(ColKind::Text).contains(&Op::Contains));
        assert!(!ops_for(ColKind::Text).contains(&Op::Gt));
        assert!(ops_for(ColKind::Number).contains(&Op::Between));
        assert!(ops_for(ColKind::Time).contains(&Op::OnDay));
        assert!(!ops_for(ColKind::Time).contains(&Op::Contains));
        for k in [ColKind::Text, ColKind::Number, ColKind::Time] {
            for op in ops_for(k) {
                assert!(!op_label(*op).is_empty());
            }
        }
        assert!(keeps_empty_rows(Op::NotContains));
        assert!(!keeps_empty_rows(Op::Contains));
    }
}
