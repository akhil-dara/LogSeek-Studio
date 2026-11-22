//! Tabs: each is a view of a file. A file can have several tabs, each with
//! its own search, filters and sort (the app keeps those by tab id).

pub struct Tab {
    pub id: u64,
    pub path: String,
}

#[derive(Default)]
pub struct Tabs {
    pub list: Vec<Tab>,
    pub current: Option<u64>,
    next: u64,
}

impl Tabs {
    fn add_after_current(&mut self, path: &str) -> u64 {
        self.next += 1;
        let id = self.next;
        let at = self
            .current
            .and_then(|c| self.list.iter().position(|t| t.id == c))
            .map_or(self.list.len(), |i| i + 1);
        self.list.insert(
            at,
            Tab {
                id,
                path: path.to_string(),
            },
        );
        id
    }

    pub fn path(&self, id: u64) -> Option<&str> {
        self.list
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.path.as_str())
    }

    /// A file is being opened: the current tab if it shows that file, else
    /// the file's first tab, else a new one. That tab becomes current.
    pub fn open_file(&mut self, path: &str) -> u64 {
        let cur = self.current.filter(|&c| self.path(c) == Some(path));
        let id = cur
            .or_else(|| self.list.iter().find(|t| t.path == path).map(|t| t.id))
            .unwrap_or_else(|| self.add_after_current(path));
        self.current = Some(id);
        id
    }

    /// A second tab on the current tab's file, right after it; it becomes current.
    pub fn duplicate(&mut self) -> Option<u64> {
        let path = self.path(self.current?)?.to_string();
        let id = self.add_after_current(&path);
        self.current = Some(id);
        Some(id)
    }

    /// Close a tab (never the last one). When it was current, the tab to
    /// show instead: its right neighbour, else its left one.
    pub fn close(&mut self, id: u64) -> Option<u64> {
        if self.list.len() < 2 {
            return None;
        }
        let i = self.list.iter().position(|t| t.id == id)?;
        self.list.remove(i);
        if self.current != Some(id) {
            return None;
        }
        self.current = None;
        self.list
            .get(i)
            .or_else(|| self.list.get(i.wrapping_sub(1)))
            .map(|t| t.id)
    }

    /// The tab after (or before) the current one, wrapping round.
    pub fn cycle(&self, forward: bool) -> Option<u64> {
        let n = self.list.len();
        let i = self.list.iter().position(|t| Some(t.id) == self.current)?;
        if n < 2 {
            return None;
        }
        let j = if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        };
        Some(self.list[j].id)
    }

    /// The tab's name: what tells its file from the other open files (the
    /// full name is on the tab's hover and in the window title), numbered
    /// when the file has several tabs.
    pub fn label(&self, id: u64) -> String {
        let Some(path) = self.path(id) else {
            return String::new();
        };
        // every open file once, in tab order
        let mut paths: Vec<&str> = Vec::new();
        for t in &self.list {
            if !paths.contains(&t.path.as_str()) {
                paths.push(&t.path);
            }
        }
        let full: Vec<String> = paths
            .iter()
            .map(|p| crate::fileset::short_name(std::path::Path::new(p)))
            .collect();
        let short = short_labels(&full);
        let name = paths
            .iter()
            .position(|p| *p == path)
            .and_then(|k| short.get(k).cloned())
            .unwrap_or_else(|| crate::fileset::short_name(std::path::Path::new(path)));
        let same: Vec<u64> = self
            .list
            .iter()
            .filter(|t| t.path == path)
            .map(|t| t.id)
            .collect();
        if same.len() < 2 {
            return name;
        }
        let k = same.iter().position(|&x| x == id).unwrap_or(0) + 1;
        format!("{name}  ({k})")
    }
}

/// The open tabs whose file name has `typed` in it (letter case ignored), in
/// tab order: (place in the list of tabs, the name). Everything when nothing
/// is typed. For the list of open files, which finds one among many.
pub fn find_tabs<'a>(names: &'a [String], typed: &str) -> Vec<(usize, &'a String)> {
    let t = typed.trim().to_lowercase();
    names
        .iter()
        .enumerate()
        .filter(|(_, n)| t.is_empty() || n.to_lowercase().contains(&t))
        .collect()
}

