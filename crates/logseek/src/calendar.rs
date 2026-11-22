//! The calendar picker: a month of days, each showing how many rows it holds
//! (like a fare calendar shows prices), with an hour strip for the chosen day.
//! All days are UTC, the same as the timeline.

use crate::bridge::Calendar;
use crate::moment::group;
use crate::theme::{self, Palette};
use eframe::egui::{self, Align2, CornerRadius, Rect, RichText, Sense, Stroke};
use egui_phosphor::regular as ph;
use logseek_core::types::{civil_from_days, days_from_civil};
use std::collections::BTreeMap;

const DAY: i64 = 86_400_000_000;
const HOUR: i64 = 3_600_000_000;
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// 42 cells (6 weeks, Monday first): the day of the month, or `None` outside it.
pub fn month_cells(y: i64, m: u32) -> Vec<Option<u32>> {
    let first = days_from_civil(y, m, 1);
    // 1970-01-01 was a Thursday (index 3 when Monday is 0)
    let lead = (first + 3).rem_euclid(7) as usize;
    let (ny, nm) = next_month(y, m);
    let len = (days_from_civil(ny, nm, 1) - first) as u32;
    (0..42)
        .map(|i| {
            let d = i as i64 - lead as i64 + 1;
            (d >= 1 && d <= len as i64).then_some(d as u32)
        })
        .collect()
}

pub fn next_month(y: i64, m: u32) -> (i64, u32) {
    if m == 12 {
        (y + 1, 1)
    } else {
        (y, m + 1)
    }
}

pub fn prev_month(y: i64, m: u32) -> (i64, u32) {
    if m == 1 {
        (y - 1, 12)
    } else {
        (y, m - 1)
    }
}

/// Rows per day (days counted from 1970-01-01, UTC).
pub fn day_totals(t0_us: i64, span_us: i64, counts: &[u64]) -> BTreeMap<i64, u64> {
    let mut out = BTreeMap::new();
    for (i, &c) in counts.iter().enumerate() {
        if c > 0 {
            let day = (t0_us + i as i64 * span_us).div_euclid(DAY);
            *out.entry(day).or_insert(0) += c;
        }
    }
    out
}

/// Rows in each hour of `day`, when the counts are hourly.
pub fn hour_counts(t0_us: i64, span_us: i64, counts: &[u64], day: i64) -> Option<[u64; 24]> {
    if span_us != HOUR {
        return None;
    }
    let mut out = [0u64; 24];
    for (h, slot) in out.iter_mut().enumerate() {
        let t = day * DAY + h as i64 * HOUR;
        let i = (t - t0_us).div_euclid(HOUR);
        if i >= 0 {
            *slot = counts.get(i as usize).copied().unwrap_or(0);
        }
    }
    Some(out)
}

/// Days `a`..=`b` (either order), optionally narrowed to hours `h1`..=`h2` of a
/// single day, as a UTC range with an exclusive end.
pub fn range_of(a: i64, b: i64, hours: Option<(u8, u8)>) -> (i64, i64) {
    let (lo, hi) = (a.min(b), a.max(b));
    match hours {
        Some((h1, h2)) if lo == hi => {
            let (h1, h2) = (h1.min(h2) as i64, h1.max(h2) as i64);
            (lo * DAY + h1 * HOUR, lo * DAY + (h2 + 1) * HOUR)
        }
        _ => (lo * DAY, (hi + 1) * DAY),
    }
}

/// "Wed 24 Aug 2016".
pub fn day_label(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    let wd = WEEKDAYS[(day + 3).rem_euclid(7) as usize];
    format!("{wd} {d} {} {y}", &MONTHS[m as usize - 1][..3])
}

/// What the picker holds between frames.
#[derive(Clone, Default)]
pub struct CalState {
    pub month: Option<(i64, u32)>,
    pub a: Option<i64>,
    pub b: Option<i64>,
    pub hours: Option<(u8, u8)>,
    hour_drag: Option<u8>,
    /// Shorter day cells, for the condition popup on small screens.
    pub compact: bool,
    /// The exact start and end, to the second, typed or stepped by hand.
    exact: Option<Exact>,
}

