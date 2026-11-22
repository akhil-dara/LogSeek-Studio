//! The budget governor: one hard byte cap shared by every component.
//!
//! Large buffers (ingest batches, decode cache, aggregation tables, sort runs,
//! UI page cache) are *reserved* before they are allocated. A refused
//! reservation means "degrade": smaller batch, spill to disk, evict, estimate.
//! The governor never lets the sum of live reservations exceed the cap.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const MIB: u64 = 1024 * 1024;
/// Floor for the automatic budget, and the ceiling used by [`auto_cap`]. The
/// default for the whole app is [`DEFAULT_CAP`].
pub const AUTO_MIN: u64 = 96 * MIB;
pub const AUTO_MAX: u64 = 512 * MIB;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Component {
    Ui,
    Cache,
    Ingest,
    Query,
    Sketch,
    Other,
}

impl Component {
    pub const ALL: [Component; 6] = [
        Component::Ui,
        Component::Cache,
        Component::Ingest,
        Component::Query,
        Component::Sketch,
        Component::Other,
    ];
    fn idx(self) -> usize {
        self as usize
    }
    pub fn label(self) -> &'static str {
        match self {
            Component::Ui => "Interface",
            Component::Cache => "Page cache",
            Component::Ingest => "Import",
            Component::Query => "Queries",
            Component::Sketch => "Profiles",
            Component::Other => "Other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetExceeded {
    pub requested: u64,
    pub available: u64,
}

impl fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "memory budget: requested {} B, {} B available",
            self.requested, self.available
        )
    }
}
impl std::error::Error for BudgetExceeded {}

struct Inner {
    /// The limit in force now: between `base` and `ceiling`.
    cap: AtomicU64,
    base: u64,
    /// Follows what the machine has free (see [`Budget::set_ceiling`]).
    ceiling: AtomicU64,
    /// A pinned budget never moves.
    fixed: bool,
    used: AtomicU64,
    peak: AtomicU64,
    by: [AtomicU64; 6],
}

/// Cheap to clone; all clones share one cap.
#[derive(Clone)]
pub struct Budget {
    inner: Arc<Inner>,
}

impl fmt::Debug for Budget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Budget")
            .field("cap", &self.cap())
            .field("used", &self.used())
            .field("peak", &self.peak())
            .finish()
    }
}

impl Budget {
    pub fn new(cap_bytes: u64) -> Budget {
        let b = Budget::elastic(cap_bytes, cap_bytes);
        Budget {
            inner: Arc::new(Inner {
                fixed: true,
                ..Arc::try_unwrap(b.inner).ok().expect("just made")
            }),
        }
    }

    /// A budget that starts at `base` and may be raised, never past
    /// `ceiling`, by whoever owns it ([`Budget::set_cap`]): small by default,
    /// bigger only while the work in hand needs it.
    pub fn elastic(base: u64, ceiling: u64) -> Budget {
        Budget {
            inner: Arc::new(Inner {
                cap: AtomicU64::new(base),
                base,
                ceiling: AtomicU64::new(ceiling.max(base)),
                fixed: false,
                used: AtomicU64::new(0),
                peak: AtomicU64::new(0),
                by: Default::default(),
            }),
        }
    }

    /// `clamp(70 % of currently available RAM, 96 MiB, default cap)`, where the
    /// default cap is [`DEFAULT_CAP`] (500 MiB) on any machine: the budget
    /// shrinks when the machine is tighter and never grows past that unless the
    /// user pins a larger value.
    pub fn auto() -> Budget {
        Budget::new(Machine::detect().budget_cap(None))
    }

    pub fn cap(&self) -> u64 {
        self.inner.cap.load(Ordering::Acquire)
    }
    /// The limit it starts from and returns to.
    pub fn base(&self) -> u64 {
        self.inner.base
    }
    /// The most it may be raised to now.
    pub fn ceiling(&self) -> u64 {
        self.inner.ceiling.load(Ordering::Acquire)
    }
    /// Move the limit (kept between base and ceiling); says what it is now.
    /// Lowering it frees nothing by itself: the owner gives memory back.
    pub fn set_cap(&self, bytes: u64) -> u64 {
        let n = bytes.clamp(self.inner.base, self.ceiling());
        self.inner.cap.store(n, Ordering::Release);
        n
    }
    /// Move how far it may grow: to what the machine has free now, measured
    /// again and again (no fixed most). Never under where it starts; a cap
    /// above the new ceiling comes down to it. A pinned budget does not move.
    pub fn set_ceiling(&self, bytes: u64) {
        if self.inner.fixed {
            return;
        }
        let c = bytes.max(self.inner.base);
        self.inner.ceiling.store(c, Ordering::Release);
        if self.cap() > c {
            self.inner.cap.store(c, Ordering::Release);
        }
    }
    pub fn used(&self) -> u64 {
        self.inner.used.load(Ordering::Acquire)
    }
    pub fn peak(&self) -> u64 {
        self.inner.peak.load(Ordering::Acquire)
    }
    pub fn available(&self) -> u64 {
        self.cap().saturating_sub(self.used())
    }

