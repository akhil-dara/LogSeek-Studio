//! Long lists in a window, a page at a time: every line can be reached
//! (first, back, next, last), and only one page is laid out per frame, so a
//! list of tens of thousands of lines stays as quick as one of ten.

use crate::moment::group;
use crate::theme::Palette;
use eframe::egui::{self, RichText};
use egui_phosphor::regular as ph;
use std::ops::Range;

/// Lines per page.
pub const PER: usize = 100;

/// The lines of page `page` of `total` (the page is kept in range).
pub fn range(page: &mut usize, total: usize) -> Range<usize> {
    let pages = total.div_ceil(PER).max(1);
    if *page >= pages {
        *page = pages - 1;
    }
    let a = *page * PER;
    a..(a + PER).min(total)
}

/// The page controls (only when there is more than one page); the lines to show.
pub fn pager(ui: &mut egui::Ui, pal: &Palette, page: &mut usize, total: usize) -> Range<usize> {
    let r = range(page, total);
    let pages = total.div_ceil(PER).max(1);
    if pages > 1 {
        ui.horizontal_wrapped(|ui| {
            let at_start = *page == 0;
            let at_end = *page + 1 >= pages;
            if ui
                .add_enabled(!at_start, egui::Button::new(ph::CARET_DOUBLE_LEFT))
                .on_hover_text("First page")
                .clicked()
            {
                *page = 0;
            }
            if ui
                .add_enabled(!at_start, egui::Button::new(ph::CARET_LEFT))
                .on_hover_text("Page before")
                .clicked()
            {
                *page -= 1;
            }
            ui.label(
                RichText::new(format!(
                    "Lines {}–{} of {}  ·  page {} of {}",
                    group(r.start as u64 + 1),
                    group(r.end as u64),
                    group(total as u64),
                    group(*page as u64 + 1),
                    group(pages as u64)
                ))
                .color(pal.text2),
            );
            if ui
                .add_enabled(!at_end, egui::Button::new(ph::CARET_RIGHT))
                .on_hover_text("Next page")
                .clicked()
            {
                *page += 1;
            }
            if ui
                .add_enabled(!at_end, egui::Button::new(ph::CARET_DOUBLE_RIGHT))
                .on_hover_text("Last page")
                .clicked()
            {
                *page = pages - 1;
            }
        });
    }
    range(page, total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_line_is_on_some_page() {
        let mut p = 0;
        assert_eq!(range(&mut p, 0), 0..0);
        assert_eq!(range(&mut p, 250), 0..100);
        p = 2;
        assert_eq!(range(&mut p, 250), 200..250);
        // a page past the end comes back to the last
        p = 9;
        assert_eq!(range(&mut p, 250), 200..250);
        assert_eq!(p, 2);
        p = 5;
        assert_eq!(range(&mut p, 100), 0..100);
    }
}
