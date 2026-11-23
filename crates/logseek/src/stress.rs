//! Stress harness: `LOGSEEK_STRESS=<report.json>` drives the app through a
//! fixed script (scroll, jump, type, filter, summarise, calendar, colours…)
//! and records every frame: how long the UI took to build it, how long since
//! the previous frame (the pacing a person sees), memory, and how long each
//! query took to answer. The report is JSON; the app closes when done.

use logseek_core::sysmem::process_memory;
use serde::Serialize;
use std::time::Instant;

#[derive(Serialize, Default, Clone)]
pub struct PhaseReport {
    pub name: String,
    pub frames: usize,
    pub ui_ms_p50: f64,
    pub ui_ms_p95: f64,
    pub ui_ms_max: f64,
    /// Whole-frame CPU time (UI + drawing, without waiting for the display).
    pub cpu_ms_p95: f64,
    pub cpu_ms_max: f64,
    pub frame_ms_p50: f64,
    pub frame_ms_p95: f64,
    pub frame_ms_max: f64,
    /// Frames more than 33 ms after the previous one (a visible hitch).
    pub stutters: usize,
    /// Frames more than 100 ms after the previous one (a freeze).
    pub freezes: usize,
    /// Time for the engine to answer, where the phase waits for an answer.
    pub answer_ms: Option<f64>,
    pub peak_private_mb: u64,
    pub timed_out: bool,
    /// Frames whose view showed rows not loaded yet (target 0 while scrolling).
    pub rows_waited: usize,
    /// Which frames of the phase waited (first 64): at the start, or throughout.
    pub waited_frames: Vec<u32>,
}

#[derive(Serialize, Default)]
pub struct Report {
    pub file: String,
    pub budget_mb: u64,
    pub rows: u64,
    pub import_s: f64,
    pub index_mb: f64,
    pub peak_private_mb: u64,
    pub peak_working_set_mb: u64,
    /// Memory after ten idle seconds at the end: what the app holds at rest.
    pub rest_private_mb: u64,
    /// With a second file (`LOGSEEK_STRESS_FILE2`): how long each switch
    /// from one file's tab to the other took until its rows were back, ms.
    pub switch_ms: Vec<f64>,
    pub phases: Vec<PhaseReport>,
}

pub struct Stress {
    path: String,
    pub phase: usize,
    pub frame_in_phase: u32,
    phase_started: Instant,
    started: Instant,
    frame_start: Option<Instant>,
    last_frame: Option<Instant>,
    ui_ms: Vec<f64>,
    frame_ms: Vec<f64>,
    cpu_ms: Vec<f64>,
    peak_phase: u64,
    peak_all: u64,
    answer_ms: Option<f64>,
    timed_out: bool,
    pub report: Report,
    /// When the phase began waiting for an answer.
    pub waiting_since: Option<Instant>,
    /// The phase just changed: the next frame is its frame 0.
    fresh: bool,
    waited: usize,
    waited_frames: Vec<u32>,
}

/// The scripted phases, in order.
pub const PHASES: [&str; 15] = [
    "import",
    "scroll",
    "jump",
    "type a search",
    "scroll results",
    "column filter",
    "values list",
    "summarise",
    "calendar",
    "colour rules",
    "add column sheet",
    "clear",
    "open a second file",
    "switch between files",
    "rest",
];

fn pct(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    s[((s.len() - 1) as f64 * p).round() as usize]
}

impl Stress {
    pub fn from_env() -> Option<Stress> {
        let path = crate::dev::var("LOGSEEK_STRESS").ok()?;
        let now = Instant::now();
        Some(Stress {
            path,
            phase: 0,
            frame_in_phase: 0,
            phase_started: now,
            started: now,
            frame_start: None,
            last_frame: None,
            ui_ms: Vec::new(),
            frame_ms: Vec::new(),
            cpu_ms: Vec::new(),
            peak_phase: 0,
            peak_all: 0,
            answer_ms: None,
            timed_out: false,
            report: Report::default(),
            waiting_since: None,
            fresh: false,
            waited: 0,
            waited_frames: Vec::new(),
        })
    }

    pub fn done(&self) -> bool {
        self.phase >= PHASES.len()
    }

