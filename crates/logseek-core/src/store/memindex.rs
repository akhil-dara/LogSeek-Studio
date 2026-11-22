//! Indexes held in memory (zero-write tier): when no disk has room, segment
//! bytes and the manifest live here, inside the RAM budget, for the session.
//! `Store::open` looks here first, so the rest of the engine cannot tell.

use super::Manifest;
use crate::budget::Reservation;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Name prefix of a segment held in memory.
pub const MEM_PREFIX: &str = "mem-";

pub struct MemIndex {
    manifest: Mutex<Manifest>,
    segs: Mutex<HashMap<String, (Arc<[u8]>, Option<Reservation>)>>,
}

impl MemIndex {
    pub fn new(m: Manifest) -> Arc<MemIndex> {
        Arc::new(MemIndex {
            manifest: Mutex::new(m),
            segs: Mutex::new(HashMap::new()),
        })
    }
    pub fn manifest(&self) -> Manifest {
        self.manifest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn set_manifest(&self, m: &Manifest) {
        *self.manifest.lock().unwrap_or_else(|e| e.into_inner()) = m.clone();
    }
    /// Keep a segment's bytes; `res` holds their share of the RAM budget.
    pub fn add(&self, name: String, bytes: Vec<u8>, res: Option<Reservation>) {
        self.segs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name, (Arc::from(bytes), res));
    }
    pub fn segment(&self, name: &str) -> Option<Arc<[u8]>> {
        self.segs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .map(|s| s.0.clone())
    }
    /// Bytes of all segments held.
    pub fn bytes(&self) -> u64 {
        self.segs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|s| s.0.len() as u64)
            .sum()
    }
}

fn reg() -> &'static Mutex<HashMap<PathBuf, Arc<MemIndex>>> {
    static R: OnceLock<Mutex<HashMap<PathBuf, Arc<MemIndex>>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn get(dir: &Path) -> Option<Arc<MemIndex>> {
    reg()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(dir)
        .cloned()
}
pub fn put(dir: &Path, m: Arc<MemIndex>) {
    reg()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(dir.to_path_buf(), m);
}
/// Forget an index held in memory (its RAM goes back to the budget once no
/// open store still reads it).
pub fn remove(dir: &Path) {
    reg().lock().unwrap_or_else(|e| e.into_inner()).remove(dir);
}
