//! Packed files (.gz, .zip). A packed file cannot be read at any place
//! without unpacking what comes before, so the file inside is unpacked once
//! to a working copy that the usual import reads. The packed file itself is
//! only ever read.

use crate::sysmem::{disk_free, DISK_SAFETY};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Removes a working copy (`.part`) when it goes out of scope, whichever way
/// the function ends: an error, an early return, or a panic unwinding through
/// it. `disarm` is called once the copy has been renamed into place.
pub(crate) struct PartGuard(Option<PathBuf>);

impl PartGuard {
    pub(crate) fn new(p: &Path) -> Self {
        PartGuard(Some(p.to_path_buf()))
    }
    pub(crate) fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for PartGuard {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// How much is written between two looks at the free space of the drive.
const ROOM_CHECK_EVERY: u64 = 4 << 20;

/// Keeps a conversion from filling the drive it writes to: the free space is
/// read every few MiB written, and the work stops with a plain message before
/// the drive drops under the margin kept for the system and other programs.
pub(crate) struct Room {
    near: PathBuf,
    verb: &'static str,
    since_check: u64,
}

impl Room {
    pub(crate) fn new(near: &Path, verb: &'static str) -> Room {
        Room {
            near: near.to_path_buf(),
            verb,
            since_check: 0,
        }
    }

    fn mib(b: u64) -> u64 {
        b >> 20
    }

    fn short(&self, free: u64) -> String {
        format!(
            "not enough room to {}: {} MiB are free on the drive holding {}, and {} MiB are always kept free for the system and other programs",
            self.verb,
            Self::mib(free),
            self.near.display(),
            Self::mib(DISK_SAFETY)
        )
    }

    /// `n` bytes have been written (or are about to be).
    pub(crate) fn wrote(&mut self, n: u64) -> Result<(), String> {
        self.since_check = self.since_check.saturating_add(n);
        if self.since_check < ROOM_CHECK_EVERY {
            return Ok(());
        }
        self.since_check = 0;
        match disk_free(&self.near) {
            Some(free) if free < DISK_SAFETY => Err(self.short(free)),
            _ => Ok(()),
        }
    }

    /// Refuse, before writing, `bytes` that the drive could not take while
    /// keeping its margin.
    pub(crate) fn need(&self, bytes: u64) -> Result<(), String> {
        match disk_free(&self.near) {
            Some(free) if free < bytes.saturating_add(DISK_SAFETY) => Err(format!(
                "{} (this step alone needs about {} MiB)",
                self.short(free),
                Self::mib(bytes)
            )),
            _ => Ok(()),
        }
    }
}

fn ext_of(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

pub fn is_packed(p: &Path) -> bool {
    matches!(ext_of(p).as_str(), "gz" | "zip")
}

/// A file inside a packed file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// Its size once unpacked, when the packed file says.
    pub bytes: Option<u64>,
}

/// The files inside, in their own order (folders are left out).
pub fn entries(path: &Path) -> Result<Vec<Entry>, String> {
    if ext_of(path) == "gz" {
        // one file: the packed file's name without ".gz"
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unpacked".into());
        // check it really is gzip before promising anything
        let mut head = [0u8; 2];
        std::fs::File::open(path)
            .and_then(|mut f| f.read_exact(&mut head))
            .map_err(|e| e.to_string())?;
        if head != [0x1f, 0x8b] {
            return Err("this is not a gzip file".into());
        }
        return Ok(vec![Entry { name, bytes: None }]);
    }
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for i in 0..z.len() {
        let e = z.by_index(i).map_err(|e| e.to_string())?;
        if e.is_dir() {
            continue;
        }
        out.push(Entry {
            name: e.name().map_err(|e| e.to_string())?.into_owned(),
            bytes: Some(e.size()),
        });
    }
    Ok(out)
}

/// Unpack `entry` of `path` to `out`. `done` counts the bytes written so
/// far; `cancel` stops it. An unfinished copy is removed.
pub fn unpack(
    path: &Path,
    entry: &str,
    out: &Path,
    cancel: &AtomicBool,
    done: &AtomicU64,
) -> Result<u64, String> {
    let tmp = out.with_extension("part");
    // gone on every way out, a panic included; kept only once renamed into place
    let mut guard = PartGuard::new(&tmp);
    let n = (|| -> Result<u64, String> {
        let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let w = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        let mut w = BufWriter::with_capacity(1 << 20, w);
        let mut room = Room::new(&tmp, "unpack");
        if ext_of(path) == "gz" {
            // MultiGzDecoder: a file made of several gzip parts is read to its end
            let mut r = flate2::read::MultiGzDecoder::new(std::io::BufReader::new(f));
            copy(&mut r, &mut w, cancel, done, &mut room)
        } else {
            let mut z = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
            let mut r = z.by_name(entry).map_err(|e| e.to_string())?;
            copy(&mut r, &mut w, cancel, done, &mut room)
        }
    })()?;
    std::fs::rename(&tmp, out).map_err(|e| e.to_string())?;
    guard.disarm();
    Ok(n)
}

fn copy(
    r: &mut impl Read,
    w: &mut impl Write,
    cancel: &AtomicBool,
    done: &AtomicU64,
    room: &mut Room,
) -> Result<u64, String> {
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("stopped".into());
        }
        let n = r.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        w.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        room.wrote(n as u64)?;
        total += n as u64;
        done.store(total, Ordering::Relaxed);
    }
    w.flush().map_err(|e| e.to_string())?;
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_working_copy_goes_when_a_panic_unwinds_and_stays_once_disarmed() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.part");
        std::fs::write(&p, b"x").unwrap();
        let r = std::panic::catch_unwind(|| {
            let _g = PartGuard::new(&p);
            panic!("a parser gave up");
        });
        assert!(r.is_err());
        assert!(!p.exists());
        // disarmed: it is the finished file now
        std::fs::write(&p, b"x").unwrap();
        let mut g = PartGuard::new(&p);
        g.disarm();
        drop(g);
        assert!(p.exists());
    }
}