    pub fn usage_by(&self) -> Vec<(Component, u64)> {
        Component::ALL
            .iter()
            .map(|&c| (c, self.inner.by[c.idx()].load(Ordering::Acquire)))
            .collect()
    }

    pub fn try_reserve(&self, bytes: u64, who: Component) -> Result<Reservation, BudgetExceeded> {
        self.acquire(bytes, who)?;
        Ok(Reservation {
            budget: self.clone(),
            bytes,
            who,
        })
    }

    fn acquire(&self, bytes: u64, who: Component) -> Result<(), BudgetExceeded> {
        let inner = &self.inner;
        let mut cur = inner.used.load(Ordering::Acquire);
        loop {
            let cap = inner.cap.load(Ordering::Acquire);
            let next = cur.checked_add(bytes).filter(|&n| n <= cap);
            let Some(next) = next else {
                return Err(BudgetExceeded {
                    requested: bytes,
                    available: cap.saturating_sub(cur),
                });
            };
            match inner
                .used
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    inner.peak.fetch_max(next, Ordering::AcqRel);
                    inner.by[who.idx()].fetch_add(bytes, Ordering::AcqRel);
                    return Ok(());
                }
                Err(actual) => cur = actual,
            }
        }
    }

    fn release(&self, bytes: u64, who: Component) {
        self.inner.used.fetch_sub(bytes, Ordering::AcqRel);
        self.inner.by[who.idx()].fetch_sub(bytes, Ordering::AcqRel);
    }
}

/// A live claim on part of the budget; released on drop.
pub struct Reservation {
    budget: Budget,
    bytes: u64,
    who: Component,
}

impl Reservation {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn grow(&mut self, extra: u64) -> Result<(), BudgetExceeded> {
        self.budget.acquire(extra, self.who)?;
        self.bytes += extra;
        Ok(())
    }
    /// Move `n` of these bytes into a reservation of their own (nothing is
    /// released or acquired: the budget's total stays the same).
    pub fn split(&mut self, n: u64) -> Option<Reservation> {
        if n > self.bytes {
            return None;
        }
        self.bytes -= n;
        Some(Reservation {
            budget: self.budget.clone(),
            bytes: n,
            who: self.who,
        })
    }

    pub fn shrink(&mut self, to: u64) {
        if to < self.bytes {
            self.budget.release(self.bytes - to, self.who);
            self.bytes = to;
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.bytes > 0 {
            self.budget.release(self.bytes, self.who);
        }
    }
}

impl fmt::Debug for Reservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Reservation({:?}, {} B)", self.who, self.bytes)
    }
}

pub fn auto_cap(available_ram: u64) -> u64 {
    (available_ram / 10 * 7).clamp(AUTO_MIN, AUTO_MAX)
}

// ───────────────────────── adaptive plan ─────────────────────────

/// The most the whole app takes unless the user pins another value: 500 MiB,
/// on any machine. A bigger machine does not make the app bigger; it only
/// gets more memory when the user asks for it (LOGSEEK_BUDGET_MB).
pub const DEFAULT_CAP: u64 = 500 * MIB;

/// Default upper limit for the automatic budget (see [`DEFAULT_CAP`]).
pub fn default_user_cap(_total_ram: u64) -> u64 {
    DEFAULT_CAP
}

/// What the machine offers right now; measured at start and again whenever
/// conditions change (other apps freeing or taking RAM).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Machine {
    pub total_ram: u64,
    pub available_ram: u64,
    pub cores: usize,
}

impl Machine {
    pub fn detect() -> Machine {
        let total_ram = total_ram_bytes();
        Machine {
            total_ram,
            available_ram: available_ram_bytes().min(total_ram.max(1)),
            cores: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(2),
        }
    }

    /// How far the app may grow while the work in hand needs it: 70 % of the
    /// RAM that is free now (no fixed share of the machine: a machine with
    /// memory to spare makes the work faster), never less than where it starts.
    pub fn elastic_ceiling(&self) -> u64 {
        let start = self.budget_cap(None);
        (self.available_ram / 10 * 7).max(start)
    }

    /// `clamp(70 % of available RAM, 96 MiB, user cap or default cap)`.
    pub fn budget_cap(&self, user_cap: Option<u64>) -> u64 {
        let hi = user_cap
            .unwrap_or_else(|| default_user_cap(self.total_ram))
            .max(AUTO_MIN);
        (self.available_ram / 10 * 7).clamp(AUTO_MIN, hi)
    }
}

/// The one word the status bar shows for how much headroom the engine has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mode {
    Lean,
    Standard,
    Turbo,
}

