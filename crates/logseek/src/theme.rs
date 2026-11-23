//! Time spine tokens for the dark (warm graphite) and light (bone) themes,
//! and their mapping onto egui visuals.

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub bg: Color32,
    pub surface: Color32,
    pub surface2: Color32,
    pub selection: Color32,
    pub hairline: Color32,
    pub control_border: Color32,
    pub text: Color32,
    pub text2: Color32,
    pub muted: Color32,
    /// The teal signal: the busiest-stretch ring, selection bar, primary button.
    pub accent: Color32,
    /// Amber used as text/lines on the background (dark: same; light: ink).
    pub accent_ink: Color32,
    pub on_accent: Color32,
    pub focus: Color32,
    pub info: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
    pub star: Color32,
}

const fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// Time spine, dark: warm graphite with a teal signal (time, selection,
/// focus, the brushed period); amber only ever means "warning".
pub const DARK: Palette = Palette {
    dark: true,
    bg: hex(0x161514),
    surface: hex(0x1D1C1A),
    surface2: hex(0x262421),
    selection: hex(0x1F3A36),
    hairline: hex(0x34312D),
    control_border: hex(0x8A8478),
    text: hex(0xECE8E1),
    text2: hex(0xBDB6AA),
    muted: hex(0xABA498),
    accent: hex(0x3FC1B0),
    accent_ink: hex(0x3FC1B0),
    on_accent: hex(0x08201C),
    focus: hex(0x3FC1B0),
    // data bars and quiet-gap lines: the teal ink (no colour outside the palette)
    info: hex(0x3FC1B0),
    success: hex(0x63D394),
    warning: hex(0xF0A04B),
    danger: hex(0xFF8079),
    star: hex(0xECE8E1),
};

/// Time spine, light: bone ground, white surfaces, ink text, teal ink.
pub const LIGHT: Palette = Palette {
    dark: false,
    bg: hex(0xF4F3F1),
    surface: hex(0xFFFFFF),
    surface2: hex(0xECEAE6),
    selection: hex(0xD8EFEB),
    hairline: hex(0xD9D5CE),
    control_border: hex(0x7A746A),
    text: hex(0x1B1A18),
    text2: hex(0x45413A),
    muted: hex(0x5F5A51),
    accent: hex(0x3FC1B0),
    accent_ink: hex(0x00695F),
    on_accent: hex(0x08201C),
    focus: hex(0x00695F),
    info: hex(0x00695F),
    success: hex(0x1D7442),
    warning: hex(0x9A4E0C),
    danger: hex(0xBE2F29),
    star: hex(0x1B1A18),
};

pub const ROW_H: f32 = 24.0;
pub const HEADER_H: f32 = 24.0;
pub const TOP_H: f32 = 40.0;
pub const STATUS_H: f32 = 24.0;
pub const UI_PX: f32 = 13.0;
pub const DATA_PX: f32 = 12.5;
pub const ZOOM_MIN: f32 = 0.8;
pub const ZOOM_MAX: f32 = 2.0;

pub fn data_font() -> FontId {
    FontId::new(DATA_PX, FontFamily::Monospace)
}
pub fn ui_font() -> FontId {
    FontId::new(UI_PX, FontFamily::Proportional)
}
pub fn small_font() -> FontId {
    FontId::new(12.0, FontFamily::Proportional)
}