/// A start and an end written out: a date and hours, minutes, seconds (UTC).
#[derive(Clone, PartialEq)]
struct Exact {
    from_date: String,
    from: [u32; 3],
    to_date: String,
    to: [u32; 3],
    /// The days and hours picked above when these fields were last filled from them.
    filled_from: Option<(i64, i64, Option<(u8, u8)>)>,
}

/// A date as it is typed in the exact fields, 2026-11-12, as days since 1970.
/// `None` when it is not a real date.
pub fn typed_day(text: &str) -> Option<i64> {
    let mut parts = text.trim().split('-');
    let y: i64 = parts.next()?.trim().parse().ok()?;
    let m: u32 = parts.next()?.trim().parse().ok()?;
    let d: u32 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&m) || d == 0 {
        return None;
    }
    let day = days_from_civil(y, m, d);
    // the 31st of a 30-day month would roll into the next: not a real date
    (civil_from_days(day) == (y, m, d)).then_some(day)
}

/// The range (UTC µs, end exclusive) from a start and an end given as a date
/// and hours, minutes, seconds. The end's own second is included.
pub fn exact_range(
    from_date: &str,
    from: [u32; 3],
    to_date: &str,
    to: [u32; 3],
) -> Result<(i64, i64), String> {
    const SEC: i64 = 1_000_000;
    let at = |date: &str, t: [u32; 3], which: &str| {
        typed_day(date)
            .map(|d| d * DAY + (t[0] as i64 * 3600 + t[1] as i64 * 60 + t[2] as i64) * SEC)
            .ok_or_else(|| format!("The {which} date must read like 2026-11-12."))
    };
    let a = at(from_date, from, "start")?;
    let b = at(to_date, to, "end")?;
    if b < a {
        return Err("The end is before the start.".into());
    }
    Ok((a, b + SEC))
}

/// A time written as text, as its date and its hours, minutes and seconds.
/// A date alone is the start of that day; anything after the seconds (a
/// fraction, a zone) is left out. A date still being typed is kept as typed.
pub fn split_when(text: &str) -> (String, [u32; 3]) {
    let text = text.trim();
    let (date, rest) = text.split_once([' ', 'T']).unwrap_or((text, ""));
    let mut clock = [0u32; 3];
    for (i, part) in rest.split(':').take(3).enumerate() {
        // "47.613 UTC" is 47 seconds
        let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
        clock[i] = digits.parse().unwrap_or(0);
    }
    (date.to_string(), clock)
}

/// A date and a clock as one written time.
pub fn join_when(date: &str, clock: [u32; 3]) -> String {
    format!("{} {:02}:{:02}:{:02}", date.trim(), clock[0], clock[1], clock[2])
}

/// A date box and hours, minutes, seconds for one written time (UTC). What
/// is entered is written back to `text` as "2026-11-12 18:30:00".
pub fn when_entry(ui: &mut egui::Ui, pal: &Palette, text: &mut String) {
    let (mut date, mut clock) = split_when(text);
    // as tall as the number boxes beside it
    let tall = (ui.text_style_height(&egui::TextStyle::Body)
        + 2.0 * ui.spacing().button_padding.y)
        .max(ui.spacing().interact_size.y);
    ui.add_sized(
        [104.0, tall],
        egui::TextEdit::singleline(&mut date)
            .font(theme::data_font())
            .hint_text("2026-11-12")
            .vertical_align(egui::Align::Center),
    )
    .on_hover_text("The date: year-month-day (UTC)");
    // a calendar to pick the day from, and the clock, behind one button
    let (btn, _) = egui::containers::menu::MenuButton::new(
        RichText::new(ph::CALENDAR_BLANK).color(pal.accent_ink),
    )
    .config(
        egui::containers::menu::MenuConfig::new()
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside),
    )
    .ui(ui, |ui| pick_when(ui, pal, &mut date, &mut clock));
    let btn = btn.on_hover_text("Pick the day from a calendar, and the time");
    // dev switch: LOGSEEK_SHOT_PICK=1 opens the first picker for a picture
    if crate::dev::var_os("LOGSEEK_SHOT_PICK").is_some() {
        std::env::remove_var("LOGSEEK_SHOT_PICK");
        egui::Popup::open_id(ui.ctx(), egui::Popup::default_response_id(&btn));
    }
    for (i, (most, what)) in [(23, "Hours"), (59, "Minutes"), (59, "Seconds")]
        .into_iter()
        .enumerate()
    {
        if i > 0 {
            ui.label(RichText::new(":").color(pal.text2));
        }
        ui.add(
            egui::DragValue::new(&mut clock[i])
                .range(0..=most)
                .speed(0.1)
                .custom_formatter(|n, _| format!("{:02}", n as u32)),
        )
        .on_hover_text(format!(
            "{what}, 0 to {most}: type a number, or drag, or use the arrow keys"
        ));
    }
    *text = if date.trim().is_empty() {
        String::new()
    } else {
        join_when(&date, clock)
    };
}

