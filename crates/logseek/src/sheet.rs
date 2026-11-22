//! Where an analysis sheet (Patterns, Shapes, Strange text, Rare pairs,
//! Timeline signals, What sets these rows apart, Search a list) sits. Every
//! sheet opens in the same place at the same size, under the top bar and
//! above the status bar, and stays there: turning a page or reading a longer
//! value never moves or resizes it. Its content scrolls inside it.

use eframe::egui;

/// The top bar's height: a sheet starts right below it, so the search box and
/// the buttons stay whole and in reach, and the strip under the bar is
/// covered whole across the sheet's width, never half a line of it.
const UNDER_TOP_BAR: f32 = 40.0;
/// Room left at the bottom for the status bar.
const ABOVE_STATUS: f32 = 40.0;
/// Room left at each side.
const SIDE: f32 = 16.0;

/// The place of a sheet in a window of `screen`: as wide as `prefer` when
/// there is room, centred, from under the top bar to above the status bar.
pub fn sheet_rect(screen: egui::Rect, prefer: f32) -> egui::Rect {
    let w = prefer.min(screen.width() - 2.0 * SIDE).max(320.0);
    let h = (screen.height() - UNDER_TOP_BAR - ABOVE_STATUS).max(240.0);
    egui::Rect::from_min_size(
        egui::pos2(screen.center().x - w * 0.5, screen.top() + UNDER_TOP_BAR),
        egui::vec2(w, h),
    )
}

/// A sheet's window, placed and sized by `sheet_rect`.
pub fn placed<'a>(win: egui::Window<'a>, ctx: &egui::Context, prefer: f32) -> egui::Window<'a> {
    let r = sheet_rect(ctx.content_rect(), prefer);
    win.title_bar(false)
        .collapsible(false)
        .resizable(false)
        .movable(false)
        .fixed_pos(r.min)
        .fixed_size(r.size())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(w: f32, h: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, h))
    }

    #[test]
    fn a_sheet_sits_under_the_top_bar_and_above_the_status_bar() {
        let r = sheet_rect(screen(1366.0, 768.0), 980.0);
        assert_eq!(r.top(), 40.0);
        assert_eq!(r.bottom(), 768.0 - 40.0);
        // centred, at the width asked for
        assert_eq!(r.width(), 980.0);
        assert_eq!(r.center().x, 683.0);
    }

    #[test]
    fn a_sheet_never_runs_past_the_sides_of_a_narrow_window() {
        let r = sheet_rect(screen(960.0, 600.0), 980.0);
        assert_eq!(r.left(), 16.0);
        assert_eq!(r.right(), 944.0);
    }

    #[test]
    fn the_same_window_gives_the_same_place_whatever_the_sheet_holds() {
        // nothing about the content is asked: the place cannot change with it
        let a = sheet_rect(screen(1920.0, 1080.0), 980.0);
        let b = sheet_rect(screen(1920.0, 1080.0), 980.0);
        assert_eq!(a, b);
        assert_eq!(a.height(), 1080.0 - 40.0 - 40.0);
    }
}