pub fn apply(ctx: &egui::Context, p: &Palette) {
    let mut v = if p.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.override_text_color = Some(p.text);
    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.extreme_bg_color = p.surface;
    v.faint_bg_color = p.surface2;
    v.code_bg_color = p.surface2;
    // one link colour everywhere: the teal ink
    v.hyperlink_color = p.accent_ink;
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.danger;
    v.selection.bg_fill = p.selection;
    v.selection.stroke = Stroke::new(1.0, p.accent_ink);
    v.text_cursor.stroke = Stroke::new(2.0, p.accent_ink);
    v.window_stroke = Stroke::new(1.0, p.hairline);
    v.window_corner_radius = CornerRadius::same(6);
    v.menu_corner_radius = CornerRadius::same(6);
    v.window_shadow = egui::epaint::Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(if p.dark { 110 } else { 40 }),
    };
    v.popup_shadow = egui::epaint::Shadow {
        offset: [0, 4],
        blur: 12,
        spread: 0,
        color: Color32::from_black_alpha(if p.dark { 100 } else { 36 }),
    };
    let r = CornerRadius::same(4);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.hairline);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text2);
    w.noninteractive.corner_radius = r;
    w.inactive.bg_fill = p.surface;
    w.inactive.weak_bg_fill = p.surface;
    w.inactive.bg_stroke = Stroke::new(1.0, p.control_border);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = r;
    w.hovered.bg_fill = p.surface2;
    w.hovered.weak_bg_fill = p.surface2;
    w.hovered.bg_stroke = Stroke::new(1.0, p.text2);
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;
    w.active.bg_fill = p.selection;
    w.active.weak_bg_fill = p.selection;
    w.active.bg_stroke = Stroke::new(2.0, p.focus);
    w.active.fg_stroke = Stroke::new(1.0, p.text);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;
    w.open = w.active;
    // window title bars and open menus on graphite: teal stays for the signal
    w.open.weak_bg_fill = p.surface2;
    let th = if p.dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    };
    ctx.set_theme(th);
    ctx.style_mut_of(th, |s| {
        s.visuals = v;
        s.text_styles = [
            (TextStyle::Small, small_font()),
            (TextStyle::Body, ui_font()),
            (TextStyle::Button, ui_font()),
            (
                TextStyle::Heading,
                FontId::new(22.0, FontFamily::Proportional),
            ),
            (TextStyle::Monospace, data_font()),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
        s.spacing.interact_size = egui::vec2(24.0, 24.0);
        // scrollbars always visible when there is more to see (never only on
        // hover): a 13-inch screen often has more fields than fit
        s.spacing.scroll = egui::style::ScrollStyle {
            floating: false,
            bar_width: 10.0,
            // easy to grab on a trackpad, even with 900 fields
            handle_min_length: 28.0,
            foreground_color: true,
            dormant_background_opacity: 0.0,
            dormant_handle_opacity: 0.55,
            active_background_opacity: 0.35,
            active_handle_opacity: 0.8,
            interact_background_opacity: 0.5,
            interact_handle_opacity: 1.0,
            // no fade at the edges: the scrollbar says there is more, and
            // text is never drawn faded
            fade: egui::style::ScrollFadeStyle {
                strength: 0.0,
                ..Default::default()
            },
            ..egui::style::ScrollStyle::solid()
        };
        s.interaction.selectable_labels = false;
    });
}

/// Colours of the time spine. `window`, `gap_band` and `gap_text` are
/// checked for contrast by the tests; the spine does not draw them yet.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct SpineColors {
    /// Bars of how many rows each moment holds.
    pub density: Color32,
    /// The bracket showing which moments the visible rows cover.
    pub window: Color32,
    /// The brushed (selected) period.
    pub brush: Color32,
    /// Quiet-gap bands between rows, and their label.
    pub gap_band: Color32,
    pub gap_text: Color32,
}

pub fn spine_colors(p: &Palette) -> SpineColors {
    SpineColors {
        density: if p.dark { hex(0x2F8F84) } else { hex(0x2E8A80) },
        window: p.text2,
        brush: p.accent_ink,
        gap_band: p.surface2,
        gap_text: p.text2,
    }
}

/// Values a machine wrote (times, numbers, addresses, hashes, ids, ports)
/// read in the mono face, where alignment means something; words people
/// wrote read in the interface face. Decided from the values themselves,
/// never from the column's name.
pub fn is_machine_value(ty: logseek_core::types::ColType, values: &[&str]) -> bool {
    use logseek_core::types::ColType;
    matches!(ty, ColType::Time | ColType::Int | ColType::Float) || machine_written(values)
}

