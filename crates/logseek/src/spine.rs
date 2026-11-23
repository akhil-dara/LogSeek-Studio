//! The time spine: time runs down a rail beside the rows, first time at the
//! top, last at the bottom. Each stretch of time is a bar whose length is how
//! many rows it holds (the length carries the count, never colour alone); the
//! rows on screen are a bracket on the rail; the busiest stretch has a ring.
//! Drag down the rail to keep a period, double-click a stretch to keep it, Esc
//! to step back out. Hover reads the time and the rows there.

use crate::bridge::Hist;
use crate::moment::{group, short_time, span_label};
use crate::theme::{spine_colors, Palette};
use eframe::egui::{self, FontFamily, FontId, Rect, Sense, Stroke};
use logseek_core::types::fmt_time;

/// The rail's width, labels included.
pub const SPINE_W: f32 = 132.0;
/// Room at the top for the rail's buttons (signals, calendar).
pub const SPINE_TOOLS_H: f32 = 32.0;
/// The rail folded away: room for the one button that brings it back.
pub const SPINE_FOLDED_W: f32 = 34.0;

/// The rail's width, open or folded (Ctrl+Shift+B).
pub fn rail_width(folded: bool) -> f32 {
    if folded {
        SPINE_FOLDED_W
    } else {
        SPINE_W
    }
}

/// The rail's id: it takes the keyboard (Tab), and the app leaves the arrow
/// keys to it while it has them.
pub fn rail_id() -> egui::Id {
    egui::Id::new("spine-rail")
}

/// The rail has the keyboard.
pub fn has_focus(ctx: &egui::Context) -> bool {
    ctx.memory(|m| m.has_focus(rail_id()))
}

/// The stretch the keyboard moves to from `k` of `rows`: ↑/↓ one, Page Up/Down
/// ten, Home/End the first and last.
pub fn step(
    k: usize,
    rows: usize,
    up: bool,
    down: bool,
    page_up: bool,
    page_down: bool,
    home: bool,
    end: bool,
) -> usize {
    let last = rows.saturating_sub(1);
    if home {
        0
    } else if end {
        last
    } else if up {
        k.saturating_sub(1)
    } else if down {
        (k + 1).min(last)
    } else if page_up {
        k.saturating_sub(10)
    } else if page_down {
        (k + 10).min(last)
    } else {
        k.min(last)
    }
}

/// A stretch of the rail in words, to the minute: "2026-10-27 19:35 to
/// 2026-10-28 01:10 UTC"; the day once when both ends share it.
pub fn stretch_words(a: i64, b: i64) -> String {
    let (fa, fb) = (fmt_time(a), fmt_time(b));
    let (da, db) = (fa.get(..10).unwrap_or(&fa), fb.get(..10).unwrap_or(&fb));
    let (ma, mb) = (fa.get(..16).unwrap_or(&fa), fb.get(..16).unwrap_or(&fb));
    if da == db {
        format!("{ma} to {} UTC", mb.get(11..).unwrap_or(mb))
    } else {
        format!("{ma} to {mb} UTC")
    }
}

/// A kept period as a sentence: "Time from 2026-10-15 00:00 to 2026-10-20
/// 00:00 UTC". To the minute when both ends fall on a whole minute, else to
/// the second, else in full; never shortened past what tells the ends apart.
pub fn period_words(a: i64, b: i64) -> String {
    let at = |us: i64| -> String {
        let f = fmt_time(us);
        if us.rem_euclid(60_000_000) == 0
            && (a.rem_euclid(60_000_000) == 0 && b.rem_euclid(60_000_000) == 0)
        {
            f.get(..16).unwrap_or(&f).to_string()
        } else if a.rem_euclid(1_000_000) == 0 && b.rem_euclid(1_000_000) == 0 {
            f.get(..19).unwrap_or(&f).to_string()
        } else {
            f
        }
    };
    format!("Time from {} to {} UTC", at(a), at(b))
}

/// While the rail has the keyboard: what the cursor's stretch holds, for the
/// status bar (never painted over the rows).
pub fn keyboard_readout(ctx: &egui::Context) -> Option<String> {
    if !has_focus(ctx) {
        return None;
    }
    ctx.data(|d| d.get_temp::<String>(rail_id().with("readout")))
}

