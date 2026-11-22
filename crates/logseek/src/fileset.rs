//! Several files opened as one view. The set is written as a small list in
//! its own index folder (the app's data, never next to the evidence); the
//! list's path stands for the set wherever the app names an open file (tabs,
//! recent files, kept views, tags), so a set reopens like any file.

use std::path::{Path, PathBuf};

/// The extension of a set's file list.
pub const EXT: &str = "lsset";

/// A set's list file, as opposed to a log file.
pub fn is_set(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(EXT))
}

/// The files of a set in the order they are read: by path, so the same files
/// picked in any order are the same set (one index).
pub fn ordered(files: &[PathBuf]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = files
        .iter()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
        .collect();
    v.sort();
    v.dedup();
    v
}

/// The index folder name for a set of files: how many, the first one's
/// name, and a fingerprint of every path.
pub fn folder_name(files: &[PathBuf]) -> String {
    let files = ordered(files);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in &files {
        for b in p.to_string_lossy().bytes().chain(std::iter::once(0)) {
            h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    let stem: String = files
        .first()
        .and_then(|p| p.file_stem())
        .map(|s| {
            s.to_string_lossy()
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                .take(32)
                .collect()
        })
        .unwrap_or_default();
    format!("{}-files-{stem}-{:08x}.lsidx", files.len(), h as u32)
}

/// Write the set's list into `dir` (its index folder) and give its path.
pub fn write(dir: &Path, files: &[PathBuf]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let list: Vec<String> = ordered(files)
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let path = dir.join(format!("files.{EXT}"));
    std::fs::write(&path, list.join("\n"))?;
    Ok(path)
}

/// A path as its parts, for comparing by text alone: nothing is opened or
/// looked up, so a network path is never contacted. Drive letters and names
/// compare without case. `None` for a path that is not whole (relative, or
/// with `..` in it).
fn parts(p: &Path) -> Option<Vec<String>> {
    use std::path::{Component, Prefix};
    let mut out = Vec::new();
    let mut rooted = false;
    for c in p.components() {
        match c {
            Component::Prefix(pre) => out.push(match pre.kind() {
                Prefix::Disk(d) | Prefix::VerbatimDisk(d) => {
                    format!("{}:", (d as char).to_ascii_lowercase())
                }
                Prefix::UNC(s, sh) | Prefix::VerbatimUNC(s, sh) => format!(
                    "\\\\{}\\{}",
                    s.to_string_lossy().to_lowercase(),
                    sh.to_string_lossy().to_lowercase()
                ),
                other => format!("{other:?}").to_lowercase(),
            }),
            Component::RootDir => rooted = true,
            Component::CurDir => {}
            Component::ParentDir => return None,
            Component::Normal(n) => out.push(n.to_string_lossy().to_lowercase()),
        }
    }
    rooted.then_some(out)
}

/// Two folder paths name the same folder, by their text alone.
pub fn same_folder(a: &Path, b: &Path) -> bool {
    match (parts(a), parts(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// The files a list from outside the app may not name: every one that is not
/// inside the list's own folder. A list the app wrote sits in the app's data
/// and names files anywhere; a list that came with other files (in a folder
/// of evidence, in an archive) may only gather the files beside it, so it
/// cannot make the app read a file elsewhere on the machine or on a network.
pub fn outside(list: &Path, files: &[PathBuf]) -> Vec<PathBuf> {
    let base = list.parent().and_then(parts);
    files
        .iter()
        .filter(|f| match (&base, parts(f)) {
            (Some(b), Some(p)) => p.len() <= b.len() || p[..b.len()] != b[..],
            _ => true,
        })
        .cloned()
        .collect()
}

/// The files of a set, from its list.
pub fn read(path: &Path) -> Option<Vec<PathBuf>> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Vec<PathBuf> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect();
    (!v.is_empty()).then_some(v)
}

/// How a set is named on screen: how many files.
pub fn title(files: &[PathBuf]) -> String {
    format!("{} files", files.len())
}

/// An open file's name in lists (recent files, tabs): a log file's name, or
/// for a set how many files and every one of them.
pub fn display_name(path: &Path) -> String {
    let name = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    match is_set(path).then(|| read(path)).flatten() {
        Some(files) => format!(
            "{}: {}",
            title(&files),
            logseek_core::ingest::pipeline::file_labels(&files).join(", ")
        ),
        None => name(path),
    }
}

/// A short name, without reading anything (tabs, drawn every frame): a log
/// file's name, or "N files" for a set (its folder's name starts with N).
pub fn short_name(path: &Path) -> String {
    if is_set(path) {
        let n = path
            .parent()
            .and_then(|d| d.file_name())
            .and_then(|f| f.to_string_lossy().split('-').next().map(str::to_string))
            .and_then(|k| k.parse::<usize>().ok());
        if let Some(n) = n {
            return format!("{n} files");
        }
    }
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The bytes of an open file, or of every file of a set.
pub fn bytes(path: &Path) -> u64 {
    let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    match is_set(path).then(|| read(path)).flatten() {
        Some(files) => files.iter().map(|f| size(f)).sum(),
        None => size(path),
    }
}

#[cfg(test)]
mod outside_tests {
    use super::*;

    #[test]
    fn a_list_from_outside_may_only_name_files_in_its_own_folder() {
        let list = Path::new(r"D:\case\logs\files.lsset");
        let ok = [
            PathBuf::from(r"D:\case\logs\a.csv"),
            PathBuf::from(r"\\?\D:\Case\Logs\sub\b.csv"),
        ];
        assert!(outside(list, &ok).is_empty());
        let bad = [
            PathBuf::from(r"C:\Windows\win.ini"),
            PathBuf::from(r"\\server\share\x.csv"),
            PathBuf::from(r"D:\case\logs\..\secret.csv"),
            PathBuf::from(r"D:\case\logs2\c.csv"),
            PathBuf::from(r"relative.csv"),
            PathBuf::from(r"D:\case\logs"),
        ];
        assert_eq!(outside(list, &bad), bad.to_vec());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_is_the_same_whatever_order_its_files_were_picked_in() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("access.log");
        let b = d.path().join("access.log.1");
        std::fs::write(&a, "x\n").unwrap();
        std::fs::write(&b, "y\n").unwrap();
        let one = folder_name(&[a.clone(), b.clone()]);
        let two = folder_name(&[b.clone(), a.clone(), a.clone()]);
        assert_eq!(one, two);
        assert!(one.starts_with("2-files-access-"), "{one}");
        // another set, another index
        assert_ne!(one, folder_name(&[a.clone()]));
        // the list round-trips and stands for the set
        let list = write(&d.path().join(&one), &[b.clone(), a.clone()]).unwrap();
        assert!(is_set(&list));
        assert!(!is_set(&a));
        assert_eq!(read(&list).unwrap(), ordered(&[a.clone(), b]));
        assert_eq!(title(&read(&list).unwrap()), "2 files");
        // names on screen: short in tabs, every file in lists
        assert_eq!(short_name(&list), "2 files");
        assert_eq!(short_name(&a), "access.log");
        assert_eq!(display_name(&list), "2 files: access.log, access.log.1");
        assert_eq!(bytes(&list), 4);
    }
}
