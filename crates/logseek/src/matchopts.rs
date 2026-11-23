//! How to match: the choices a "find" box usually hides behind tick boxes
//! (do capitals matter, part of a value or all of it, a whole word, the
//! start or the end, every field or one column), written as one search
//! term in the same language the search box takes. So a choice made here
//! is visible, editable and shareable like any other search.

use logseek_core::formula::{column_ref, quote};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Contains,
    StartsWith,
    EndsWith,
    /// The value is exactly the text.
    Whole,
    /// The text as a word of its own: "net" in "net use", not in "netsvcs".
    Word,
    /// The text is a pattern (regular expression).
    Pattern,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Opts {
    pub text: String,
    pub mode: Mode,
    /// Capitals matter ("Bob" does not find "bob").
    pub case: bool,
    /// One column; `None` = every field.
    pub col: Option<String>,
}

/// A regex with every `/` escaped, so it can sit between two slashes.
fn between_slashes(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    let mut esc = false;
    for c in p.chars() {
        if c == '/' && !esc {
            out.push('\\');
        }
        esc = c == '\\' && !esc;
        out.push(c);
    }
    out
}

impl Opts {
    /// The search term these choices mean; `None` while there is no text.
    pub fn term(&self) -> Option<String> {
        let t = self.text.as_str();
        if t.is_empty() {
            return None;
        }
        // the plain case is the plain (and fastest) search
        if self.mode == Mode::Contains && !self.case {
            return Some(match &self.col {
                None => quote(t),
                // `*` after a colon is a wildcard: text holding one goes the pattern way
                Some(c) if !t.contains('*') => format!("{}:{}", column_ref(c), quote(t)),
                Some(c) => format!(
                    "{}~/(?i){}/",
                    column_ref(c),
                    between_slashes(&regex::escape(t))
                ),
            });
        }
        let lit = regex::escape(t);
        let body = match self.mode {
            Mode::Contains => lit,
            Mode::StartsWith => format!("^{lit}"),
            Mode::EndsWith => format!("{lit}$"),
            Mode::Whole => format!("^{lit}$"),
            Mode::Word => format!("\\b{lit}\\b"),
            Mode::Pattern => t.to_string(),
        };
        let flag = if self.case { "" } else { "(?i)" };
        let re = between_slashes(&format!("{flag}{body}"));
        Some(match &self.col {
            None => format!("~/{re}/"),
            Some(c) => format!("{}~/{re}/", column_ref(c)),
        })
    }

    /// The choices in words: `any field starts with "cmd", capitals matter`.
    #[cfg(test)]
    pub fn words(&self) -> String {
        let who = self.col.as_deref().unwrap_or("any field");
        let verb = match self.mode {
            Mode::Contains => "contains",
            Mode::StartsWith => "starts with",
            Mode::EndsWith => "ends with",
            Mode::Whole => "is exactly",
            Mode::Word => "has the word",
            Mode::Pattern => "matches the pattern",
        };
        format!(
            "{who} {verb} \"{}\", capitals {}",
            self.text,
            if self.case { "matter" } else { "do not matter" }
        )
    }
}

/// A pattern the choices above write, read back: (verb, text, capitals
/// matter). `None` for any other pattern.
pub fn pattern_words(p: &str) -> Option<(&'static str, String, bool)> {
    let text = logseek_core::query::exec::plain_text_of(p)?;
    let case = !p.starts_with("(?i)");
    let rest = p.strip_prefix("(?i)").unwrap_or(p);
    let starts = rest.starts_with('^');
    let ends = rest.ends_with('$') && !rest.ends_with("\\$");
    let word = rest.starts_with("\\b") && rest.ends_with("\\b");
    let verb = match (starts, ends, word) {
        (true, true, _) => "is exactly",
        (true, false, _) => "starts with",
        (false, true, _) => "ends with",
        (_, _, true) => "has the word",
        _ => "contains",
    };
    Some((verb, text, case))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(text: &str, mode: Mode, case: bool, col: Option<&str>) -> Opts {
        Opts {
            text: text.into(),
            mode,
            case,
            col: col.map(str::to_string),
        }
    }

    #[test]
    fn the_plain_choice_is_the_plain_search() {
        assert_eq!(
            o("power shell", Mode::Contains, false, None)
                .term()
                .unwrap(),
            "\"power shell\""
        );
        assert_eq!(
            o("cmd", Mode::Contains, false, Some("FileName"))
                .term()
                .unwrap(),
            "FileName:\"cmd\""
        );
        assert_eq!(o("", Mode::Whole, true, None).term(), None);
    }

    #[test]
    fn every_choice_is_a_term_the_engine_reads() {
        let cases = [
            (o("Bob", Mode::Contains, true, None), "~/Bob/"),
            (o("cmd", Mode::StartsWith, false, None), "~/(?i)^cmd/"),
            (
                o(".exe", Mode::EndsWith, false, Some("FileName")),
                "FileName~/(?i)\\.exe$/",
            ),
            (
                o("cmd.exe", Mode::Whole, true, Some("File Name")),
                "`File Name`~/^cmd\\.exe$/",
            ),
            (o("net", Mode::Word, false, None), "~/(?i)\\bnet\\b/"),
            (
                o("^(cmd|pwsh)$", Mode::Pattern, true, None),
                "~/^(cmd|pwsh)$/",
            ),
            // a slash in the text cannot end the pattern early
            (o("C:/x", Mode::StartsWith, true, None), "~/^C:\\/x/"),
            // a star after a colon would be a wildcard: it goes the pattern way
            (
                o("a*b", Mode::Contains, false, Some("Cmd")),
                "Cmd~/(?i)a\\*b/",
            ),
        ];
        for (opts, want) in cases {
            let got = opts.term().unwrap();
            assert_eq!(got, want);
            assert!(logseek_core::query::parse(&got).is_ok(), "{got}");
        }
    }

    #[test]
    fn choices_read_back_in_words() {
        assert_eq!(
            o("cmd", Mode::StartsWith, true, Some("FileName")).words(),
            "FileName starts with \"cmd\", capitals matter"
        );
        assert_eq!(
            pattern_words("(?i)^cmd\\.exe$"),
            Some(("is exactly", "cmd.exe".into(), false))
        );
        assert_eq!(
            pattern_words("\\bnet\\b"),
            Some(("has the word", "net".into(), true))
        );
        assert_eq!(
            pattern_words("(?i)\\.exe$"),
            Some(("ends with", ".exe".into(), false))
        );
        assert_eq!(pattern_words("Bob"), Some(("contains", "Bob".into(), true)));
        assert_eq!(pattern_words("a.*b"), None);
    }
}