/// The month a picker opens on: the month of the date written, else this month.
pub fn month_to_show(date: &str, today: i64) -> (i64, u32) {
    let (y, m, _) = civil_from_days(typed_day(date).unwrap_or(today));
    (y, m)
}

/// The calendar and the clock of one written time: a month to click a day
/// in, the hours, minutes and seconds, and the ends of the day in one click.
fn pick_when(ui: &mut egui::Ui, pal: &Palette, date: &mut String, clock: &mut [u32; 3]) {
    ui.set_min_width(268.0);
    ui.set_max_width(300.0);
    let today = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64 / 86_400);
    let chosen = typed_day(date);
    // the month on show is kept while the picker is open
    let key = ui.id().with("month-shown");
    let (mut y, mut m): (i64, u32) = ui
        .data(|d| d.get_temp(key))
        .unwrap_or_else(|| month_to_show(date, today));
    ui.horizontal(|ui| {
        if ui.button(ph::CARET_LEFT).on_hover_text("The month before").clicked() {
            (y, m) = prev_month(y, m);
        }
        ui.label(
            RichText::new(format!("{} {y}", MONTHS[(m - 1) as usize]))
                .font(crate::fonts::strong(13.0)),
        );
        if ui.button(ph::CARET_RIGHT).on_hover_text("The month after").clicked() {
            (y, m) = next_month(y, m);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Today").clicked() {
                *date = date_text(today);
                (y, m) = month_to_show(date, today);
            }
        });
    });
    ui.data_mut(|d| d.insert_temp(key, (y, m)));
    egui::Grid::new("pick-when-days")
        .num_columns(7)
        .spacing([2.0, 2.0])
        .show(ui, |ui| {
            for w in WEEKDAYS {
                ui.label(RichText::new(w).font(theme::small_font()).color(pal.text2));
            }
            ui.end_row();
            for (i, cell) in month_cells(y, m).into_iter().enumerate() {
                match cell {
                    Some(d) => {
                        let day = days_from_civil(y, m, d);
                        let on = chosen == Some(day);
                        let b = egui::Button::selectable(
                            on,
                            RichText::new(d.to_string()).color(if on {
                                pal.accent_ink
                            } else if day == today {
                                pal.text
                            } else {
                                pal.text2
                            }),
                        )
                        .min_size(egui::vec2(34.0, 26.0));
                        if ui.add(b).clicked() {
                            *date = date_text(day);
                        }
                    }
                    None => {
                        ui.label("");
                    }
                }
                if i % 7 == 6 {
                    ui.end_row();
                }
            }
        });
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Time").color(pal.text2));
        for (i, most) in [23u32, 59, 59].into_iter().enumerate() {
            if i > 0 {
                ui.label(RichText::new(":").color(pal.text2));
            }
            ui.add(
                egui::DragValue::new(&mut clock[i])
                    .range(0..=most)
                    .speed(0.1)
                    .custom_formatter(|n, _| format!("{:02}", n as u32)),
            );
        }
        ui.label(RichText::new("UTC").color(pal.text2));
    });
    ui.horizontal_wrapped(|ui| {
        for (label, c) in [
            ("Start of the day", [0, 0, 0]),
            ("Midday", [12, 0, 0]),
            ("End of the day", [23, 59, 59]),
        ] {
            if ui.selectable_label(*clock == c, label).clicked() {
                *clock = c;
            }
        }
    });
    if date.trim().is_empty() {
        ui.label(
            RichText::new("Click a day to set the date.")
                .font(theme::small_font())
                .color(pal.text2),
        );
    }
}

