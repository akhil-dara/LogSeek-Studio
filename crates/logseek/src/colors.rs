//! Colour rules: rows that match a rule get a soft tint and a marker in the
//! gutter (colour is never the only cue). First enabled match wins. Rules are
//! checked only on the rows on screen, so they cost the engine nothing.

use eframe::egui::Color32;
use serde::{Deserialize, Serialize};

/// How a rule tests a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleOp {
    Contains,
    /// Contains any of the comma-separated words.
    ContainsAny,
    Is,
    /// Is any of the comma-separated values.
    IsAny,
    StartsWith,
    Regex,
    Gt,
    Lt,
}

pub const OPS: [RuleOp; 8] = [
    RuleOp::Contains,
    RuleOp::ContainsAny,
    RuleOp::Is,
    RuleOp::IsAny,
    RuleOp::StartsWith,
    RuleOp::Regex,
    RuleOp::Gt,
    RuleOp::Lt,
];

pub fn op_label(op: RuleOp) -> &'static str {
    match op {
        RuleOp::Contains => "contains",
        RuleOp::ContainsAny => "contains any of",
        RuleOp::Is => "is",
        RuleOp::IsAny => "is any of",
        RuleOp::StartsWith => "starts with",
        RuleOp::Regex => "matches the regular expression",
        RuleOp::Gt => "is greater than",
        RuleOp::Lt => "is less than",
    }
}

/// One colour rule: scope (one column or any), condition, tint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColorRule {
    pub enabled: bool,
    /// What the colour means, shown on the gutter marker ("LOLBins").
    pub name: String,
    /// `None` = any column.
    pub column: Option<String>,
    pub op: RuleOp,
    pub value: String,
    /// Index into [`TINTS`].
    pub tint: usize,
    /// Colour just the matching cells (with a corner mark), not the whole row.
    #[serde(default)]
    pub cells: bool,
}

/// The eight tints, named so a rule never depends on seeing colour.
pub const TINTS: [&str; 8] = [
    "Red", "Orange", "Amber", "Green", "Teal", "Blue", "Purple", "Grey",
];

