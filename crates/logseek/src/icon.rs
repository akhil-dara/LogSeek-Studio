//! The LogSeek mark: an amber magnifier over log lines (embedded from
//! assets/brand). Shown as a texture so the app, the
//! window icon and the brand files are the same picture.

use eframe::egui;

/// Upload the mark once; draw it with `ui.image((tex.id(), size))`.
pub fn logo_texture(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let png = include_bytes!("../../../assets/brand/logseek-icon-64.png");
    let icon = eframe::icon_data::from_png_bytes(png).ok()?;
    let img = egui::ColorImage::from_rgba_unmultiplied(
        [icon.width as usize, icon.height as usize],
        &icon.rgba,
    );
    Some(ctx.load_texture("logseek-mark", img, egui::TextureOptions::LINEAR))
}

/// The window/taskbar icon from the shipped brand raster.
pub fn window_icon() -> egui::IconData {
    let png = include_bytes!("../../../assets/brand/logseek-icon-256.png");
    eframe::icon_data::from_png_bytes(png).unwrap_or_else(|_| egui::IconData {
        rgba: vec![0xF5, 0xB5, 0x44, 0xFF].repeat(16 * 16),
        width: 16,
        height: 16,
    })
}
