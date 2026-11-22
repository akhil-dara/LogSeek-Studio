//! The query workspace: write a query with room to think, see whether it
//! can run before running it, and get back to anything run before.

use serde::{Deserialize, Serialize};

/// One query that was run.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Ran {
    pub query: String,
    /// Rows it kept, and the rows in the file.
    pub rows: u64,
    pub of: u64,
    pub file: String,
    /// When (seconds since 1970, UTC).
    pub at: u64,
}

/// A query typed over several lines, as the one line the engine runs.
pub fn one_line(text: &str) -> String {
    crate::querylines::build(text).query
}

/// An example query for the empty editor, made from the open file's own
/// columns (never from names the app assumes a file has).
pub fn example(columns: &[String], time_col: Option<usize>) -> String {
    let usable: Vec<&str> = columns
        .iter()
        .enumerate()
        .filter(|(i, n)| Some(*i) != time_col && !n.is_empty() && !n.contains(char::is_whitespace))
        .map(|(_, n)| n.as_str())
        .collect();
    let a = usable.first().copied().unwrap_or("column");
    let b = usable.get(1).copied().unwrap_or(a);
    let mut s = format!("{a}=value\nNOT {b}:text*");
    if time_col.is_some() {
        s.push_str("\n@2026-11-12T10:00..2026-11-12T11:00");
    }
    s
}

/// Note a query that ran. Running the same query on the same file again
/// updates its entry and moves it to the top; nothing is ever dropped.
pub fn note(log: &mut Vec<Ran>, r: Ran) {
    if r.query.trim().is_empty() {
        return;
    }
    log.retain(|x| !(x.query == r.query && x.file == r.file));
    log.insert(0, r);
}

/// The entries whose query or file contains every word of `needle`.
pub fn find(log: &[Ran], needle: &str) -> Vec<usize> {
    let words: Vec<String> = needle.split_whitespace().map(str::to_lowercase).collect();
    (0..log.len())
        .filter(|&i| {
            let hay = format!("{} {}", log[i].query, log[i].file).to_lowercase();
            words.iter().all(|w| hay.contains(w))
        })
        .collect()
}

/// Why a query cannot run, in words; `None` when it can.
pub fn problem(text: &str) -> Option<String> {
    crate::querylines::problem(text).map(|(line, msg)| {
        if text.lines().filter(|l| !l.trim().is_empty()).count() > 1 {
            format!("Line {}: {msg}", line + 1)
        } else {
            msg
        }
    })
}

#[derive(Default)]
pub struct WorkState {
    pub text: String,
    pub find: String,
    /// 0 history, 1 saved.
    pub tab: u8,
    pub save_name: String,
    pub focus: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_example_query_uses_the_open_files_own_columns() {
        let cols: Vec<String> = ["timestamp", "Host Name", "user", "status"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // the time column is left to the period line; a name with a space is skipped
        assert_eq!(
            example(&cols, Some(0)),
            "user=value\nNOT status:text*\n@2026-11-12T10:00..2026-11-12T11:00"
        );
        // one usable column serves both lines; no time column, no period line
        assert_eq!(
            example(&["Kind".to_string()], None),
            "Kind=value\nNOT Kind:text*"
        );
        // nothing usable: plain words
        assert_eq!(example(&[], None), "column=value\nNOT column:text*");
    }

    fn ran(q: &str, file: &str, rows: u64, at: u64) -> Ran {
        Ran {
            query: q.into(),
            rows,
            of: 100,
            file: file.into(),
            at,
        }
    }

    #[test]
    fn history_keeps_everything_newest_first_and_never_twice() {
        let mut log = Vec::new();
        note(&mut log, ran("a", "f.csv", 1, 10));
        note(&mut log, ran("b", "f.csv", 2, 20));
        note(&mut log, ran("a", "f.csv", 5, 30));
        // "a" ran again: one entry, on top, with the new count
        assert_eq!(log.len(), 2);
        assert_eq!(
            (log[0].query.as_str(), log[0].rows, log[0].at),
            ("a", 5, 30)
        );
        // the same query on another file is its own entry
        note(&mut log, ran("a", "g.csv", 9, 40));
        assert_eq!(log.len(), 3);
        // an empty query is not history
        note(&mut log, ran("  ", "f.csv", 0, 50));
        assert_eq!(log.len(), 3);
        assert_eq!(find(&log, "A g.csv"), vec![0]);
        assert_eq!(find(&log, "").len(), 3);
    }

    #[test]
    fn several_lines_run_as_one_and_problems_are_said_before_running() {
        assert_eq!(
            one_line("host=web1\n  status>=500\n"),
            "host=web1 status>=500"
        );
        assert_eq!(problem("host=web1\nstatus>=500"), None);
        assert_eq!(problem("   "), None);
        assert!(problem("host=(").is_some());
        // steps the grid cannot run are refused here, not silently ignored
        assert!(problem("host=web1 | stats count by user").is_some());
    }
}