/// One value looks machine-written: no spaces, and either made only of hex
/// digits and separators (addresses, hashes, GUIDs, MACs) or at least half
/// digits and punctuation (ports, ids, versions).
fn value_machine_written(v: &str) -> bool {
    let v = v.trim();
    if v.is_empty() || v.chars().any(char::is_whitespace) {
        return false;
    }
    let sep = |c: char| matches!(c, ':' | '.' | '-' | '_' | '/' | '{' | '}' | '=' | '+');
    if v.chars().all(|c| c.is_ascii_hexdigit() || sep(c)) && v.chars().any(|c| c.is_ascii_digit()) {
        return true;
    }
    let n = v.chars().count();
    let digits_punct = v
        .chars()
        .filter(|c| c.is_ascii_digit() || c.is_ascii_punctuation())
        .count();
    digits_punct * 2 >= n
}

/// Most of a column's values (at least four in five of those with a value,
/// and at least three) look machine-written.
pub fn machine_written(values: &[&str]) -> bool {
    let filled: Vec<&&str> = values.iter().filter(|v| !v.trim().is_empty()).collect();
    if filled.len() < 3 {
        return false;
    }
    let m = filled.iter().filter(|v| value_machine_written(v)).count();
    m * 5 >= filled.len() * 4
}

/// Scrollbar colours: (track, thumb, thumb while hovered or dragged).
pub fn scroll_colors(p: &Palette) -> (Color32, Color32, Color32) {
    (p.surface2, p.control_border, p.text2)
}

