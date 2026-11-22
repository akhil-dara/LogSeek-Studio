//! The search builder: several conditions, each made by choosing, never by
//! writing a pattern. Every condition says where to look (every field or
//! one column), how (contains, starts with, is one of, is shaped like an
//! example the user gives, a number test, empty), and what. Nothing about
//! any kind of data is built in: a shape comes from the user's own example.
//! The whole thing
//! is written out as an ordinary search, so it can be read, edited, saved
//! and shared like any other.

use crate::matchopts::{Mode, Opts};
use logseek_core::formula::{column_ref, quote};
use logseek_core::query::shape::between_slashes;
pub use logseek_core::query::shape::{shape_pattern, shape_words, Shape};

/// How a condition matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum How {
    #[default]
    Contains,
    StartsWith,
    EndsWith,
    Exactly,
    Word,
    /// One of a list of values (the whole value).
    OneOf,
    /// The whole value has the shape of an example the user gives.
    Shaped,
    /// Something with the shape of the example is somewhere in the value.
    HasShaped,
    MoreThan,
    LessThan,
    Between,
    Empty,
    HasValue,
    /// A pattern typed by hand (for those who want one).
    Pattern,
}

impl How {
    pub const ALL: [How; 14] = [
        How::Contains,
        How::StartsWith,
        How::EndsWith,
        How::Exactly,
        How::Word,
        How::OneOf,
        How::Shaped,
        How::HasShaped,
        How::MoreThan,
        How::LessThan,
        How::Between,
        How::Empty,
        How::HasValue,
        How::Pattern,
    ];

    pub fn label(self) -> &'static str {
        match self {
            How::Contains => "contains",
            How::StartsWith => "starts with",
            How::EndsWith => "ends with",
            How::Exactly => "is exactly",
            How::Word => "has the word",
            How::OneOf => "is one of",
            How::Shaped => "is shaped like",
            How::HasShaped => "contains something shaped like",
            How::MoreThan => "is more than",
            How::LessThan => "is less than",
            How::Between => "is between",
            How::Empty => "is empty",
            How::HasValue => "has a value",
            How::Pattern => "matches the pattern",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            How::Contains => "The text anywhere in the value",
            How::StartsWith => "The value begins with the text",
            How::EndsWith => "The value ends with the text",
            How::Exactly => "The whole value is the text, nothing more",
            How::Word => {
                "The text as a word of its own: \"net\" in \"net use\", not in \"netsvcs\""
            }
            How::OneOf => "The value is any one of a list: type or paste them, one per line",
            How::Shaped => "Give one example value: the whole value must have its shape (digits where it has digits, letters where it has letters, the same marks between)",
            How::HasShaped => "Give one example: something with its shape must be somewhere in the value",
            How::MoreThan => "A number (or a date) larger than this",
            How::LessThan => "A number (or a date) smaller than this",
            How::Between => "From one number (or date) to another, both included",
            How::Empty => "The column has nothing in this row",
            How::HasValue => "The column has something in this row",
            How::Pattern => "A regular expression, for those who want to write one",
        }
    }

    /// Needs one column (cannot look in "every field").
    pub fn needs_column(self) -> bool {
        matches!(
            self,
            How::MoreThan | How::LessThan | How::Between | How::Empty | How::HasValue
        )
    }

    /// The ways offered for a column: a column of times is asked about in
    /// time (after, before, between); any other column has every way.
    pub fn choices(time: bool) -> &'static [How] {
        const FOR_TIME: [How; 5] = [
            How::MoreThan,
            How::LessThan,
            How::Between,
            How::Empty,
            How::HasValue,
        ];
        if time {
            &FOR_TIME
        } else {
            &How::ALL
        }
    }

    /// The way in words, in time words for a column of times.
    pub fn label_for(self, time: bool) -> &'static str {
        match (time, self) {
            (true, How::MoreThan) => "is on or after",
            (true, How::LessThan) => "is before",
            _ => self.label(),
        }
    }

    /// Whether "capitals matter" means anything for it.
    pub fn has_case(self) -> bool {
        matches!(
            self,
            How::Contains
                | How::StartsWith
                | How::EndsWith
                | How::Exactly
                | How::Word
                | How::Pattern
        )
    }
}

/// One condition.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    /// One column; `None` = every field.
    pub col: Option<String>,
    /// The column holds times: the condition is about a date and time.
    pub time: bool,
    pub how: How,
    /// The text, the number, or (one of) the list with one value per line.
    pub text: String,
    /// The upper end of "is between".
    pub to: String,
    /// How closely to follow the example ("is shaped like").
    pub shape: Shape,
    /// Capitals matter.
    pub case: bool,
    /// Keep the rows that do NOT match.
    pub not: bool,
}

