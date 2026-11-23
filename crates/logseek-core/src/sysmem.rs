//! Process memory as the OS sees it (for the budget meter and benchmarks).

#[derive(Debug, Clone, Copy, Default)]
pub struct ProcMem {
    /// Private committed bytes (Windows PrivateUsage / Linux RssAnon+swap approx).
    pub private: u64,
    /// Current working set / RSS.
    pub working_set: u64,
    /// Peak working set / RSS high-water mark.
    pub peak_working_set: u64,
}

#[cfg(windows)]
pub fn process_memory() -> ProcMem {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: plain-data struct, size passed explicitly; pseudo-handle needs no close.
    unsafe {
        let mut c: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
        c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        if GetProcessMemoryInfo(GetCurrentProcess(), &mut c as *mut _ as *mut _, c.cb) != 0 {
            return ProcMem {
                private: c.PrivateUsage as u64,
                working_set: c.WorkingSetSize as u64,
                peak_working_set: c.PeakWorkingSetSize as u64,
            };
        }
    }
    ProcMem::default()
}

#[cfg(not(windows))]
pub fn process_memory() -> ProcMem {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb = |key: &str| {
        s.lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            * 1024
    };
    ProcMem {
        private: kb("RssAnon:"),
        working_set: kb("VmRSS:"),
        peak_working_set: kb("VmHWM:"),
    }
}

/// Disk kept free for the system and other programs: the index never eats into it.
pub const DISK_SAFETY: u64 = 200 << 20;

/// Bytes free to this user on the drive holding `path` (measured at the
/// nearest folder that exists). In a debug build, or one made with the
/// `devtools` feature, `LOGSEEK_FAKE_FREE_MB` stands in for the real figure
/// (tests of a nearly full disk); a release build always measures.
pub fn disk_free(path: &std::path::Path) -> Option<u64> {
    if cfg!(any(debug_assertions, feature = "devtools")) {
        if let Some(mb) = std::env::var("LOGSEEK_FAKE_FREE_MB")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
        {
            return Some(mb << 20);
        }
    }
    let mut p = path;
    while !p.exists() {
        p = p.parent()?;
    }
    disk_free_os(p)
}

#[cfg(windows)]
fn disk_free_os(p: &std::path::Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = p
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut avail: u64 = 0;
    // SAFETY: NUL-terminated path; the two totals we do not need may be null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut avail,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(avail)
}

#[cfg(not(windows))]
fn disk_free_os(p: &std::path::Path) -> Option<u64> {
    // `df -Pk <path>`: the 4th field of the 2nd line is the free space in KiB
    let out = std::process::Command::new("df")
        .arg("-Pk")
        .arg(p)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let kib: u64 = text
        .lines()
        .nth(1)?
        .split_whitespace()
        .nth(3)?
        .parse()
        .ok()?;
    Some(kib * 1024)
}

/// The disk budget for an index at `path`: `min(user cap, free − DISK_SAFETY)`.
pub fn disk_budget(path: &std::path::Path, user_cap: Option<u64>) -> u64 {
    let room = disk_free(path)
        .unwrap_or(u64::MAX)
        .saturating_sub(DISK_SAFETY);
    user_cap.map_or(room, |c| c.min(room))
}