/// A tint's colour; rows use it faintly, the gutter marker at full strength.
pub fn tint_color(i: usize, dark: bool) -> Color32 {
    let (d, l) = match i % 8 {
        0 => (0xF2555A, 0xD13438),
        1 => (0xF7843F, 0xC75A12),
        2 => (0xF5C04A, 0xA87B0A),
        3 => (0x3DBE7E, 0x1E8C55),
        4 => (0x2DBFAE, 0x0E8577),
        5 => (0x6E8EF5, 0x3456C8),
        6 => (0xB07EE8, 0x7E44B8),
        _ => (0xA0A3AE, 0x6E717C),
    };
    let c = if dark { d } else { l };
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

fn words(v: &str) -> Vec<String> {
    v.split(',')
        .map(|w| w.trim().to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Case-insensitive substring test; `needle` is already lower case.
fn ci_contains(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    if needle.is_ascii() {
        let n = needle.as_bytes();
        hay.as_bytes()
            .windows(n.len())
            .any(|w| w.eq_ignore_ascii_case(n))
    } else {
        hay.to_lowercase().contains(needle)
    }
}

enum Matcher {
    Contains(Vec<String>),
    Is(Vec<String>),
    Starts(String),
    Re(regex::Regex),
    Gt(f64),
    Lt(f64),
    Never,
}

impl Matcher {
    fn of(r: &ColorRule) -> Matcher {
        let v = r.value.trim();
        match r.op {
            RuleOp::Contains => Matcher::Contains(vec![v.to_lowercase()]),
            RuleOp::ContainsAny => Matcher::Contains(words(v)),
            RuleOp::Is => Matcher::Is(vec![v.to_lowercase()]),
            RuleOp::IsAny => Matcher::Is(words(v)),
            RuleOp::StartsWith => Matcher::Starts(v.to_lowercase()),
            RuleOp::Regex => regex::Regex::new(&format!("(?i){v}"))
                .map(Matcher::Re)
                .unwrap_or(Matcher::Never),
            RuleOp::Gt => v.parse().map(Matcher::Gt).unwrap_or(Matcher::Never),
            RuleOp::Lt => v.parse().map(Matcher::Lt).unwrap_or(Matcher::Never),
        }
    }

    fn test(&self, cell: &str) -> bool {
        match self {
            Matcher::Contains(ws) => ws.iter().any(|w| ci_contains(cell, w)),
            Matcher::Is(ws) => ws.iter().any(|w| cell.trim().to_lowercase() == *w),
            Matcher::Starts(w) => !w.is_empty() && cell.to_lowercase().starts_with(w.as_str()),
            Matcher::Re(re) => re.is_match(cell),
            Matcher::Gt(n) => cell.trim().parse::<f64>().is_ok_and(|x| x > *n),
            Matcher::Lt(n) => cell.trim().parse::<f64>().is_ok_and(|x| x < *n),
            Matcher::Never => false,
        }
    }
}

struct CRule {
    tint: usize,
    name: String,
    column: Option<String>,
    m: Matcher,
    cells: bool,
}

/// Rules ready to test rows: regexes compiled once, words lower-cased once.
#[derive(Default)]
pub struct Compiled {
    rules: Vec<CRule>,
    /// The rules this was built from, to know when to rebuild.
    pub source: Vec<ColorRule>,
}

impl Compiled {
    pub fn new(rules: &[ColorRule]) -> Compiled {
        Compiled {
            rules: rules
                .iter()
                .filter(|r| r.enabled && !r.value.trim().is_empty())
                .map(|r| CRule {
                    tint: r.tint,
                    name: if r.name.is_empty() {
                        describe(r)
                    } else {
                        r.name.clone()
                    },
                    column: r.column.clone(),
                    m: Matcher::of(r),
                    cells: r.cells,
                })
                .collect(),
            source: rules.to_vec(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Some rule colours single cells.
    pub fn has_cell_rules(&self) -> bool {
        self.rules.iter().any(|r| r.cells)
    }

    /// The first cell rule this cell of column `col` matches: its tint.
    pub fn cell_match(&self, col: &str, value: &str) -> Option<usize> {
        self.rules
            .iter()
            .filter(|r| r.cells && r.column.as_deref().is_none_or(|c| c == col))
            .find(|r| r.m.test(value))
            .map(|r| r.tint)
    }

    /// The first whole-row rule this row matches: its tint and what it means.
    pub fn row_match(&self, cols: &[String], row: &[Option<String>]) -> Option<(usize, &str)> {
        for r in self.rules.iter().filter(|r| !r.cells) {
            let hit = match &r.column {
                Some(c) => cols
                    .iter()
                    .position(|n| n == c)
                    .and_then(|i| row.get(i))
                    .and_then(|v| v.as_deref())
                    .is_some_and(|v| r.m.test(v)),
                None => row.iter().flatten().any(|v| r.m.test(v)),
            };
            if hit {
                return Some((r.tint, &r.name));
            }
        }
        None
    }
}

/// The tint of the first enabled rule the row matches.
#[cfg(test)]
pub fn row_tint(rules: &[ColorRule], cols: &[String], row: &[Option<String>]) -> Option<usize> {
    Compiled::new(rules).row_match(cols, row).map(|(t, _)| t)
}

/// The rule "column is value", in the first tint no rule uses yet (or the
/// next in turn when all are taken), from a cell the user pointed at.
pub fn rule_from_cell(rules: &[ColorRule], col: &str, value: &str) -> ColorRule {
    let tint = (0..TINTS.len())
        .find(|t| !rules.iter().any(|r| r.tint == *t))
        .unwrap_or(rules.len() % TINTS.len());
    ColorRule {
        enabled: true,
        name: String::new(),
        column: Some(col.to_string()),
        op: RuleOp::Is,
        value: value.to_string(),
        tint,
        cells: false,
    }
}

/// The query that counts the rows of the current view a rule matches: the
/// view's search and filters, and the rule as one more filter.
pub fn count_query(
    r: &ColorRule,
    search: &str,
    filters: &crate::filters::Filters,
) -> Option<String> {
    if !r.enabled || r.value.trim().is_empty() {
        return None;
    }
    match show_only(r)? {
        ShowOnly::Column(c, t) => {
            let mut f = filters.clone();
            let e = f.entry(c).or_default();
            if e.enabled && !e.text.trim().is_empty() {
                e.text = crate::conditions::combine(&e.text, true, &t);
            } else {
                e.text = t;
                e.enabled = true;
            }
            Some(crate::filters::compose(search, &f, None))
        }
        ShowOnly::Global(q) => {
            let s = if search.trim().is_empty() {
                q
            } else {
                format!("{} ({q})", search.trim())
            };
            Some(crate::filters::compose(&s, filters, None))
        }
    }
}

/// A rule in plain words: "host contains srv".
pub fn describe(r: &ColorRule) -> String {
    format!(
        "{} {} {}",
        r.column.as_deref().unwrap_or("any column"),
        op_label(r.op),
        r.value.trim()
    )
}

/// What "Show only these rows" does with a rule.
#[derive(Debug, PartialEq)]
pub enum ShowOnly {
    /// Put this text in the column's filter row.
    Column(String, String),
    /// Search for this in every field.
    Global(String),
}

pub fn show_only(r: &ColorRule) -> Option<ShowOnly> {
    use crate::conditions::{combine, to_filter_text, Op};
    let v = r.value.trim();
    let items: Vec<&str> = v
        .split(',')
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .collect();
    match &r.column {
        Some(c) => {
            let text = match r.op {
                RuleOp::Contains => to_filter_text(Op::Contains, v, "", None).ok()?,
                RuleOp::ContainsAny => {
                    let mut acc: Option<String> = None;
                    for w in &items {
                        let t = to_filter_text(Op::Contains, w, "", None).ok()?;
                        acc = Some(match acc {
                            None => t,
                            Some(x) => combine(&x, false, &t),
                        });
                    }
                    acc?
                }
                RuleOp::Is => to_filter_text(Op::Is, v, "", None).ok()?,
                RuleOp::IsAny => to_filter_text(Op::OneOf, v, "", None).ok()?,
                RuleOp::StartsWith => to_filter_text(Op::StartsWith, v, "", None).ok()?,
                RuleOp::Regex => format!("/{v}/i"),
                RuleOp::Gt => to_filter_text(Op::Gt, v, "", None).ok()?,
                RuleOp::Lt => to_filter_text(Op::Lt, v, "", None).ok()?,
            };
            Some(ShowOnly::Column(c.clone(), text))
        }
        None => {
            let q = logseek_core::formula::quote;
            match r.op {
                RuleOp::Contains if !v.is_empty() => Some(ShowOnly::Global(q(v))),
                RuleOp::ContainsAny if !items.is_empty() => Some(ShowOnly::Global(
                    items.iter().map(|w| q(w)).collect::<Vec<_>>().join(" OR "),
                )),
                _ => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    fn rule5(col: Option<&str>, op: RuleOp, v: &str, tint: usize, cells: bool) -> ColorRule {
        ColorRule {
            enabled: true,
            name: String::new(),
            column: col.map(str::to_string),
            op,
            value: v.into(),
            tint,
            cells,
        }
    }

    #[test]
    fn a_cell_rule_colours_the_cell_and_leaves_the_row_to_row_rules() {
        let rules = [
            rule5(Some("status"), RuleOp::Gt, "499", 0, true),
            rule5(None, RuleOp::Contains, "admin", 4, false),
        ];
        let c = Compiled::new(&rules);
        assert!(c.has_cell_rules());
        let cols = vec!["user".to_string(), "status".to_string()];
        // the cell rule tints only its column's cells
        assert_eq!(c.cell_match("status", "503"), Some(0));
        assert_eq!(c.cell_match("status", "200"), None);
        assert_eq!(c.cell_match("user", "503"), None);
        // the row is coloured only by the row rule
        let row = vec![Some("bob".to_string()), Some("503".to_string())];
        assert_eq!(c.row_match(&cols, &row), None);
        let row = vec![Some("admin".to_string()), Some("503".to_string())];
        assert_eq!(c.row_match(&cols, &row).map(|m| m.0), Some(4));
    }

    #[test]
    fn a_rule_from_a_cell_takes_a_tint_no_rule_uses() {
        let rules = [
            rule5(None, RuleOp::Contains, "a", 0, false),
            rule5(None, RuleOp::Contains, "b", 1, false),
        ];
        let r = rule_from_cell(&rules, "host", "web-01");
        assert_eq!(
            (r.column.as_deref(), r.op, r.value.as_str(), r.tint),
            (Some("host"), RuleOp::Is, "web-01", 2)
        );
        assert!(!r.cells && r.enabled);
    }

    #[test]
    fn a_rule_is_counted_within_the_view_it_colours() {
        use crate::filters::{ColFilter, Filters};
        let mut f = Filters::new();
        f.insert(
            "level".into(),
            ColFilter {
                text: "warn".into(),
                ..Default::default()
            },
        );
        // a column rule joins that column's own filter, the search stays
        let r = rule5(Some("level"), RuleOp::Is, "error", 0, false);
        let q = count_query(&r, "chrome", &f).unwrap();
        assert!(logseek_core::query::parse(&q).is_ok(), "{q}");
        assert!(
            q.contains("chrome") && q.contains("warn") && q.contains("error"),
            "{q}"
        );
        // an any-column rule joins the search
        let r = rule5(None, RuleOp::ContainsAny, "mimikatz, procdump", 1, false);
        let q = count_query(&r, "", &f).unwrap();
        assert!(logseek_core::query::parse(&q).is_ok(), "{q}");
        // nothing to count for an empty or switched-off rule
        assert!(count_query(&rule5(None, RuleOp::Contains, " ", 0, false), "", &f).is_none());
    }

    /// Readability: the text on a row coloured by a rule (the tint is drawn
    /// faintly over the row background) still meets WCAG AA in both themes —
    /// values, secondary text and the "+N chars" marker.
    #[test]
    fn text_on_tinted_rows_stays_readable() {
        use crate::theme::{contrast, DARK, LIGHT};
        fn over(c: Color32, a: f32, bg: Color32) -> Color32 {
            let m = |x: u8, y: u8| (x as f32 * a + y as f32 * (1.0 - a)).round() as u8;
            Color32::from_rgb(m(c.r(), bg.r()), m(c.g(), bg.g()), m(c.b(), bg.b()))
        }
        let mut bad = Vec::new();
        for p in [&DARK, &LIGHT] {
            let a = if p.dark { 0.20 } else { 0.15 };
            for (i, name) in TINTS.iter().enumerate() {
                let row = over(tint_color(i, p.dark), a, p.bg);
                for (t, tn) in [
                    (p.text, "text"),
                    (p.text2, "text2"),
                    (p.accent_ink, "accent_ink"),
                ] {
                    let c = contrast(t, row);
                    if c < 4.5 {
                        bad.push(format!(
                            "{} {tn} on {name} row: {c:.2}",
                            if p.dark { "dark" } else { "light" }
                        ));
                    }
                }
                // the gutter marker at full strength is a graphic: 3:1
                let c = contrast(tint_color(i, p.dark), p.bg);
                if c < 3.0 {
                    bad.push(format!(
                        "{} {name} marker: {c:.2}",
                        if p.dark { "dark" } else { "light" }
                    ));
                }
            }
            for t in 0..9 {
                let c = contrast(crate::tags::color(t, p.dark), p.bg);
                if c < 3.0 {
                    bad.push(format!(
                        "{} tag {t}: {c:.2}",
                        if p.dark { "dark" } else { "light" }
                    ));
                }
            }
        }
        assert!(
            bad.is_empty(),
            "hard to read:
{}",
            bad.join(
                "
"
            )
        );
    }

    use super::*;

    fn rule(column: Option<&str>, op: RuleOp, value: &str, tint: usize) -> ColorRule {
        ColorRule {
            enabled: true,
            name: String::new(),
            column: column.map(str::to_string),
            op,
            value: value.into(),
            tint,
            cells: false,
        }
    }

    fn row(v: &[&str]) -> Vec<Option<String>> {
        v.iter()
            .map(|s| (!s.is_empty()).then(|| s.to_string()))
            .collect()
    }

    #[test]
    fn first_matching_rule_wins_and_scopes_apply() {
        let cols: Vec<String> = ["host", "user"].iter().map(|s| s.to_string()).collect();
        let mut off = rule(None, RuleOp::Contains, "desk", 3);
        off.enabled = false;
        let rules = vec![
            rule(Some("host"), RuleOp::Contains, "srv", 1),
            rule(None, RuleOp::Contains, "we", 2),
            off,
            rule(Some("EventID"), RuleOp::Is, "4625", 4),
        ];
        assert_eq!(
            row_tint(&rules, &cols, &row(&["srv-b02", "bob"])),
            Some(1)
        );
        assert_eq!(
            row_tint(&rules, &cols, &row(&["web-desk01", "bob"])),
            Some(2)
        );
        assert_eq!(
            row_tint(&rules, &cols, &row(&["x", "Owen"])),
            Some(2),
            "any column, any case"
        );
        assert_eq!(
            row_tint(&rules, &cols, &row(&["desk", "x"])),
            None,
            "disabled rule, missing column"
        );
    }

    #[test]
    fn numbers_compare_as_numbers() {
        let cols = vec!["bytes".to_string()];
        let rules = vec![rule(Some("bytes"), RuleOp::Gt, "1000", 5)];
        assert_eq!(row_tint(&rules, &cols, &row(&["2048"])), Some(5));
        assert_eq!(row_tint(&rules, &cols, &row(&["999"])), None);
        assert_eq!(row_tint(&rules, &cols, &row(&["abc"])), None);
        let bad = vec![rule(None, RuleOp::Regex, "(unclosed", 1)];
        assert_eq!(
            row_tint(&bad, &cols, &row(&["(unclosed"])),
            None,
            "a bad regex matches nothing"
        );
    }

    #[test]
    fn show_only_writes_valid_filters() {
        let cases = [
            rule(Some("host"), RuleOp::Contains, "srv", 0),
            rule(Some("host"), RuleOp::ContainsAny, "srv, desk", 0),
            rule(Some("host"), RuleOp::Is, "srv-b02", 0),
            rule(Some("EventID"), RuleOp::IsAny, "4625, 4771", 0),
            rule(Some("host"), RuleOp::StartsWith, "we", 0),
            rule(Some("host"), RuleOp::Regex, "we\\d+", 0),
            rule(Some("bytes"), RuleOp::Gt, "1000", 0),
            rule(Some("bytes"), RuleOp::Lt, "10", 0),
        ];
        for r in &cases {
            match show_only(r) {
                Some(ShowOnly::Column(c, t)) => {
                    assert!(
                        matches!(crate::filters::text_clause(&c, &t), Ok(Some(_))),
                        "{r:?} → {t}"
                    )
                }
                other => panic!("{r:?} → {other:?}"),
            }
        }
        match show_only(&rule(None, RuleOp::ContainsAny, "certutil, mshta", 0)) {
            Some(ShowOnly::Global(q)) => {
                assert!(logseek_core::query::parse(&q).is_ok(), "{q}");
                assert!(q.contains("certutil") && q.contains("mshta"));
            }
            other => panic!("{other:?}"),
        }
        assert!(
            show_only(&rule(None, RuleOp::Gt, "5", 0)).is_none(),
            "no single column to compare"
        );
    }
}