/// A pattern with every `/` escaped, so it can sit between two slashes.
/// "a OR b OR c" for a few, nested in halves for many (the engine walks one
/// level per OR otherwise).
fn any_of(parts: &[String]) -> String {
    if parts.len() <= 32 {
        return parts.join(" OR ");
    }
    let (a, b) = parts.split_at(parts.len() / 2);
    format!("({}) OR ({})", any_of(a), any_of(b))
}

/// The values of a pasted or typed list: one per line (commas and
/// semicolons also separate), trimmed, no empties, no repeats, in order.
pub fn list_values(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in text.split(['\n', '\r', ',', ';']) {
        let v = v.trim();
        if !v.is_empty() && !out.iter().any(|o| o == v) {
            out.push(v.to_string());
        }
    }
    out
}

impl Row {
    /// What is still missing before it can be used, in words; `None` = ready.
    pub fn missing(&self) -> Option<&'static str> {
        if self.how.needs_column() && self.col.is_none() {
            return Some("choose a column");
        }
        if self.time {
            // a date, or a date and time, that reads as one; the ends in order
            const WRITE: &str =
                "type a date like 2026-11-12, or a date and time like 2026-11-12 18:30:00";
            let at = |t: &str| logseek_core::time::parse_time(t.trim());
            match self.how {
                How::MoreThan | How::LessThan if at(&self.text).is_none() => return Some(WRITE),
                How::Between => match (at(&self.text), at(&self.to)) {
                    (Some(a), Some(b)) if b < a => return Some("the end is before the start"),
                    (Some(_), Some(_)) => {}
                    _ => return Some(WRITE),
                },
                _ => {}
            }
        }
        match self.how {
            How::Empty | How::HasValue => None,
            How::Shaped | How::HasShaped if self.text.trim().is_empty() => {
                Some("type one example value")
            }
            How::OneOf if list_values(&self.text).is_empty() => Some("type or paste the values"),
            How::Between if self.text.trim().is_empty() || self.to.trim().is_empty() => {
                Some("type both ends")
            }
            _ if self.text.is_empty() => Some("type what to look for"),
            _ => None,
        }
    }

    /// The condition as a search term (without its "not"); `None` while
    /// something is missing.
    fn positive(&self) -> Option<String> {
        if self.missing().is_some() {
            return None;
        }
        let col = self.col.as_deref().map(column_ref);
        let text_mode = |mode: Mode| {
            Opts {
                text: self.text.clone(),
                mode,
                case: self.case,
                col: self.col.clone(),
            }
            .term()
        };
        match self.how {
            How::Contains => text_mode(Mode::Contains),
            How::StartsWith => text_mode(Mode::StartsWith),
            How::EndsWith => text_mode(Mode::EndsWith),
            How::Exactly => text_mode(Mode::Whole),
            How::Word => text_mode(Mode::Word),
            How::Pattern => text_mode(Mode::Pattern),
            How::Shaped | How::HasShaped => {
                let body = shape_pattern(self.text.trim(), self.shape)?;
                let re = between_slashes(&if self.how == How::Shaped {
                    format!("^{body}$")
                } else {
                    body
                });
                Some(match col {
                    Some(c) => format!("{c}~/{re}/"),
                    None => format!("~/{re}/"),
                })
            }
            How::OneOf => {
                let vals = list_values(&self.text);
                let parts: Vec<String> = vals
                    .iter()
                    .map(|v| match &col {
                        // the whole value, any capitals
                        Some(c) => format!("{c}={}", quote(v)),
                        // every field: the whole of some field is the value
                        None => format!("~/(?i)^{}$/", between_slashes(&regex::escape(v))),
                    })
                    .collect();
                Some(if parts.len() == 1 {
                    parts[0].clone()
                } else {
                    format!("({})", any_of(&parts))
                })
            }
            // a time is "on or after" its start: the moment itself counts
            How::MoreThan if self.time => Some(format!("{}>={}", col?, quote(self.text.trim()))),
            How::MoreThan => Some(format!("{}>{}", col?, quote(self.text.trim()))),
            How::LessThan => Some(format!("{}<{}", col?, quote(self.text.trim()))),
            How::Between if self.time => {
                // the end is written to the second and rows have fractions of
                // one: the end's own second counts, up to the next second
                let c = col?;
                let end = logseek_core::time::parse_time(self.to.trim())?;
                let next = logseek_core::types::fmt_time(end + 1_000_000);
                Some(format!(
                    "({c}>={} {c}<{})",
                    quote(self.text.trim()),
                    quote(&next)
                ))
            }
            How::Between => {
                let c = col?;
                Some(format!(
                    "({c}>={} {c}<={})",
                    quote(self.text.trim()),
                    quote(self.to.trim())
                ))
            }
            How::HasValue => Some(format!("{}:*", col?)),
            How::Empty => Some(format!("NOT {}:*", col?)),
        }
    }

    /// The condition as a search term, "not" included.
    pub fn term(&self) -> Option<String> {
        let t = self.positive()?;
        Some(match (self.not, self.how) {
            (false, _) => t,
            // "not empty" is "has a value", said directly
            (true, How::Empty) => format!("{}:*", column_ref(self.col.as_deref()?)),
            (true, _) => format!("NOT ({t})"),
        })
    }

    /// The condition in words.
    pub fn words(&self) -> String {
        let who = self.col.as_deref().unwrap_or("any field");
        let not = if self.not { "not: " } else { "" };
        let case = if self.case && self.how.has_case() {
            " (capitals matter)"
        } else {
            ""
        };
        match self.how {
            How::Empty | How::HasValue => format!("{not}{who} {}", self.how.label()),
            How::Shaped | How::HasShaped => {
                let shape = shape_pattern(self.text.trim(), self.shape)
                    .and_then(|p| shape_words(&p))
                    .unwrap_or_default();
                format!(
                    "{not}{who} {} \"{}\" ({shape})",
                    self.how.label(),
                    self.text.trim()
                )
            }
            How::OneOf => {
                let n = list_values(&self.text).len();
                format!(
                    "{not}{who} is one of {n} {}",
                    if n == 1 { "value" } else { "values" }
                )
            }
            How::Between => format!(
                "{not}{who} is between {} and {}",
                self.text.trim(),
                self.to.trim()
            ),
            _ if self.time => format!(
                "{not}{who} {} {}",
                self.how.label_for(true),
                self.text.trim()
            ),
            _ => format!("{not}{who} {} \"{}\"{case}", self.how.label(), self.text),
        }
    }
}