fn date_text(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

/// A time in µs as (day, [hours, minutes, seconds]).
fn day_and_clock(us: i64) -> (i64, [u32; 3]) {
    let day = us.div_euclid(DAY);
    let secs = (us.rem_euclid(DAY) / 1_000_000) as u32;
    (day, [secs / 3600, secs / 60 % 60, secs % 60])
}

pub enum CalAction {
    None,
    /// Use this UTC range (end exclusive); `whole_days` when no hours were narrowed.
    Use {
        a: i64,
        b: i64,
        whole_days: bool,
    },
    Cancel,
}

/// Draws the picker. `cal` is `None` while the counts are on their way.
pub fn show(
    ui: &mut egui::Ui,
    pal: &Palette,
    cal: Option<&Calendar>,
    st: &mut CalState,
) -> CalAction {
    let mut action = CalAction::None;
    let Some(cal) = cal else {
        ui.label(RichText::new("Counting rows per day").color(pal.muted));
        return action;
    };
    let totals = day_totals(cal.t0_us, cal.span_us, &cal.counts);
    if totals.is_empty() {
        ui.label(RichText::new("No rows with a time to show").color(pal.muted));
        return action;
    }
    // open on the busiest month
    let (y, m) = *st.month.get_or_insert_with(|| {
        let busiest = totals
            .iter()
            .max_by_key(|(_, c)| **c)
            .map(|(d, _)| *d)
            .unwrap_or(0);
        let (y, m, _) = civil_from_days(busiest);
        (y, m)
    });
    let first_day = *totals.keys().next().unwrap();
    let last_day = *totals.keys().next_back().unwrap();
    let cells = month_cells(y, m);
    let month_start = days_from_civil(y, m, 1);
    let month_max = cells
        .iter()
        .flatten()
        .map(|d| {
            totals
                .get(&(month_start + *d as i64 - 1))
                .copied()
                .unwrap_or(0)
        })
        .max()
        .unwrap_or(0)
        .max(1);

    let cell_w = 62.0;
    let cell_h = if st.compact { 36.0 } else { 44.0 };
    let grid_w = cell_w * 7.0;
    // header: month and arrows
    ui.horizontal(|ui| {
        ui.set_width(grid_w);
        let (py, pm) = prev_month(y, m);
        let can_prev = days_from_civil(py, pm, 1) <= last_day && month_start > first_day;
        if ui
            .add_enabled(can_prev, egui::Button::new(ph::CARET_LEFT).frame(false))
            .on_hover_text("Previous month")
            .clicked()
        {
            st.month = Some((py, pm));
        }
        ui.label(
            RichText::new(format!("{} {y}", MONTHS[m as usize - 1]))
                .font(crate::fonts::strong(14.0)),
        );
        let (ny, nm) = next_month(y, m);
        let can_next = days_from_civil(ny, nm, 1) <= last_day;
        if ui
            .add_enabled(can_next, egui::Button::new(ph::CARET_RIGHT).frame(false))
            .on_hover_text("Next month")
            .clicked()
        {
            st.month = Some((ny, nm));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new("rows per day · UTC")
                    .font(theme::small_font())
                    .color(pal.text2),
            );
        });
    });
    // weekday names
    let (wr, _) = ui.allocate_exact_size(egui::vec2(grid_w, 18.0), Sense::hover());
    for (i, w) in WEEKDAYS.iter().enumerate() {
        ui.painter().text(
            egui::pos2(wr.left() + i as f32 * cell_w + cell_w * 0.5, wr.center().y),
            Align2::CENTER_CENTER,
            *w,
            theme::small_font(),
            pal.text2,
        );
    }
    // the days
    let weeks = cells
        .iter()
        .rposition(|c| c.is_some())
        .map(|i| i / 7 + 1)
        .unwrap_or(6);
    let (gr, _) = ui.allocate_exact_size(egui::vec2(grid_w, cell_h * weeks as f32), Sense::hover());
    let shift = ui.input(|i| i.modifiers.shift);
    let sel =
        st.a.map(|a| (a.min(st.b.unwrap_or(a)), a.max(st.b.unwrap_or(a))));
    for (i, cell) in cells.iter().enumerate() {
        let Some(dom) = cell else { continue };
        let day = month_start + *dom as i64 - 1;
        let r = Rect::from_min_size(
            egui::pos2(
                gr.left() + (i % 7) as f32 * cell_w,
                gr.top() + (i / 7) as f32 * cell_h,
            ),
            egui::vec2(cell_w, cell_h),
        )
        .shrink(2.0);
        let count = totals.get(&day).copied().unwrap_or(0);
        let resp = ui.interact(r, ui.id().with(("cal-day", day)), Sense::click());
        let p = ui.painter();
        if count > 0 {
            // busier days are deeper; the count is printed too, never colour alone
            let k = ((count as f32).sqrt() / (month_max as f32).sqrt()).clamp(0.08, 1.0);
            p.rect_filled(
                r,
                CornerRadius::same(5),
                pal.info.gamma_multiply(0.10 + 0.55 * k),
            );
        } else {
            p.rect_stroke(
                r,
                CornerRadius::same(5),
                Stroke::new(1.0, pal.hairline),
                egui::StrokeKind::Inside,
            );
        }
        let chosen = sel.is_some_and(|(lo, hi)| day >= lo && day <= hi);
        if chosen {
            p.rect_stroke(
                r,
                CornerRadius::same(5),
                Stroke::new(2.0, pal.accent_ink),
                egui::StrokeKind::Inside,
            );
        } else if resp.hovered() {
            p.rect_stroke(
                r,
                CornerRadius::same(5),
                Stroke::new(1.0, pal.control_border),
                egui::StrokeKind::Inside,
            );
        }
        let text_col = if count > 0 { pal.text } else { pal.muted };
        p.text(
            r.left_top() + egui::vec2(6.0, 4.0),
            Align2::LEFT_TOP,
            dom.to_string(),
            crate::fonts::strong(12.0),
            text_col,
        );
        if count > 0 {
            p.text(
                r.right_bottom() - egui::vec2(5.0, 4.0),
                Align2::RIGHT_BOTTOM,
                group(count),
                theme::small_font(),
                pal.text,
            );
        }
        let resp = resp.on_hover_text(format!("{} · {} rows", day_label(day), group(count)));
        if resp.clicked() {
            if shift && st.a.is_some() {
                st.b = Some(day);
            } else {
                st.a = Some(day);
                st.b = Some(day);
            }
            st.hours = None;
        }
    }
    ui.label(
        RichText::new("Click a day · Shift+click another day for a range")
            .font(theme::small_font())
            .color(pal.text2),
    );
    // the hours of a single chosen day
    let single = sel.filter(|(lo, hi)| lo == hi).map(|(d, _)| d);
    if let Some(hours) = single.and_then(|d| hour_counts(cal.t0_us, cal.span_us, &cal.counts, d)) {
        ui.add_space(4.0);
        let hmax = hours.iter().copied().max().unwrap_or(0).max(1);
        let hw = grid_w / 24.0;
        let (hr, hresp) = ui.allocate_exact_size(egui::vec2(grid_w, 22.0), Sense::click_and_drag());
        let hour_at = |x: f32| (((x - hr.left()) / hw).floor().clamp(0.0, 23.0)) as u8;
        if hresp.drag_started() || hresp.clicked() {
            if let Some(pos) = hresp.interact_pointer_pos() {
                let h = hour_at(pos.x);
                st.hour_drag = Some(h);
                st.hours = Some((h, h));
            }
        }
        if hresp.dragged() {
            if let (Some(h0), Some(pos)) = (st.hour_drag, hresp.interact_pointer_pos()) {
                st.hours = Some((h0, hour_at(pos.x)));
            }
        }
        if hresp.drag_stopped() {
            st.hour_drag = None;
        }
        let p = ui.painter();
        for (h, &c) in hours.iter().enumerate() {
            let r = Rect::from_min_size(
                egui::pos2(hr.left() + h as f32 * hw, hr.top()),
                egui::vec2(hw, 22.0),
            )
            .shrink(1.0);
            if c > 0 {
                let k = ((c as f32).sqrt() / (hmax as f32).sqrt()).clamp(0.08, 1.0);
                p.rect_filled(
                    r,
                    CornerRadius::same(2),
                    pal.info.gamma_multiply(0.10 + 0.6 * k),
                );
            } else {
                p.rect_stroke(
                    r,
                    CornerRadius::same(2),
                    Stroke::new(1.0, pal.hairline),
                    egui::StrokeKind::Inside,
                );
            }
            let on = st
                .hours
                .is_some_and(|(a, b)| (h as u8) >= a.min(b) && (h as u8) <= a.max(b));
            if on {
                p.rect_stroke(
                    r,
                    CornerRadius::same(2),
                    Stroke::new(2.0, pal.accent_ink),
                    egui::StrokeKind::Inside,
                );
            }
        }
        let (lr, _) = ui.allocate_exact_size(egui::vec2(grid_w, 14.0), Sense::hover());
        for h in (0..24).step_by(3) {
            ui.painter().text(
                egui::pos2(lr.left() + h as f32 * hw + 1.0, lr.center().y),
                Align2::LEFT_CENTER,
                format!("{h:02}"),
                theme::small_font(),
                pal.text2,
            );
        }
        if let Some(pos) = hresp.hover_pos() {
            let h = hour_at(pos.x) as usize;
            hresp.on_hover_text(format!(
                "{h:02}:00 to {:02}:00 UTC · {} rows · drag across hours",
                (h + 1) % 24,
                group(hours[h])
            ));
        }
    }
    // the exact start and end, to the second: filled from the days and hours
    // picked above, and free to change by typing or with the arrow keys
    {
        let picked = sel.map(|(lo, hi)| (lo, hi, st.hours));
        let stale = match &st.exact {
            None => true,
            Some(e) => picked.is_some() && e.filled_from != picked,
        };
        if stale {
            let (a, b) = match sel {
                Some((lo, hi)) => range_of(lo, hi, st.hours),
                None => (first_day * DAY, (last_day + 1) * DAY),
            };
            let (fd, ft) = day_and_clock(a);
            let (td, tt) = day_and_clock(b - 1_000_000);
            st.exact = Some(Exact {
                from_date: date_text(fd),
                from: ft,
                to_date: date_text(td),
                to: tt,
                filled_from: picked,
            });
        }
        ui.add_space(8.0);
        ui.label(RichText::new("Exact times (UTC)").font(crate::fonts::strong(12.5)));
        if let Some(e) = st.exact.as_mut() {
            let side = |ui: &mut egui::Ui, label: &str, date: &mut String, t: &mut [u32; 3]| {
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [36.0, 20.0],
                        egui::Label::new(RichText::new(label).color(pal.text2)),
                    );
                    // as tall as the number boxes beside it
                    let tall = (ui.text_style_height(&egui::TextStyle::Body)
                        + 2.0 * ui.spacing().button_padding.y)
                        .max(ui.spacing().interact_size.y);
                    ui.add_sized(
                        [104.0, tall],
                        egui::TextEdit::singleline(date)
                            .font(theme::data_font())
                            .hint_text("2026-11-12")
                            .vertical_align(egui::Align::Center),
                    )
                    .on_hover_text("The date: year-month-day");
                    for (i, (most, what)) in [(23, "Hours"), (59, "Minutes"), (59, "Seconds")]
                        .into_iter()
                        .enumerate()
                    {
                        if i > 0 {
                            ui.label(RichText::new(":").color(pal.text2));
                        }
                        ui.add(
                            egui::DragValue::new(&mut t[i])
                                .range(0..=most)
                                .speed(0.1)
                                .custom_formatter(|n, _| format!("{:02}", n as u32)),
                        )
                        .on_hover_text(format!(
                            "{what}, 0 to {most}: type a number, or drag, or use the arrow keys"
                        ));
                    }
                });
            };
            side(ui, "From", &mut e.from_date, &mut e.from);
            side(ui, "To", &mut e.to_date, &mut e.to);
        }
        ui.add_space(4.0);
    }
    // one action for both ways of choosing: the exact times are what is used.
    // They follow the days and hours picked above until they are changed by hand.
    let exact = st
        .exact
        .as_ref()
        .map(|e| exact_range(&e.from_date, e.from, &e.to_date, e.to));
    let picked = sel.map(|(lo, hi)| range_of(lo, hi, st.hours));
    match exact {
        Some(Err(why)) => {
            ui.label(RichText::new(why).color(pal.danger));
            if ui.button("Cancel").clicked() {
                action = CalAction::Cancel;
            }
            return action;
        }
        Some(Ok((a, b))) if picked != Some((a, b)) => {
            // typed by hand: said as the times themselves
            let t = logseek_core::types::fmt_time;
            ui.label(
                RichText::new(format!("{} to {} UTC", t(a), t(b - 1_000_000)))
                    .font(crate::fonts::strong(12.5)),
            );
            ui.horizontal(|ui| {
                if crate::app::primary_button(ui, pal, "Show these rows")
                    .on_hover_text("From the first time to the second, both included")
                    .clicked()
                {
                    action = CalAction::Use {
                        a,
                        b,
                        whole_days: false,
                    };
                }
                if ui.button("Cancel").clicked() {
                    action = CalAction::Cancel;
                }
            });
            return action;
        }
        _ => {}
    }
    // what is chosen, exactly
    if let Some((lo, hi)) = sel {
        let (a, b) = range_of(lo, hi, st.hours);
        let rows: u64 = match (
            st.hours,
            single.and_then(|d| hour_counts(cal.t0_us, cal.span_us, &cal.counts, d)),
        ) {
            (Some((h1, h2)), Some(hours)) => hours[h1.min(h2) as usize..=h1.max(h2) as usize]
                .iter()
                .sum(),
            _ => totals.range(lo..=hi).map(|(_, c)| *c).sum(),
        };
        let what = match st.hours {
            Some((h1, h2)) if lo == hi => format!(
                "{} · {:02}:00 to {:02}:00 UTC",
                day_label(lo),
                h1.min(h2),
                (h1.max(h2) + 1) % 24
            ),
            _ if lo == hi => day_label(lo),
            _ => format!("{} to {}", day_label(lo), day_label(hi)),
        };
        ui.add_space(4.0);
        ui.label(
            RichText::new(format!("{what} · {} rows", group(rows)))
                .font(crate::fonts::strong(12.5)),
        );
        ui.horizontal(|ui| {
            if crate::app::primary_button(ui, pal, "Show these rows").clicked() {
                action = CalAction::Use {
                    a,
                    b,
                    whole_days: st.hours.is_none(),
                };
            }
            if ui.button("Cancel").clicked() {
                action = CalAction::Cancel;
            }
        });
    } else if ui.button("Cancel").clicked() {
        action = CalAction::Cancel;
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use logseek_core::types::days_from_civil;

    const DAY: i64 = 86_400_000_000;
    const HOUR: i64 = 3_600_000_000;

    #[test]
    fn months_start_on_monday() {
        // 1 Aug 2016 was a Monday, 1 Sep 2016 a Thursday
        let aug = month_cells(2016, 8);
        assert_eq!(aug.len(), 42);
        assert_eq!(aug[0], Some(1));
        assert_eq!(aug[30], Some(31));
        assert_eq!(aug[31], None);
        let sep = month_cells(2016, 9);
        assert_eq!(&sep[..4], &[None, None, None, Some(1)]);
        assert_eq!(next_month(2016, 12), (2017, 1));
        assert_eq!(prev_month(2017, 1), (2016, 12));
    }

    #[test]
    fn hours_add_up_to_days() {
        let d = days_from_civil(2016, 8, 24);
        let counts: Vec<u64> = (1..=30).collect(); // 24 hours of day d, 6 of the next
        let totals = day_totals(d * DAY, HOUR, &counts);
        assert_eq!(totals.get(&d), Some(&(1..=24).sum::<u64>()));
        assert_eq!(totals.get(&(d + 1)), Some(&(25..=30).sum::<u64>()));
        let hours = hour_counts(d * DAY, HOUR, &counts, d).unwrap();
        assert_eq!(hours[0], 1);
        assert_eq!(hours[23], 24);
        // daily buckets have no hours to show
        assert!(hour_counts(d * DAY, DAY, &counts, d).is_none());
    }

    #[test]
    fn exact_times_make_a_range_that_includes_the_last_second() {
        let d = days_from_civil(2026, 11, 12);
        const S: i64 = 1_000_000;
        // 02:20:15 to 02:20:18 on one day: three seconds and the end's own
        assert_eq!(
            exact_range("2026-11-12", [2, 20, 15], "2026-11-12", [2, 20, 18]),
            Ok((d * DAY + 8415 * S, d * DAY + 8419 * S))
        );
        // across midnight, and spaces around the date are forgiven
        assert_eq!(
            exact_range(" 2026-11-12 ", [23, 59, 59], "2026-11-13", [0, 0, 0]),
            Ok((d * DAY + 86_399 * S, (d + 1) * DAY + S))
        );
        // the same second for both is one second
        assert_eq!(
            exact_range("2026-11-12", [0, 0, 0], "2026-11-12", [0, 0, 0]),
            Ok((d * DAY, d * DAY + S))
        );
    }

    #[test]
    fn exact_times_say_what_is_wrong() {
        assert_eq!(
            exact_range("2026-11-12", [5, 0, 0], "2026-11-12", [4, 0, 0]),
            Err("The end is before the start.".to_string())
        );
        assert_eq!(
            exact_range("12/11/2026", [0, 0, 0], "2026-11-12", [0, 0, 0]),
            Err("The start date must read like 2026-11-12.".to_string())
        );
        // dates that do not exist are refused, not rolled into the next month
        assert_eq!(typed_day("2026-11-31"), None);
        assert_eq!(typed_day("2026-02-29"), None);
        assert_eq!(typed_day("2024-02-29"), Some(days_from_civil(2024, 2, 29)));
        assert_eq!(typed_day("2026-13-01"), None);
        assert_eq!(typed_day("2026-11"), None);
    }

    #[test]
    fn a_written_time_splits_into_a_date_and_a_clock_and_joins_back() {
        assert_eq!(
            split_when("2026-11-12 18:37:47"),
            ("2026-11-12".to_string(), [18, 37, 47])
        );
        // a date alone is the start of the day; fractions of a second are dropped
        assert_eq!(split_when("2026-11-12"), ("2026-11-12".to_string(), [0, 0, 0]));
        assert_eq!(
            split_when("2026-11-12 18:37:47.613 UTC"),
            ("2026-11-12".to_string(), [18, 37, 47])
        );
        assert_eq!(split_when(""), (String::new(), [0, 0, 0]));
        // a date still being typed is kept as typed
        assert_eq!(split_when("2026-1"), ("2026-1".to_string(), [0, 0, 0]));
        assert_eq!(join_when("2026-11-12", [8, 5, 9]), "2026-11-12 08:05:09");
    }

    #[test]
    fn a_time_splits_into_its_day_and_clock() {
        let d = days_from_civil(2026, 11, 12);
        assert_eq!(day_and_clock(d * DAY + 8_415_000_000), (d, [2, 20, 15]));
        assert_eq!(date_text(d), "2026-11-12");
    }

    #[test]
    fn a_choice_becomes_a_time_range() {
        let d = days_from_civil(2016, 8, 24);
        assert_eq!(range_of(d, d, None), (d * DAY, (d + 1) * DAY));
        assert_eq!(range_of(d + 2, d, None), (d * DAY, (d + 3) * DAY));
        assert_eq!(
            range_of(d, d, Some((9, 6))),
            (d * DAY + 6 * HOUR, d * DAY + 10 * HOUR)
        );
        assert_eq!(day_label(d), "Wed 24 Aug 2016");
    }

    #[test]
    fn a_picker_opens_on_the_month_written_else_on_this_month() {
        let today = days_from_civil(2026, 11, 22);
        assert_eq!(month_to_show("2026-10-22", today), (2026, 10));
        assert_eq!(month_to_show("", today), (2026, 11));
        assert_eq!(month_to_show("not a date", today), (2026, 11));
    }
}