impl Mode {
    pub fn pick(cap: u64, cores: usize) -> Mode {
        if cap < 1024 * MIB {
            Mode::Lean
        } else if cap >= 4096 * MIB && cores >= 8 {
            Mode::Turbo
        } else {
            Mode::Standard
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Mode::Lean => "Lean",
            Mode::Standard => "Standard",
            Mode::Turbo => "Turbo",
        }
    }
}

/// How the engine spends a budget. Features never disappear at the floor;
/// they run slower there (more passes, smaller caches) and say so. More
/// resources only add speed and switch extras on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub mode: Mode,
    pub cap: u64,
    /// Parser / scan threads.
    pub workers: usize,
    /// Decoded-chunk cache.
    pub cache_bytes: u64,
    /// Import working set (chunks in flight).
    pub ingest_bytes: u64,
    /// Trigram Bloom filter size per block of source.
    pub bloom_bytes_per_block: u64,
    /// Group-by keys held in RAM before multi-pass partitioning kicks in.
    pub single_pass_groups: u64,
    /// Turbo: per-column token index for instant free-text search.
    pub token_index: bool,
    /// Standard and up: 1-second time rollups precomputed for the time spine.
    pub second_rollups: bool,
    /// Turbo: formula / extract previews over all rows instead of a sample.
    pub full_previews: bool,
}

impl Plan {
    pub fn for_machine(m: &Machine, user_cap: Option<u64>) -> Plan {
        Plan::for_resources(m.budget_cap(user_cap), m.cores)
    }

    pub fn for_resources(cap: u64, cores: usize) -> Plan {
        let mode = Mode::pick(cap, cores);
        let cores = cores.max(1);
        let (workers, cache, ingest, bloom) = match mode {
            // threads cost no budget (the governor throttles chunks in flight),
            // so even Lean uses up to 8 cores: measured 206 MB/s at 512 MB
            Mode::Lean => (cores.min(8), cap / 5 * 2, cap / 5 * 2, 8 * 1024),
            Mode::Standard => (cores.clamp(2, 8), cap / 2, cap / 10 * 3, 32 * 1024),
            Mode::Turbo => (cores.clamp(8, 32), cap / 2, cap / 10 * 3, 64 * 1024),
        };
        Plan {
            mode,
            cap,
            workers,
            cache_bytes: cache,
            ingest_bytes: ingest,
            bloom_bytes_per_block: bloom,
            // ~64 bytes per key in the compact arena (agg::ResultTable)
            single_pass_groups: cache / 64,
            token_index: mode == Mode::Turbo,
            second_rollups: mode >= Mode::Standard,
            full_previews: mode == Mode::Turbo,
        }
    }

    /// The status-bar tooltip: what more resources would unlock, in plain words.
    pub fn unlock_hint(&self) -> Option<&'static str> {
        match self.mode {
            Mode::Lean => Some(
                "Running lean. With 1 GB or more of free RAM, repeat queries get faster, big group-bys finish in one pass and the timeline gets 1-second detail.",
            ),
            Mode::Standard => Some(
                "With 4 GB of RAM for LogSeek and 8 or more cores, Turbo adds instant free-text search and previews over every row.",
            ),
            Mode::Turbo => None,
        }
    }
}

/// Total physical RAM in bytes (0 if unknown, which clamps to the floor).
pub fn total_ram_bytes() -> u64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        // SAFETY: MEMORYSTATUSEX is plain data; dwLength must be set before the call.
        unsafe {
            let mut st: MEMORYSTATUSEX = std::mem::zeroed();
            st.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
            if GlobalMemoryStatusEx(&mut st) != 0 {
                return st.ullTotalPhys;
            }
        }
        0
    }
    #[cfg(not(windows))]
    {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("MemTotal:"))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .and_then(|kb| kb.parse::<u64>().ok())
            })
            .map(|kb| kb * 1024)
            .unwrap_or(0)
    }
}

/// Physical RAM currently available to new allocations (0 if unknown).
pub fn available_ram_bytes() -> u64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        // SAFETY: MEMORYSTATUSEX is plain data; dwLength must be set before the call.
        unsafe {
            let mut st: MEMORYSTATUSEX = std::mem::zeroed();
            st.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
            if GlobalMemoryStatusEx(&mut st) != 0 {
                return st.ullAvailPhys;
            }
        }
        0
    }
    #[cfg(not(windows))]
    {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("MemAvailable:"))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .and_then(|kb| kb.parse::<u64>().ok())
            })
            .map(|kb| kb * 1024)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn auto_cap_uses_512_mb_when_plenty_is_available() {
        assert_eq!(auto_cap(4096 * MIB), 512 * MIB);
        assert_eq!(auto_cap(760 * MIB), 512 * MIB);
    }

    #[test]
    fn auto_cap_takes_70_percent_of_available_when_tight() {
        assert_eq!(auto_cap(300 * MIB), 210 * MIB);
    }

    #[test]
    fn auto_cap_never_below_floor() {
        assert_eq!(auto_cap(50 * MIB), AUTO_MIN);
        assert_eq!(auto_cap(0), AUTO_MIN);
    }
}