/// The conditions and how they join.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Build {
    pub rows: Vec<Row>,
    /// Every condition must hold (else: any one of them).
    pub all: bool,
}

impl Default for Build {
    fn default() -> Self {
        Build {
            rows: vec![Row::default()],
            all: true,
        }
    }
}

impl Build {
    /// The terms of the conditions that are ready, in order.
    pub fn terms(&self) -> Vec<String> {
        self.rows.iter().filter_map(Row::term).collect()
    }

    /// The whole search; `None` while no condition is ready.
    pub fn query(&self) -> Option<String> {
        let terms = self.terms();
        match terms.len() {
            0 => None,
            1 => Some(terms[0].clone()),
            _ if self.all => Some(
                terms
                    .iter()
                    .map(|t| {
                        // a term with an OR of its own is fenced off
                        if t.contains(" OR ") && !t.starts_with('(') && !t.starts_with("NOT (") {
                            format!("({t})")
                        } else {
                            t.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            _ => Some(format!("({})", any_of(&terms))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(col: Option<&str>, how: How, text: &str) -> Row {
        Row {
            col: col.map(str::to_string),
            how,
            text: text.into(),
            ..Default::default()
        }
    }

    fn ok(q: &str) {
        assert!(logseek_core::query::parse(q).is_ok(), "does not parse: {q}");
    }

    #[test]
    fn every_way_of_matching_writes_a_search_the_engine_reads() {
        let rows = [
            (row(None, How::Contains, "power shell"), "\"power shell\""),
            (
                row(Some("FileName"), How::StartsWith, "power"),
                "FileName~/(?i)^power/",
            ),
            (
                row(Some("FileName"), How::Exactly, "cmd.exe"),
                "FileName~/(?i)^cmd\\.exe$/",
            ),
            (row(None, How::Word, "net"), "~/(?i)\\bnet\\b/"),
            (row(Some("Bytes"), How::MoreThan, "500"), "Bytes>\"500\""),
            (row(Some("User"), How::Empty, ""), "NOT User:*"),
            (row(Some("User"), How::HasValue, ""), "User:*"),
            (
                row(Some("User"), How::OneOf, "ann\nbob , carol;ann"),
                "(User=\"ann\" OR User=\"bob\" OR User=\"carol\")",
            ),
            (row(None, How::OneOf, "1.2.3.4"), "~/(?i)^1\\.2\\.3\\.4$/"),
        ];
        for (r, want) in rows {
            let got = r.term().unwrap();
            assert_eq!(got, want);
            ok(&got);
        }
        let b = Row {
            to: "20".into(),
            ..row(Some("Bytes"), How::Between, "10")
        };
        assert_eq!(b.term().unwrap(), "(Bytes>=\"10\" Bytes<=\"20\")");
        ok(&b.term().unwrap());
    }

    fn shaped(how: How, example: &str, shape: Shape) -> Row {
        Row {
            how,
            text: example.into(),
            shape,
            ..Default::default()
        }
    }

    #[test]
    fn a_shape_comes_from_the_example_and_from_nothing_else() {
        // digits where it has digits, the marks kept: any example at all
        let p = shape_pattern("10.50.0.58", Shape::AnyLength).unwrap();
        assert_eq!(p, "[0-9]+\\.[0-9]+\\.[0-9]+\\.[0-9]+");
        let re = regex::Regex::new(&format!("^{p}$")).unwrap();
        assert!(re.is_match("203.0.113.4") && !re.is_match("10.50.0") && !re.is_match("a.b.c.d"));
        // the same lengths as the example
        let p = shape_pattern("PS-0042.exe", Shape::SameLength).unwrap();
        assert_eq!(p, "[A-Za-z]{2}\\-[0-9]{4}\\.[A-Za-z]{3}");
        let re = regex::Regex::new(&format!("^{p}$")).unwrap();
        assert!(re.is_match("AB-1234.dll") && !re.is_match("ABC-1234.dll"));
        // letters and digits alike: a code of any make-up
        let p = shape_pattern("a1b2-c3", Shape::Alike).unwrap();
        assert_eq!(p, "[A-Za-z0-9]+\\-[A-Za-z0-9]+");
        // an example nobody thought of in advance works the same way
        let p = shape_pattern("ticket #4471 (open)", Shape::AnyLength).unwrap();
        let re = regex::Regex::new(&format!("^{p}$")).unwrap();
        assert!(re.is_match("order #9 (closed)") && !re.is_match("ticket 4471 open"));
        assert_eq!(shape_pattern("", Shape::AnyLength), None);
    }

    #[test]
    fn a_shape_reads_back_in_words_and_as_a_search() {
        let p = shape_pattern("10.50.0.58", Shape::AnyLength).unwrap();
        assert_eq!(
            shape_words(&p).unwrap(),
            "digits, \".\", digits, \".\", digits, \".\", digits"
        );
        let p = shape_pattern("PS-0042.exe", Shape::SameLength).unwrap();
        assert_eq!(
            shape_words(&p).unwrap(),
            "2 letters, \"-\", 4 digits, \".\", 3 letters"
        );
        // other patterns, and plain text, are not shapes
        assert_eq!(shape_words("a.*b"), None);
        assert_eq!(shape_words("\\d+"), None);
        assert_eq!(shape_words("cmd\\.exe"), None);
        // as a condition: the whole value, or somewhere in it
        let whole = shaped(How::Shaped, "10.50.0.58", Shape::AnyLength);
        assert_eq!(
            whole.term().unwrap(),
            "~/^[0-9]+\\.[0-9]+\\.[0-9]+\\.[0-9]+$/"
        );
        ok(&whole.term().unwrap());
        let mut part = shaped(How::HasShaped, "a/b", Shape::SameLength);
        part.col = Some("Path".into());
        assert_eq!(part.term().unwrap(), "Path~/[A-Za-z]{1}\\/[A-Za-z]{1}/");
        ok(&part.term().unwrap());
        assert_eq!(
            whole.words(),
            "any field is shaped like \"10.50.0.58\" (digits, \".\", digits, \".\", digits, \".\", digits)"
        );
        assert_eq!(
            shaped(How::Shaped, " ", Shape::AnyLength).missing(),
            Some("type one example value")
        );
    }

    #[test]
    fn not_and_missing_parts() {
        let mut r = row(Some("FileName"), How::Contains, "cmd");
        r.not = true;
        assert_eq!(r.term().unwrap(), "NOT (FileName:\"cmd\")");
        ok(&r.term().unwrap());
        // "not empty" is said directly
        let mut e = row(Some("User"), How::Empty, "");
        e.not = true;
        assert_eq!(e.term().unwrap(), "User:*");
        // nothing typed, or a number test with no column: not ready, and it says why
        assert_eq!(
            row(None, How::Contains, "").missing(),
            Some("type what to look for")
        );
        assert_eq!(
            row(None, How::MoreThan, "5").missing(),
            Some("choose a column")
        );
        assert_eq!(row(None, How::MoreThan, "5").term(), None);
        assert_eq!(
            row(Some("a"), How::OneOf, " \n ").missing(),
            Some("type or paste the values")
        );
    }

    #[test]
    fn several_conditions_join_as_all_or_any() {
        let mut b = Build {
            rows: vec![
                row(Some("FileName"), How::StartsWith, "power"),
                row(Some("User"), How::OneOf, "ann\nbob"),
                row(None, How::Contains, ""), // not ready: left out
            ],
            all: true,
        };
        let q = b.query().unwrap();
        assert_eq!(q, "FileName~/(?i)^power/ (User=\"ann\" OR User=\"bob\")");
        ok(&q);
        b.all = false;
        let q = b.query().unwrap();
        assert_eq!(
            q,
            "(FileName~/(?i)^power/ OR (User=\"ann\" OR User=\"bob\"))"
        );
        ok(&q);
        assert_eq!(Build::default().query(), None);
    }

    #[test]
    fn conditions_read_in_words() {
        assert_eq!(
            row(Some("FileName"), How::StartsWith, "power").words(),
            "FileName starts with \"power\""
        );
        assert_eq!(
            row(Some("User"), How::OneOf, "a\nb").words(),
            "User is one of 2 values"
        );
    }

    fn time_row(how: How, text: &str, to: &str) -> Row {
        Row {
            col: Some("ts".into()),
            time: true,
            how,
            text: text.into(),
            to: to.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_time_column_offers_only_the_ways_that_fit_a_time_in_time_words() {
        assert_eq!(
            How::choices(true),
            &[How::MoreThan, How::LessThan, How::Between, How::Empty, How::HasValue]
        );
        assert_eq!(How::MoreThan.label_for(true), "is on or after");
        assert_eq!(How::LessThan.label_for(true), "is before");
        assert_eq!(How::Between.label_for(true), "is between");
        // any other column keeps every way, in its usual words
        assert_eq!(How::choices(false), &How::ALL);
        assert_eq!(How::MoreThan.label_for(false), "is more than");
    }

    #[test]
    fn time_conditions_become_terms_that_compare_times() {
        assert_eq!(
            time_row(How::MoreThan, "2026-11-12 18:00:00", "").term().as_deref(),
            Some(r#"ts>="2026-11-12 18:00:00""#)
        );
        assert_eq!(
            time_row(How::LessThan, "2026-11-12 18:00:00", "").term().as_deref(),
            Some(r#"ts<"2026-11-12 18:00:00""#)
        );
        assert_eq!(
            time_row(How::Between, "2026-11-12 18:00:00", "2026-11-12 19:30:00")
                .term()
                .as_deref(),
            // the end's own second counts: everything before the next one
            Some(r#"(ts>="2026-11-12 18:00:00" ts<"2026-11-12 19:30:01")"#)
        );
        // a date alone is a time too: the start of that day
        assert_eq!(
            time_row(How::MoreThan, "2026-11-12", "").term().as_deref(),
            Some(r#"ts>="2026-11-12""#)
        );
    }

    #[test]
    fn a_time_condition_says_what_is_wrong_with_what_was_typed() {
        let bad = time_row(How::MoreThan, "12/11/2026 6pm", "");
        assert_eq!(bad.term(), None);
        assert_eq!(
            bad.missing(),
            Some("type a date like 2026-11-12, or a date and time like 2026-11-12 18:30:00")
        );
        let back = time_row(How::Between, "2026-11-12 19:00:00", "2026-11-12 18:00:00");
        assert_eq!(back.term(), None);
        assert_eq!(back.missing(), Some("the end is before the start"));
        // a number column with the same typing is left alone
        let number = Row {
            col: Some("n".into()),
            how: How::MoreThan,
            text: "5".into(),
            ..Default::default()
        };
        assert_eq!(number.term().as_deref(), Some(r#"n>"5""#));
    }

    #[test]
    fn a_time_condition_reads_as_a_sentence_about_time() {
        assert_eq!(
            time_row(How::MoreThan, "2026-11-12 18:00:00", "").words(),
            "ts is on or after 2026-11-12 18:00:00"
        );
        assert_eq!(
            time_row(How::Between, "2026-11-12", "2026-11-13").words(),
            "ts is between 2026-11-12 and 2026-11-13"
        );
    }
}
