//! Why a row is here: while the rows a list search found are showing, each
//! row on screen says which of the keywords it holds and in which fields.
//! Worked out from the cells of the rows on screen only, so it costs the
//! same on a thousand rows as on a hundred million.

use aho_corasick::{AhoCorasick, MatchKind};

pub struct Marks {
    ac: AhoCorasick,
    pub terms: Vec<String>,
    word: bool,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Marks {
    /// The keywords to look for (the ones the list search found), with the
    /// search's own choices: capitals matter, whole words only.
    pub fn new(terms: Vec<String>, case: bool, word: bool) -> Option<Marks> {
        if terms.is_empty() || terms.iter().any(String::is_empty) {
            return None;
        }
        let ac = AhoCorasick::builder()
            .ascii_case_insensitive(!case)
            .match_kind(MatchKind::Standard)
            .build(&terms)
            .ok()?;
        Some(Marks { ac, terms, word })
    }

    /// (keyword, column) for every keyword in every cell of a row: each pair
    /// once, by column then by keyword.
    pub fn of_row(&self, cells: &[Option<String>]) -> Vec<(usize, usize)> {
        let mut out: Vec<(usize, usize)> = Vec::new();
        for (c, v) in cells.iter().enumerate() {
            let Some(v) = v.as_deref() else { continue };
            let from = out.len();
            for m in self.ac.find_overlapping_iter(v) {
                if self.word
                    && (v[..m.start()].chars().next_back().is_some_and(is_word)
                        || v[m.end()..].chars().next().is_some_and(is_word))
                {
                    continue;
                }
                let t = m.pattern().as_usize();
                if !out[from..].iter().any(|&(k, _)| k == t) {
                    out.push((t, c));
                }
            }
            out[from..].sort_unstable();
        }
        out
    }
}

/// What the band shows, whole: the keyword when there is one, the count
/// when there are several, and in how many fields.
pub fn short(hits: &[(usize, usize)], terms: &[String]) -> String {
    let mut ks: Vec<usize> = hits.iter().map(|h| h.0).collect();
    ks.sort_unstable();
    ks.dedup();
    let fields = hits.len();
    let f = format!("{fields} {}", if fields == 1 { "field" } else { "fields" });
    match ks.len() {
        0 => String::new(),
        1 => format!("{}  ·  {f}", terms[ks[0]]),
        n => format!("{n} keywords  ·  {f}"),
    }
}

/// Every keyword of the row with the fields it is in, one keyword per line.
pub fn full(hits: &[(usize, usize)], terms: &[String], columns: &[String]) -> String {
    let mut ks: Vec<usize> = hits.iter().map(|h| h.0).collect();
    ks.sort_unstable();
    ks.dedup();
    ks.iter()
        .map(|&k| {
            let cols: Vec<&str> = hits
                .iter()
                .filter(|h| h.0 == k)
                .map(|h| columns.get(h.1).map_or("", String::as_str))
                .collect();
            format!("{}  in  {}", terms[k], cols.join(", "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn a_row_says_which_keywords_it_holds_and_in_which_fields() {
        let m = Marks::new(s(&["whoami", "bob", "10.9.8.7"]), false, false).unwrap();
        let cells = vec![
            Some("WS-01".to_string()),
            Some("Bob".to_string()),
            None,
            Some("cmd /c whoami & whoami /all (as bob)".to_string()),
        ];
        let hits = m.of_row(&cells);
        // each pair once, even when a keyword is in a cell twice
        assert_eq!(hits, vec![(1, 1), (0, 3), (1, 3)]);
        assert_eq!(short(&hits, &m.terms), "2 keywords  ·  3 fields");
        let cols = s(&["Host", "User", "Extra", "CommandLine"]);
        assert_eq!(
            full(&hits, &m.terms, &cols),
            "whoami  in  CommandLine\nbob  in  User, CommandLine"
        );
        // one keyword reads as itself
        let one = m.of_row(&[Some("x whoami y".to_string())]);
        assert_eq!(short(&one, &m.terms), "whoami  ·  1 field");
        // nothing found: nothing shown
        assert_eq!(
            short(&m.of_row(&[Some("nothing".to_string())]), &m.terms),
            ""
        );
    }

    #[test]
    fn the_search_choices_are_respected() {
        // capitals matter
        let m = Marks::new(s(&["bob"]), true, false).unwrap();
        assert!(m.of_row(&[Some("Bob".to_string())]).is_empty());
        // whole words only: "net" is in "net use", not in "netsvcs"
        let m = Marks::new(s(&["net"]), false, true).unwrap();
        assert!(m.of_row(&[Some("netsvcs".to_string())]).is_empty());
        assert_eq!(m.of_row(&[Some("net use".to_string())]), vec![(0, 0)]);
        // no keywords, or an empty one: nothing to mark with
        assert!(Marks::new(Vec::new(), false, false).is_none());
        assert!(Marks::new(s(&["a", ""]), false, false).is_none());
    }
}
