//! Fonts: the platform UI face (Segoe UI on Windows) for interface text, a
//! real monospace (Cascadia Mono) for data, Phosphor for icons. Falls back to
//! egui's bundled fonts when a system face is missing.

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};
use std::sync::Arc;

fn load(paths: &[&str]) -> Option<Vec<u8>> {
    paths.iter().find_map(|p| std::fs::read(p).ok())
}

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let win = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let f = |name: &str| format!("{win}\\Fonts\\{name}");
    if let Some(ui) = load(&[
        &f("segoeui.ttf"),
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    ]) {
        fonts
            .font_data
            .insert("ui".into(), Arc::new(FontData::from_owned(ui)));
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "ui".into());
    }
    if let Some(mono) = load(&[
        &f("CascadiaMono.ttf"),
        &f("consola.ttf"),
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    ]) {
        fonts
            .font_data
            .insert("data".into(), Arc::new(FontData::from_owned(mono)));
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .insert(0, "data".into());
    }
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    // bold text = the bold face, then everything the regular family has
    // (text face, Phosphor icons, emoji fallback) so icons work in headings too
    let mut strong: Vec<String> = Vec::new();
    if let Some(ui_b) = load(&[&f("seguisb.ttf"), &f("segoeuib.ttf")]) {
        fonts
            .font_data
            .insert("ui-strong".into(), Arc::new(FontData::from_owned(ui_b)));
        strong.push("ui-strong".into());
    }
    strong.extend(fonts.families[&FontFamily::Proportional].iter().cloned());
    fonts
        .families
        .insert(FontFamily::Name("strong".into()), strong);
    ctx.set_fonts(fonts);
}

pub fn strong(px: f32) -> egui::FontId {
    egui::FontId::new(px, FontFamily::Name("strong".into()))
}