/// What the user did on the spine this frame.
pub enum SpineAction {
    None,
    /// Dragged down the rail (or double-clicked a stretch): keep this period, µs.
    Range(i64, i64),
    /// Esc while over the rail: step back out to the whole source.
    Clear,
    /// A click on the rail, or the keyboard moving to a stretch: bring the
    /// first row at or after this time (µs) to the top of the rows.
    Seek(i64),
    /// The wheel over the rail: scroll the rows by this many (up is negative).
    Scroll(f32),
}

/// The height on the rail of time `t`, the rail running from `t0` (top) to `t1` (bottom).
pub fn y_of(t: i64, rail: Rect, t0: i64, t1: i64) -> f32 {
    let f = (t - t0) as f64 / (t1 - t0).max(1) as f64;
    rail.top() + f.clamp(0.0, 1.0) as f32 * rail.height()
}

/// The time at height `y` on the rail.
pub fn time_at(y: f32, rail: Rect, t0: i64, t1: i64) -> i64 {
    let f = ((y - rail.top()) / rail.height().max(1.0)).clamp(0.0, 1.0) as f64;
    t0 + (f * (t1 - t0) as f64) as i64
}

/// Draws the spine in `rect` (its tools row left free at the top).
pub fn show(
    ui: &mut egui::Ui,
    pal: &Palette,
    hist: Option<&Hist>,
    matched: u64,
    has_time: bool,
    active: Option<(i64, i64)>,
    visible: Option<(i64, i64)>,
    seekable: bool,
    tag_times: &[(i64, u16)],
    rect: Rect,
) -> SpineAction {
    let mut action = SpineAction::None;
    let colors = spine_colors(pal);
    let label_font = FontId::new(12.0, FontFamily::Proportional);
    let p = ui.painter_at(rect);
    let Some(h) = hist.filter(|h| !h.counts.is_empty()) else {
        let msg = if matched == 0 {
            "No rows to show on the timeline"
        } else if has_time {
            "Reading the timeline"
        } else {
            "No time column in this source"
        };
        let g = p.layout(msg.to_string(), label_font, pal.muted, rect.width() - 16.0);
        p.galley(
            egui::pos2(rect.left() + 8.0, rect.top() + 8.0),
            g,
            pal.muted,
        );
        return action;
    };
    let n = h.counts.len();
    let (t0, t1) = (h.t0_us, h.t0_us + n as i64 * h.span_us);
    let range_us = t1 - t0;
    // the first time above the rail; the last time and the bars' scale below
    // it, each wrapped to the rail's width, never cut
    let wrap = rect.width() - 16.0;
    let first_g = p.layout(
        format!("{} UTC", short_time(t0, range_us)),
        label_font.clone(),
        pal.muted,
        wrap,
    );
    let last_g = p.layout(
        format!("{} UTC", short_time(t1, range_us)),
        label_font.clone(),
        pal.muted,
        wrap,
    );
    let scale_g = p.layout(
        format!("bars: rows per {}", span_label(h.span_us)),
        label_font.clone(),
        pal.muted,
        wrap,
    );
    let rail = Rect::from_min_max(
        egui::pos2(rect.left() + 8.0, rect.top() + first_g.size().y + 10.0),
        egui::pos2(
            rect.right() - 8.0,
            rect.bottom() - last_g.size().y - scale_g.size().y - 12.0,
        ),
    );
    if rail.height() < 40.0 {
        return action;
    }
    let resp = ui.interact(rail, rail_id(), Sense::click_and_drag());
    if resp.clicked() || resp.drag_started() {
        resp.request_focus();
    }
    if resp.clicked() && seekable {
        if let Some(pos) = resp.interact_pointer_pos() {
            action = SpineAction::Seek(time_at(pos.y, rail, t0, t1));
        }
    }
    if resp.hovered() {
        let dy = ui.input(|i| i.smooth_scroll_delta.y);
        if dy != 0.0 {
            action = SpineAction::Scroll(-dy / 24.0);
        }
    }
    // the bars live to the right of the tick labels
    let bars_x = rail.left() + 74.0;
    // tagged rows get a lane of their own at the right edge (only when there are any)
    let lane = if tag_times.is_empty() { 0.0 } else { 10.0 };
    let bars_w = rail.right() - lane - bars_x;
    // stretches of time as rows of the rail, 3 px each: counts summed into them
    let rows = ((rail.height() / 3.0).floor() as usize).max(1);
    let row_h = rail.height() / rows as f32;
    let mut sums = vec![0u64; rows];
    for (i, &c) in h.counts.iter().enumerate() {
        let mid = t0 + (i as i64 * h.span_us) + h.span_us / 2;
        let k = (((y_of(mid, rail, t0, t1) - rail.top()) / row_h) as usize).min(rows - 1);
        sums[k] += c;
    }
    // tagged rows per stretch: how many, and which tags
    let mut tagged = vec![(0u64, 0u16); rows];
    for &(t, m) in tag_times {
        if t < t0 || t > t1 {
            continue;
        }
        let k = (((y_of(t, rail, t0, t1) - rail.top()) / row_h) as usize).min(rows - 1);
        tagged[k].0 += 1;
        tagged[k].1 |= m;
    }
    let max = *sums.iter().max().unwrap_or(&1).max(&1) as f32;
    let len_of = |c: u64| ((c as f32).sqrt() / max.sqrt()).clamp(0.0, 1.0) * bars_w;
    let mut peak = (0usize, 0u64);
    for (k, &c) in sums.iter().enumerate() {
        if c == 0 {
            continue;
        }
        if c > peak.1 {
            peak = (k, c);
        }
        let y = rail.top() + k as f32 * row_h;
        let bar = Rect::from_min_size(
            egui::pos2(bars_x, y + 0.5),
            egui::vec2(len_of(c).max(1.5), (row_h - 1.0).max(1.0)),
        );
        p.rect_filled(bar, egui::CornerRadius::ZERO, colors.density);
    }
    // the tagged rows: each tag's own shape and colour, never colour alone
    if lane > 0.0 {
        let x = rail.right() - lane * 0.5;
        for (k, &(n, m)) in tagged.iter().enumerate() {
            if n == 0 || m == 0 {
                continue;
            }
            let i = m.trailing_zeros() as usize;
            let y = rail.top() + (k as f32 + 0.5) * row_h;
            crate::tags::draw(
                &p,
                i,
                egui::pos2(x, y),
                3.5,
                crate::tags::color(i, pal.dark),
            );
        }
    }
    // a thin axis the bars grow from
    p.line_segment(
        [
            egui::pos2(bars_x, rail.top()),
            egui::pos2(bars_x, rail.bottom()),
        ],
        Stroke::new(1.0, pal.hairline),
    );
    // the busiest stretch: a ring at the end of its bar
    if peak.1 > 0 {
        let y = rail.top() + (peak.0 as f32 + 0.5) * row_h;
        let x = (bars_x + len_of(peak.1) + 6.0).min(rail.right() - 6.0);
        p.circle_stroke(egui::pos2(x, y), 5.0, Stroke::new(1.5, pal.accent));
    }
    // the rows on screen: a bracket on the rail, so the grid and the spine agree
    if let Some((a, b)) = visible {
        let (ya, yb) = (y_of(a.min(b), rail, t0, t1), y_of(a.max(b), rail, t0, t1));
        // never thinner than 8 px, centred on where the rows are
        let mid = (ya + yb) * 0.5;
        let half = ((yb - ya) * 0.5).max(4.0);
        let (ya, yb) = (
            (mid - half).max(rail.top()),
            (mid + half).min(rail.bottom()),
        );
        let band = Rect::from_x_y_ranges(bars_x - 6.0..=rail.right(), ya..=yb);
        p.rect_filled(
            band,
            egui::CornerRadius::same(2),
            colors.brush.gamma_multiply(0.22),
        );
        p.line_segment(
            [egui::pos2(bars_x - 6.0, ya), egui::pos2(bars_x - 6.0, yb)],
            Stroke::new(2.0, colors.brush),
        );
        // a notch into the label column, so the eye finds it from the times
        let m = (ya + yb) * 0.5;
        p.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(bars_x - 6.0, m - 4.0),
                egui::pos2(bars_x - 11.0, m),
                egui::pos2(bars_x - 6.0, m + 4.0),
            ],
            colors.brush,
            Stroke::NONE,
        ));
    }
    // tick times on the left of the bars; the first and last above and below
    p.galley(
        egui::pos2(rect.left() + 8.0, rail.top() - 5.0 - first_g.size().y),
        first_g,
        pal.muted,
    );
    let below = egui::pos2(rect.left() + 8.0, rail.bottom() + 4.0);
    let last_h = last_g.size().y;
    p.galley(below, last_g, pal.muted);
    p.galley(below + egui::vec2(0.0, last_h + 2.0), scale_g, pal.muted);
    let ticks = ((rail.height() / 120.0).floor() as usize).max(1);
    for k in 1..ticks {
        let y = rail.top() + rail.height() * k as f32 / ticks as f32;
        let t = time_at(y, rail, t0, t1);
        let step = range_us / ticks as i64;
        let g = p.layout(
            crate::moment::tick_label(t, step),
            label_font.clone(),
            pal.muted,
            70.0,
        );
        let at = egui::pos2(rail.left(), y - g.size().y * 0.5);
        p.galley(at, g, pal.muted);
        p.line_segment(
            [egui::pos2(bars_x - 3.0, y), egui::pos2(bars_x, y)],
            Stroke::new(1.0, pal.control_border),
        );
    }
    // drag down the rail: keep that period
    let drag_id = ui.id().with("spine-drag");
    if resp.drag_started() {
        if let Some(p0) = resp.interact_pointer_pos() {
            ui.data_mut(|d| d.insert_temp(drag_id, p0.y));
        }
    }
    let drag_from: Option<f32> = ui.data(|d| d.get_temp(drag_id));
    if let (Some(y0), Some(pos)) = (
        drag_from,
        resp.interact_pointer_pos().filter(|_| resp.dragged()),
    ) {
        let band = Rect::from_x_y_ranges(
            rail.left()..=rail.right(),
            y0.min(pos.y).max(rail.top())..=y0.max(pos.y).min(rail.bottom()),
        );
        p.rect_filled(
            band,
            egui::CornerRadius::same(2),
            pal.accent.gamma_multiply(0.18),
        );
        p.rect_stroke(
            band,
            egui::CornerRadius::same(2),
            Stroke::new(1.2, colors.brush),
            egui::StrokeKind::Inside,
        );
    }
    if resp.drag_stopped() {
        if let (Some(y0), Some(pos)) = (drag_from, resp.interact_pointer_pos()) {
            if (pos.y - y0).abs() >= 4.0 {
                let a = time_at(y0.min(pos.y), rail, t0, t1);
                let b = time_at(y0.max(pos.y), rail, t0, t1);
                action = SpineAction::Range(a, b.max(a + 1_000_000));
            }
        }
        ui.data_mut(|d| d.remove::<f32>(drag_id));
    }
    // double-click a stretch: keep it
    if resp.double_clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let k = (((pos.y - rail.top()) / row_h).floor().max(0.0) as usize).min(rows - 1);
            let a = time_at(rail.top() + k as f32 * row_h, rail, t0, t1);
            let b = time_at(rail.top() + (k + 1) as f32 * row_h, rail, t0, t1);
            action = SpineAction::Range(a, b.max(a + 1_000_000));
        }
    }
    // Esc (or Backspace with the keyboard on the rail) steps out; a key meant
    // for the search box is never taken because the pointer rests here
    let typing = ui.ctx().text_edit_focused();
    let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
    let back = ui.input(|i| i.key_pressed(egui::Key::Backspace));
    if active.is_some()
        && ((resp.has_focus() && (esc || back)) || (resp.hovered() && !typing && esc))
    {
        action = SpineAction::Clear;
    }
    // the keyboard: Tab to the rail, ↑/↓ (Page Up/Down, Home/End) through the
    // stretches, Enter keeps the one under the cursor
    let cursor_id = rail_id().with("cursor");
    let mut cursor: Option<usize> = None;
    if resp.has_focus() {
        let k0: usize = ui.data(|d| d.get_temp(cursor_id)).unwrap_or(0);
        let k = ui.input(|i| {
            step(
                k0,
                rows,
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::PageUp),
                i.key_pressed(egui::Key::PageDown),
                i.key_pressed(egui::Key::Home),
                i.key_pressed(egui::Key::End),
            )
        });
        ui.data_mut(|d| d.insert_temp(cursor_id, k));
        if k != k0 && seekable {
            action = SpineAction::Seek(time_at(rail.top() + k as f32 * row_h, rail, t0, t1));
        }
        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let a = time_at(rail.top() + k as f32 * row_h, rail, t0, t1);
            let b = time_at(rail.top() + (k + 1) as f32 * row_h, rail, t0, t1);
            action = SpineAction::Range(a, b.max(a + 1_000_000));
        }
        cursor = Some(k);
        // the focus ring: where the keyboard is
        p.rect_stroke(
            Rect::from_x_y_ranges(
                bars_x - 3.0..=rail.right() + 4.0,
                rail.top() - 3.0..=rail.bottom() + 3.0,
            ),
            egui::CornerRadius::same(3),
            Stroke::new(2.0, pal.accent_ink),
            egui::StrokeKind::Inside,
        );
    }
    // the kept period: its edges across the rail
    if let Some((a, b)) = active {
        let (ya, yb) = (y_of(a, rail, t0, t1), y_of(b, rail, t0, t1));
        p.rect_filled(
            Rect::from_x_y_ranges(rail.left()..=rail.right(), ya..=yb.max(ya + 2.0)),
            egui::CornerRadius::same(2),
            colors.brush.gamma_multiply(0.10),
        );
        if yb - ya < rail.height() - 2.0 {
            for y in [ya, yb] {
                p.line_segment(
                    [egui::pos2(rail.left(), y), egui::pos2(rail.right(), y)],
                    Stroke::new(1.2, colors.brush),
                );
            }
        }
    }
    // hover: a guide across, and the time and rows there beside the rail
    let pointed = resp
        .hover_pos()
        .filter(|_| drag_from.is_none())
        .map(|pos| (((pos.y - rail.top()) / row_h).floor().max(0.0) as usize).min(rows - 1));
    if let Some(k) = pointed.or(cursor) {
        let y = rail.top() + (k as f32 + 0.5) * row_h;
        p.line_segment(
            [egui::pos2(rail.left(), y), egui::pos2(rail.right(), y)],
            Stroke::new(1.0, pal.control_border),
        );
        let t = time_at(rail.top() + k as f32 * row_h, rail, t0, t1);
        let t_end = time_at(rail.top() + (k + 1) as f32 * row_h, rail, t0, t1);
        let how = match (pointed.is_some(), seekable) {
            (true, true) => "click to go there  ·  drag down to keep a period",
            (true, false) => "drag down to keep a period (the rows are not in time order, so a time is no place in them: sort by the time column to go to a time)",
            (false, _) => "↑ ↓ move  ·  Enter keeps this stretch  ·  Esc steps out",
        };
        let tagged_words = match tagged[k].0 {
            0 => String::new(),
            1 => "  ·  1 tagged row".to_string(),
            n => format!("  ·  {} tagged rows", group(n)),
        };
        let text = format!(
            "{}  ·  {} rows{tagged_words}   —   {how}",
            stretch_words(t, t_end),
            group(sums[k])
        );
        if pointed.is_none() {
            // the keyboard's readout goes to the status bar, off the rows
            ui.ctx()
                .data_mut(|d| d.insert_temp(rail_id().with("readout"), text));
            return action;
        }
        let lp = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            ui.id().with("spine-readout"),
        ));
        let g = lp.layout_no_wrap(text, FontId::new(12.0, FontFamily::Proportional), pal.text);
        let size = g.size() + egui::vec2(14.0, 8.0);
        let r = Rect::from_min_size(egui::pos2(rect.right() + 6.0, y - size.y * 0.5), size);
        lp.rect_filled(r, egui::CornerRadius::same(4), pal.surface2);
        lp.rect_stroke(
            r,
            egui::CornerRadius::same(4),
            Stroke::new(1.0, pal.hairline),
            egui::StrokeKind::Inside,
        );
        lp.galley(egui::pos2(r.left() + 7.0, r.top() + 4.0), g, pal.text);
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rail() -> Rect {
        Rect::from_min_size(egui::pos2(10.0, 100.0), egui::vec2(100.0, 600.0))
    }

    #[test]
    fn time_at_and_y_of_round_trip() {
        let (t0, t1) = (1_000_000_000i64, 1_000_000_000 + 3_600_000_000);
        // the first time at the top, the last at the bottom
        assert_eq!(y_of(t0, rail(), t0, t1), 100.0);
        assert_eq!(y_of(t1, rail(), t0, t1), 700.0);
        assert_eq!(time_at(100.0, rail(), t0, t1), t0);
        assert_eq!(time_at(700.0, rail(), t0, t1), t1);
        // there and back, within one pixel's time
        let px = (t1 - t0) / 600;
        for t in [t0 + 1, t0 + 1_234_567_890, t1 - 1] {
            let back = time_at(y_of(t, rail(), t0, t1), rail(), t0, t1);
            assert!((back - t).abs() <= px, "{t} came back as {back}");
        }
        // outside the rail: held to its ends
        assert_eq!(time_at(50.0, rail(), t0, t1), t0);
        assert_eq!(y_of(t1 + 99, rail(), t0, t1), 700.0);
    }

    #[test]
    fn the_keyboard_steps_through_the_stretches_and_stays_on_the_rail() {
        let n = 100;
        // ↓ and ↑ one, held at the ends
        assert_eq!(step(5, n, false, true, false, false, false, false), 6);
        assert_eq!(step(5, n, true, false, false, false, false, false), 4);
        assert_eq!(step(0, n, true, false, false, false, false, false), 0);
        assert_eq!(step(99, n, false, true, false, false, false, false), 99);
        // a page is ten
        assert_eq!(step(5, n, false, false, false, true, false, false), 15);
        assert_eq!(step(5, n, false, false, true, false, false, false), 0);
        assert_eq!(step(95, n, false, false, false, true, false, false), 99);
        // Home and End
        assert_eq!(step(50, n, false, false, false, false, true, false), 0);
        assert_eq!(step(50, n, false, false, false, false, false, true), 99);
        // a rail that got shorter: the cursor comes back onto it
        assert_eq!(step(250, n, false, false, false, false, false, false), 99);
    }

    #[test]
    fn a_period_reads_as_a_sentence_as_exact_as_its_ends() {
        let day = 1_794_441_600_000_000i64; // 2026-11-12 00:00:00 UTC
                                            // whole minutes: to the minute
        assert_eq!(
            period_words(day, day + 3_600_000_000),
            "Time from 2026-11-12 00:00 to 2026-11-12 01:00 UTC"
        );
        // an end on a second: both to the second
        assert_eq!(
            period_words(day, day + 61_000_000),
            "Time from 2026-11-12 00:00:00 to 2026-11-12 00:01:01 UTC"
        );
        // an end inside a second: in full
        let p = period_words(day, day + 1_500_000);
        assert!(p.starts_with("Time from 2026-11-12 00:00:00"), "{p}");
        assert!(p.contains("00:00:01.5"), "{p}");
    }

    #[test]
    fn a_stretch_reads_to_the_minute() {
        // 2026-10-27 19:35:30.642986 to 2026-10-28 01:10:00
        let a = 1_793_129_730_642_986i64;
        let b = 1_793_149_800_000_000i64;
        assert_eq!(
            stretch_words(a, b),
            "2026-10-27 19:35 to 2026-10-28 01:10 UTC"
        );
        // the same day: written once
        assert_eq!(
            stretch_words(a, a + 600_000_000),
            "2026-10-27 19:35 to 19:45 UTC"
        );
    }

    #[test]
    fn a_rail_of_one_instant_does_not_divide_by_nothing() {
        let t = 5_000_000i64;
        assert_eq!(y_of(t, rail(), t, t), 100.0);
        assert_eq!(time_at(400.0, rail(), t, t), t);
    }
}