/// Each name without the beginning all of them share, so files unpacked from
/// one archive, or exported with one long prefix, read by what tells them
/// apart. The shared part ends at a mark between words (a space, `_`, `-`,
/// `.`, `·`, `/`), never inside a word. The names come back unchanged when
/// there is one name, nothing shared, or a name that would be left empty.
pub fn short_labels(names: &[String]) -> Vec<String> {
    if names.len() < 2 {
        return names.to_vec();
    }
    let first: Vec<char> = names[0].chars().collect();
    let mut common = first.len();
    for n in &names[1..] {
        let same = first
            .iter()
            .zip(n.chars())
            .take_while(|(a, b)| **a == *b)
            .count();
        common = common.min(same);
    }
    // back to just after the last mark between words inside the shared part
    let is_mark = |c: char| matches!(c, ' ' | '_' | '-' | '.' | '·' | '/' | '\\');
    let cut = first[..common]
        .iter()
        .rposition(|c| is_mark(*c))
        .map_or(0, |i| i + 1);
    if cut == 0 {
        return names.to_vec();
    }
    let out: Vec<String> = names
        .iter()
        .map(|n| n.chars().skip(cut).collect::<String>().trim().to_string())
        .collect();
    if out.iter().any(|n| n.is_empty()) {
        return names.to_vec();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(t: &Tabs) -> Vec<(u64, &str)> {
        t.list.iter().map(|x| (x.id, x.path.as_str())).collect()
    }

    #[test]
    fn a_file_gets_one_tab_and_more_only_when_asked() {
        let mut t = Tabs::default();
        let a = t.open_file("a.csv");
        let b = t.open_file("b.csv");
        assert_eq!(t.open_file("a.csv"), a);
        assert_eq!(ids(&t), vec![(a, "a.csv"), (b, "b.csv")]);
        // a second view of a.csv sits right after the first, and is current
        let a2 = t.duplicate().unwrap();
        assert_eq!(ids(&t), vec![(a, "a.csv"), (a2, "a.csv"), (b, "b.csv")]);
        assert_eq!(t.current, Some(a2));
        assert_eq!(t.label(a), "a.csv  (1)");
        assert_eq!(t.label(a2), "a.csv  (2)");
        assert_eq!(t.label(b), "b.csv");
        // opening a.csv again stays on the tab that is showing it
        assert_eq!(t.open_file("a.csv"), a2);
    }

    #[test]
    fn closing_and_cycling() {
        let mut t = Tabs::default();
        let a = t.open_file("a.csv");
        let b = t.open_file("b.csv");
        let c = t.open_file("c.csv");
        assert_eq!(t.cycle(true), Some(a));
        assert_eq!(t.cycle(false), Some(b));
        // closing a tab that is not on screen leaves the current one alone
        assert_eq!(t.close(a), None);
        assert_eq!(t.current, Some(c));
        // closing the one on screen moves to a neighbour
        assert_eq!(t.close(c), Some(b));
        // the last tab does not close
        assert_eq!(t.close(b), None);
        assert_eq!(t.list.len(), 1);
    }

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn one_open_file_is_found_among_many_by_part_of_its_name() {
        let all: Vec<String> = (0..100).map(|i| format!("export_part-{i:03}.jsonl")).collect();
        // nothing typed: every one, in tab order
        assert_eq!(find_tabs(&all, "").len(), 100);
        // part of a name, any letter case: only those, with their place among the tabs
        let hit = find_tabs(&all, "PART-07");
        assert_eq!(hit.len(), 10);
        assert_eq!(hit[0], (70, &all[70]));
        assert!(find_tabs(&all, "nothing-like-this").is_empty());
    }

    #[test]
    fn tabs_are_named_by_what_tells_them_apart() {
        // files unpacked from one archive share a long beginning: it is left out
        let long = names(&[
            "logs_2026-10-21_to_2026-11-20.zip · logs_2026-10-21_to_2026-11-20_admin-activity.jsonl",
            "logs_2026-10-21_to_2026-11-20.zip · logs_2026-10-21_to_2026-11-20_policy-denied.jsonl",
            "logs_2026-10-21_to_2026-11-20.zip · logs_2026-10-21_to_2026-11-20_summary.csv",
        ]);
        assert_eq!(
            short_labels(&long),
            names(&["admin-activity.jsonl", "policy-denied.jsonl", "summary.csv"])
        );
        // the shared part ends at a mark between words, never inside a word
        assert_eq!(
            short_labels(&names(&["server-auth.log", "server-audit.log"])),
            names(&["auth.log", "audit.log"])
        );
        // nothing shared, or one tab alone: the names as they are
        let apart = names(&["shop.csv", "devices.jsonl"]);
        assert_eq!(short_labels(&apart), apart);
        let one = names(&["logs_2026-10-21_admin-activity.jsonl"]);
        assert_eq!(short_labels(&one), one);
        // one name is the whole beginning of another: nothing is left out,
        // since that would leave a tab with no name
        let inside = names(&["report", "report-final"]);
        assert_eq!(short_labels(&inside), inside);
    }
}
