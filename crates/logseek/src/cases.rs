//! Cases: a named set of files that belong to one investigation, so any of
//! them is one click away. Files are only listed here; they are never
//! copied, moved or changed.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Case {
    pub name: String,
    /// Full paths, in the order shown (sorted by path).
    pub files: Vec<String>,
}

/// File kinds LogSeek reads.
pub const KINDS: [&str; 8] = ["csv", "tsv", "psv", "txt", "json", "jsonl", "ndjson", "log"];

fn ext_of(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Whether LogSeek can read this kind of file.
pub fn readable(p: &Path) -> bool {
    KINDS.contains(&ext_of(p).as_str()) || crate::book::handles(p)
}

/// A file with no extension that starts like a text log: no zero bytes in
/// its first 4 KB, at least one whole line, and almost all of it printable.
pub fn looks_like_text(p: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(p) else {
        return false;
    };
    let mut buf = [0u8; 4096];
    let n = f.read(&mut buf).unwrap_or(0);
    let head = &buf[..n];
    // 10 = line feed, 13 = carriage return, 9 = tab
    if n == 0 || head.contains(&0) || !head.contains(&10) {
        return false;
    }
    let odd = head
        .iter()
        .filter(|&&b| b < 0x20 && !matches!(b, 10 | 13 | 9))
        .count();
    odd * 100 <= n
}

/// Every readable file under `dir` (all levels), and how many files of each
/// other kind were left out.
pub fn scan_folder(dir: &Path) -> (Vec<PathBuf>, BTreeMap<String, usize>) {
    let mut found = Vec::new();
    let mut skipped: BTreeMap<String, usize> = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(p),
                Ok(t) if t.is_file() => {
                    if readable(&p) || (ext_of(&p).is_empty() && looks_like_text(&p)) {
                        found.push(p);
                    } else {
                        let ext = ext_of(&p);
                        *skipped
                            .entry(if ext.is_empty() {
                                "no extension".into()
                            } else {
                                format!(".{ext}")
                            })
                            .or_default() += 1;
                    }
                }
                _ => {}
            }
        }
    }
    found.sort();
    (found, skipped)
}

impl Case {
    /// Add files (no doubles); returns how many were new.
    pub fn add(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> usize {
        let before = self.files.len();
        for p in paths {
            let s = p.to_string_lossy().into_owned();
            if !self.files.contains(&s) {
                self.files.push(s);
            }
        }
        self.files.sort_by_key(|f| f.to_lowercase());
        self.files.len() - before
    }

    /// The files whose path contains every word of `needle` (any case).
    pub fn find(&self, needle: &str) -> Vec<usize> {
        let words: Vec<String> = needle.split_whitespace().map(str::to_lowercase).collect();
        (0..self.files.len())
            .filter(|&i| {
                let f = self.files[i].to_lowercase();
                words.iter().all(|w| f.contains(w))
            })
            .collect()
    }
}

/// "11,653 files LogSeek cannot read yet were left out: 9,707 .yml, 829
/// .evtx, 509 no extension and 57 more kinds"; every kind when `all`.
pub fn skipped_line(skipped: &BTreeMap<String, usize>, all: bool) -> Option<String> {
    let total: usize = skipped.values().sum();
    if total == 0 {
        return None;
    }
    let n = |x: usize| crate::moment::group(x as u64);
    let mut kinds: Vec<(&String, &usize)> = skipped.iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let shown = if all { kinds.len() } else { kinds.len().min(3) };
    let list: Vec<String> = kinds[..shown]
        .iter()
        .map(|(k, c)| format!("{} {k}", n(**c)))
        .collect();
    let rest = kinds.len() - shown;
    let tail = if rest > 0 {
        format!(
            " and {rest} more {}",
            if rest == 1 { "kind" } else { "kinds" }
        )
    } else {
        String::new()
    };
    Some(format!(
        "{} {} LogSeek cannot read yet {} left out: {}{tail}",
        n(total),
        if total == 1 { "file" } else { "files" },
        if total == 1 { "was" } else { "were" },
        list.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_gives_its_readable_files_at_every_level_and_counts_the_rest() {
        let d = tempfile::tempdir().unwrap();
        let mk = |rel: &str| {
            let p = d.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"x").unwrap();
        };
        for f in [
            "a.csv",
            "sub/b.JSON",
            "sub/deep/c.log",
            "sub/x.etl",
            "y.etl",
            "z.pcap",
            "README",
        ] {
            mk(f);
        }
        let (found, skipped) = scan_folder(d.path());
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.csv", "b.JSON", "c.log"]);
        assert_eq!(skipped.get(".etl"), Some(&2));
        assert_eq!(skipped.get(".pcap"), Some(&1));
        assert_eq!(skipped.get("no extension"), Some(&1));
        assert_eq!(
            skipped_line(&skipped, false).as_deref(),
            Some("4 files LogSeek cannot read yet were left out: 2 .etl, 1 .pcap, 1 no extension")
        );
        assert_eq!(skipped_line(&BTreeMap::new(), false), None);
        // many kinds: the three biggest, the rest counted; all of them on request
        let mut many = BTreeMap::new();
        for (k, c) in [
            (".yml", 9707),
            (".evtx", 829),
            (".md", 120),
            (".py", 76),
            (".zip", 20),
        ] {
            many.insert(k.to_string(), c);
        }
        assert_eq!(
            skipped_line(&many, false).as_deref(),
            Some("10,752 files LogSeek cannot read yet were left out: 9,707 .yml, 829 .evtx, 120 .md and 2 more kinds")
        );
        assert!(skipped_line(&many, true)
            .unwrap()
            .ends_with("76 .py, 20 .zip"));
    }

    #[test]
    fn files_with_no_extension_are_listed_when_they_read_as_text() {
        let d = tempfile::tempdir().unwrap();
        let lf = [10u8];
        let log = [
            b"2026-11-12 INFO started".as_slice(),
            &lf,
            b"2026-11-12 INFO ok",
            &lf,
        ]
        .concat();
        std::fs::write(d.path().join("waagent"), log).unwrap();
        std::fs::write(d.path().join("blob"), [0u8, 159, 146, 150, 10, 0, 1, 2]).unwrap();
        std::fs::write(d.path().join("oneword"), b"lock").unwrap();
        std::fs::write(d.path().join("empty"), b"").unwrap();
        let (found, skipped) = scan_folder(d.path());
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["waagent"]);
        assert_eq!(skipped.get("no extension"), Some(&3));
    }

    #[test]
    fn adding_twice_adds_once_and_find_matches_every_word() {
        let mut c = Case::default();
        let files = vec![
            PathBuf::from(r"C:\ev\analysis\falcon_timeline_host.csv"),
            PathBuf::from(r"C:\ev\analysis\azure_ops_all.csv"),
        ];
        assert_eq!(c.add(files.clone()), 2);
        assert_eq!(c.add(files), 0);
        assert_eq!(c.files.len(), 2);
        assert_eq!(c.find("FALCON host"), vec![1]);
        assert_eq!(c.find("analysis").len(), 2);
        assert!(c.find("nothing").is_empty());
    }
}
