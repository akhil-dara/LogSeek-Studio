//! What a file keeps between runs, and saying so.
//!
//! Everything done with a file is kept with it: its tags and made columns
//! next to its index (already), and here its view (search and filters, sort,
//! hidden columns, grouping), so opening it again brings it back as it was
//! left. Opening it says in one line what came back, and "Start from scratch"
//! clears what the user picks. The file itself is never changed.

use serde::{Deserialize, Serialize};

/// A file's view as it was left.
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
#[serde(default)]
pub struct KeptView {
    /// The file, as opened.
    pub path: String,
    /// The search, with every filter written into it.
    pub query: String,
    /// The sort: column and largest first.
    pub sort: Option<(String, bool)>,
    pub hidden: Vec<String>,
    pub group_by: Vec<String>,
    pub group_sort: u8,
    /// When it was left (seconds since 1970, UTC).
    pub at: u64,
}

impl KeptView {
    /// Nothing differs from a file opened for the first time.
    pub fn is_empty(&self) -> bool {
        self.query.trim().is_empty()
            && self.sort.is_none()
            && self.hidden.is_empty()
            && self.group_by.is_empty()
    }
}

/// Keep `v` for its file (an empty view is simply forgotten).
pub fn keep(list: &mut Vec<KeptView>, v: KeptView) {
    list.retain(|k| k.path != v.path);
    if !v.is_empty() {
        list.push(v);
    }
}

pub fn find<'a>(list: &'a [KeptView], path: &str) -> Option<&'a KeptView> {
    list.iter().find(|k| k.path == path)
}

pub fn forget(list: &mut Vec<KeptView>, path: &str) {
    list.retain(|k| k.path != path);
}

/// "a", "a and b", "a, b and c".
pub fn and_list(items: &[String]) -> String {
    match items.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

/// `n` with its noun, one or many ("1 field", "3 fields").
pub fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{} {many}", crate::moment::group(n as u64))
    }
}

/// The parts of a view in words: "the search", "2 hidden columns", "grouped
/// by App", "sorted by Time, largest first".
pub fn view_words(v: &KeptView) -> Vec<String> {
    let mut out = Vec::new();
    if !v.query.trim().is_empty() {
        out.push("the search and filters".to_string());
    }
    if let Some((col, big)) = &v.sort {
        out.push(format!(
            "sorted by {col}, {}",
            if *big {
                "largest first"
            } else {
                "smallest first"
            }
        ));
    }
    if !v.hidden.is_empty() {
        out.push(plural(v.hidden.len(), "hidden column", "hidden columns"));
    }
    if !v.group_by.is_empty() {
        out.push(format!("grouped by {}", and_list(&v.group_by)));
    }
    out
}

/// What came back with a file, in one sentence; `None` when nothing did.
pub fn came_back_line(view: &[String], tagged_rows: usize, made: &[String]) -> Option<String> {
    let mut parts: Vec<String> = view.to_vec();
    if tagged_rows > 0 {
        parts.push(format!("tags on {}", plural(tagged_rows, "row", "rows")));
    }
    if !made.is_empty() {
        parts.push(format!(
            "{} ({})",
            plural(made.len(), "made column", "made columns"),
            made.join(", ")
        ));
    }
    if parts.is_empty() {
        return None;
    }
    let lead = if view.is_empty() {
        "Saved with this file"
    } else {
        "Opened as you left it"
    };
    Some(format!("{lead}: {}.", and_list(&parts)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_of_a_thing_is_said_as_one() {
        assert_eq!(plural(1, "field", "fields"), "1 field");
        assert_eq!(plural(1_204, "field", "fields"), "1,204 fields");
        assert_eq!(plural(0, "column", "columns"), "0 columns");
    }

    fn v(path: &str, query: &str) -> KeptView {
        KeptView {
            path: path.into(),
            query: query.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_view_is_kept_per_file_and_an_empty_one_forgotten() {
        let mut list = Vec::new();
        keep(&mut list, v("a.csv", "error"));
        keep(&mut list, v("b.csv", "host=web1"));
        keep(&mut list, v("a.csv", "warning"));
        assert_eq!(list.len(), 2);
        assert_eq!(find(&list, "a.csv").unwrap().query, "warning");
        // left with nothing set: nothing to bring back
        keep(&mut list, v("b.csv", "  "));
        assert!(find(&list, "b.csv").is_none());
        forget(&mut list, "a.csv");
        assert!(list.is_empty());
    }

    #[test]
    fn the_view_reads_in_words() {
        let k = KeptView {
            path: "a.csv".into(),
            query: "error".into(),
            sort: Some(("Time".into(), false)),
            hidden: vec!["Id".into(), "Guid".into()],
            group_by: vec!["App".into(), "User".into()],
            ..Default::default()
        };
        assert_eq!(
            view_words(&k),
            [
                "the search and filters",
                "sorted by Time, smallest first",
                "2 hidden columns",
                "grouped by App and User"
            ]
        );
        assert!(view_words(&v("a.csv", "")).is_empty());
    }

    #[test]
    fn opening_says_what_came_back() {
        let view = vec![
            "the search and filters".to_string(),
            "1 hidden column".to_string(),
        ];
        assert_eq!(
            came_back_line(&view, 3, &["user".into(), "host".into()]).unwrap(),
            "Opened as you left it: the search and filters, 1 hidden column, tags on 3 rows and 2 made columns (user, host)."
        );
        // no view, but work saved with the file
        assert_eq!(
            came_back_line(&[], 1, &[]).unwrap(),
            "Saved with this file: tags on 1 row."
        );
        // a file opened for the first time: nothing to say
        assert!(came_back_line(&[], 0, &[]).is_none());
    }
}
