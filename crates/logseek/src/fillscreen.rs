//! The window starts filling the screen. Windows can be left saying a window
//! is maximised while it has an ordinary size (a size given to a window that
//! is already maximised keeps the maximised mark). This tells that state
//! from a window that really fills its screen.

use eframe::egui;

/// True when a window said to be maximised is plainly smaller than its
/// screen. A maximised window covers the screen less the taskbar, so a little
/// short is normal; this much short is the fault.
pub fn short_of_the_screen(outer: egui::Vec2, monitor: egui::Vec2) -> bool {
    if monitor.x <= 0.0 || monitor.y <= 0.0 {
        return false;
    }
    outer.x < monitor.x * 0.92 || outer.y < monitor.y * 0.8
}

#[cfg(test)]
mod tests {
    use super::short_of_the_screen;
    use eframe::egui::vec2;

    #[test]
    fn a_window_at_its_starting_size_does_not_fill_a_bigger_screen() {
        // 1920x1080 at 125%: the screen is 1536x864, the window came up 1366x792
        assert!(short_of_the_screen(vec2(1366.4, 792.0), vec2(1536.0, 864.0)));
        assert!(short_of_the_screen(vec2(1366.0, 600.0), vec2(1366.0, 768.0)));
    }

    #[test]
    fn a_maximised_window_fills_it_whatever_side_the_taskbar_is_on() {
        // borders hang over the edges; the taskbar takes the bottom
        assert!(!short_of_the_screen(vec2(1550.4, 830.4), vec2(1536.0, 864.0)));
        // a taskbar down the side
        assert!(!short_of_the_screen(vec2(1474.0, 878.0), vec2(1536.0, 864.0)));
        // a screen whose size is not known says nothing
        assert!(!short_of_the_screen(vec2(800.0, 600.0), vec2(0.0, 0.0)));
    }
}