    /// Call at the top of each frame.
    pub fn begin_frame(&mut self, last_cpu: Option<f32>) {
        let now = Instant::now();
        if let (Some(c), true) = (last_cpu, self.phase > 0) {
            self.cpu_ms.push(c as f64 * 1000.0);
        }
        if let Some(prev) = self.last_frame {
            // the import phase is a wait, not interaction: its pacing is not scored
            if self.phase > 0 {
                self.frame_ms
                    .push(now.duration_since(prev).as_secs_f64() * 1000.0);
            }
        }
        self.last_frame = Some(now);
        self.frame_start = Some(now);
        if self.frame_in_phase % 15 == 0 {
            let m = process_memory();
            self.peak_phase = self.peak_phase.max(m.private >> 20);
            self.peak_all = self.peak_all.max(m.private >> 20);
        }
    }

    /// Call at the end of each frame; `waited`: the grid showed rows not loaded.
    pub fn end_frame(&mut self, waited: bool) {
        if waited && self.phase > 0 {
            self.waited += 1;
            if self.waited_frames.len() < 64 {
                self.waited_frames.push(self.frame_in_phase);
            }
        }
        if let Some(s) = self.frame_start.take() {
            if self.phase > 0 {
                self.ui_ms.push(s.elapsed().as_secs_f64() * 1000.0);
            }
        }
        if self.fresh {
            self.fresh = false;
        } else {
            self.frame_in_phase += 1;
        }
    }

    pub fn answered(&mut self) {
        if let Some(w) = self.waiting_since.take() {
            self.answer_ms = Some(w.elapsed().as_secs_f64() * 1000.0);
        }
    }

    pub fn time_out(&mut self) {
        self.timed_out = true;
    }

    /// Seconds since the current phase began.
    pub fn phase_secs(&self) -> f64 {
        self.phase_started.elapsed().as_secs_f64()
    }

    /// Note what the process holds now as its memory at rest.
    pub fn note_rest(&mut self) {
        self.report.rest_private_mb = process_memory().private >> 20;
    }

    /// Close the current phase and start the next.
    pub fn next(&mut self) {
        let name = PHASES.get(self.phase).copied().unwrap_or("?").to_string();
        if self.phase == 0 {
            self.report.import_s = self.started.elapsed().as_secs_f64();
        }
        let r = PhaseReport {
            name,
            frames: self.ui_ms.len().max(self.frame_in_phase as usize),
            ui_ms_p50: pct(&self.ui_ms, 0.5),
            ui_ms_p95: pct(&self.ui_ms, 0.95),
            ui_ms_max: self.ui_ms.iter().cloned().fold(0.0, f64::max),
            cpu_ms_p95: pct(&self.cpu_ms, 0.95),
            cpu_ms_max: self.cpu_ms.iter().cloned().fold(0.0, f64::max),
            frame_ms_p50: pct(&self.frame_ms, 0.5),
            frame_ms_p95: pct(&self.frame_ms, 0.95),
            frame_ms_max: self.frame_ms.iter().cloned().fold(0.0, f64::max),
            stutters: self.frame_ms.iter().filter(|&&m| m > 33.4).count(),
            freezes: self.frame_ms.iter().filter(|&&m| m > 100.0).count(),
            answer_ms: self.answer_ms.take(),
            peak_private_mb: self.peak_phase,
            timed_out: std::mem::take(&mut self.timed_out),
            rows_waited: std::mem::take(&mut self.waited),
            waited_frames: std::mem::take(&mut self.waited_frames),
        };
        self.report.phases.push(r);
        self.phase += 1;
        self.frame_in_phase = 0;
        self.phase_started = Instant::now();
        self.ui_ms.clear();
        self.frame_ms.clear();
        self.cpu_ms.clear();
        self.peak_phase = 0;
        self.waiting_since = None;
        self.fresh = true;
    }

    /// Writes the report; returns true once written.
    pub fn finish(&mut self) -> bool {
        let m = process_memory();
        self.report.peak_private_mb = self.peak_all.max(m.private >> 20);
        self.report.peak_working_set_mb = m.peak_working_set >> 20;
        match serde_json::to_string_pretty(&self.report) {
            Ok(j) => std::fs::write(&self.path, j).is_ok(),
            Err(_) => false,
        }
    }
}