/// WCAG 2.x relative luminance.
#[cfg_attr(not(test), allow(dead_code))]
pub fn luminance(c: Color32) -> f64 {
    let ch = |v: u8| {
        let s = v as f64 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn contrast(a: Color32, b: Color32) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    let (hi, lo) = if x > y { (x, y) } else { (y, x) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hue(c: Color32) -> f64 {
        let (r, g, b) = (
            c.r() as f64 / 255.0,
            c.g() as f64 / 255.0,
            c.b() as f64 / 255.0,
        );
        let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
        if mx == mn {
            return 0.0;
        }
        let d = mx - mn;
        let h = if mx == r {
            ((g - b) / d).rem_euclid(6.0)
        } else if mx == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        h * 60.0
    }

    #[test]
    fn accent_is_teal_not_amber() {
        for p in [&DARK, &LIGHT] {
            for c in [p.accent, p.accent_ink, p.focus] {
                let h = hue(c);
                assert!((160.0..=190.0).contains(&h), "hue {h:.0} (dark {})", p.dark);
            }
        }
    }

    #[test]
    fn spine_colours_meet_contrast() {
        for p in [&DARK, &LIGHT] {
            let s = spine_colors(p);
            for (c, what) in [
                (s.density, "density"),
                (s.window, "window"),
                (s.brush, "brush"),
            ] {
                assert!(
                    contrast(c, p.bg) >= 3.0,
                    "{what} {:.2} (dark {})",
                    contrast(c, p.bg),
                    p.dark
                );
            }
            assert!(
                contrast(s.gap_text, s.gap_band) >= 4.5,
                "gap label (dark {})",
                p.dark
            );
        }
    }

    /// WCAG 2.2 AA across the whole UI: every text colour on every surface it
    /// is drawn on (4.5:1), and every line or control edge that carries meaning
    /// (3:1) — the row-number band, pinned edge, selection, hints, buttons.
    #[test]
    fn every_pair_meets_wcag_aa() {
        let mut bad = Vec::new();
        for p in [&DARK, &LIGHT] {
            let grounds = [
                (p.bg, "bg"),
                (p.surface, "surface"),
                (p.surface2, "surface2"),
                (p.selection, "selection"),
            ];
            let texts = [
                (p.text, "text"),
                (p.text2, "text2"),
                (p.muted, "muted"),
                (p.accent_ink, "accent_ink"),
                (p.info, "info"),
                (p.danger, "danger"),
                (p.warning, "warning"),
                (p.success, "success"),
            ];
            for (g, gn) in grounds {
                for (t, tn) in texts {
                    let c = contrast(t, g);
                    if c < 4.5 {
                        bad.push(format!(
                            "{} {tn} on {gn}: {c:.2}",
                            if p.dark { "dark" } else { "light" }
                        ));
                    }
                }
                for (l, ln) in [(p.control_border, "control_border"), (p.focus, "focus")] {
                    let c = contrast(l, g);
                    if c < 3.0 {
                        bad.push(format!(
                            "{} {ln} on {gn}: {c:.2}",
                            if p.dark { "dark" } else { "light" }
                        ));
                    }
                }
            }
            let c = contrast(p.on_accent, p.accent);
            if c < 4.5 {
                bad.push(format!(
                    "{} on_accent on accent: {c:.2}",
                    if p.dark { "dark" } else { "light" }
                ));
            }
        }
        assert!(
            bad.is_empty(),
            "below WCAG AA:
{}",
            bad.join(
                "
"
            )
        );
    }

    #[test]
    fn machine_written_values_use_mono_whatever_the_column_is_called() {
        use logseek_core::types::ColType;
        // times and numbers always
        assert!(is_machine_value(ColType::Int, &[]));
        assert!(is_machine_value(ColType::Time, &[]));
        // addresses, hashes, GUIDs, ports, ids: by their values
        for vals in [
            vec!["10.0.0.1", "192.168.4.20", "8.8.8.8"],
            vec![
                "d41d8cd98f00b204e9800998ecf8427e",
                "9e107d9d372bb6826bd81d3542a419d6",
                "e4d909c290d0fb1ca068ffaddf22cbd0",
            ],
            vec![
                "{0E3E1A3B-1C2D-4E5F-8A9B-0C1D2E3F4A5B}",
                "{7A1B2C3D-4E5F-6A7B-8C9D-0E1F2A3B4C5D}",
                "{11111111-2222-3333-4444-555555555555}",
            ],
            vec!["443", "8080", "53", ""],
            vec!["fe80::1", "00:1A:2B:3C:4D:5E", "2001:db8::7"],
        ] {
            assert!(is_machine_value(ColType::Text, &vals), "{vals:?}");
        }
        // words people wrote, names, sentences, paths with words: interface face
        for vals in [
            vec!["user logged on", "service started", "disk is full"],
            vec!["chrome.exe", "svchost.exe", "explorer.exe"],
            vec![
                "S-1-5-18 (systemprofile)",
                "S-1-5-19 (localService)",
                "S-1-5-20 (networkService)",
            ],
            vec!["alice", "bob", "carol"],
        ] {
            assert!(!is_machine_value(ColType::Text, &vals), "{vals:?}");
        }
        // too few values to judge: the interface face
        assert!(!machine_written(&["10.0.0.1", ""]));
    }
    #[test]
    fn scrollbars_are_visible_in_both_themes() {
        for p in [&DARK, &LIGHT] {
            let (track, thumb, hot) = scroll_colors(p);
            for (c, what) in [(thumb, "thumb"), (hot, "hot thumb")] {
                assert!(
                    contrast(c, track) >= 3.0,
                    "{what} on track {:.2} (dark {})",
                    contrast(c, track),
                    p.dark
                );
                assert!(
                    contrast(c, p.bg) >= 3.0,
                    "{what} on bg {:.2} (dark {})",
                    contrast(c, p.bg),
                    p.dark
                );
                assert!(
                    contrast(c, p.surface) >= 3.0,
                    "{what} on surface {:.2} (dark {})",
                    contrast(c, p.surface),
                    p.dark
                );
            }
        }
    }

    #[test]
    fn text_tokens_meet_wcag_aa_on_every_surface_in_both_themes() {
        for p in [DARK, LIGHT] {
            for surf in [p.bg, p.surface, p.surface2, p.selection] {
                for (name, t) in [
                    ("text", p.text),
                    ("text2", p.text2),
                    ("muted", p.muted),
                    ("accent_ink", p.accent_ink),
                    ("info", p.info),
                    ("danger", p.danger),
                    ("success", p.success),
                    ("warning", p.warning),
                ] {
                    let r = contrast(t, surf);
                    assert!(
                        r >= 4.5,
                        "{} {name} on {surf:?}: {r:.2}",
                        if p.dark { "dark" } else { "light" }
                    );
                }
            }
            assert!(contrast(p.on_accent, p.accent) >= 4.5, "text on amber");
        }
    }

    #[test]
    fn controls_and_focus_meet_3_to_1() {
        for p in [DARK, LIGHT] {
            for surf in [p.bg, p.surface] {
                assert!(contrast(p.control_border, surf) >= 3.0);
                assert!(contrast(p.focus, surf) >= 3.0);
            }
        }
    }
}
