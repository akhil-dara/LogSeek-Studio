//! LogSeek Studio — native desktop log explorer (egui).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod addcol;
mod app;
mod book;
mod bridge;
mod builder;
mod calendar;
mod cases;
mod check;
mod clip;
mod colchooser;
mod colors;
mod colpick;
mod coverage;
mod dev;
mod columnsview;
mod conditions;
mod explainwin;
mod export;
mod fields;
mod fileset;
mod fillscreen;
mod filters;
mod follow;
mod followtree;
mod fonts;
mod found;
mod grid;
mod groups;
mod icon;
mod importnote;
mod inspect;
mod keep;
mod listwin;
mod matchopts;
mod meaning;
mod moment;
mod pager;
mod pairwin;
mod palette;
mod patternwin;
mod plainline;
mod querylines;
mod rawcols;
mod recipes;
mod rhythmwin;
mod rowsel;
mod searchwin;
mod shapewin;
mod sheet;
mod shot;
mod spine;
mod strangewin;
mod stress;
mod summary;
mod tabs;
mod tags;
mod theme;
mod unusualwin;
mod viewquery;
mod workspace;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("LogSeek Studio")
            .with_app_id("LogSeek")
            // every start fills the screen: the rows need the room. No size
            // is given with it: a size set on a window that is already
            // maximised leaves it marked maximised at that size
            .with_maximized(true)
            .with_min_inner_size([960.0, 600.0])
            .with_icon(icon::window_icon()),
        persist_window: false,
        // dev switch: LOGSEEK_NO_VSYNC=1 measures frame cost without display pacing
        glow_options: eframe::egui_glow::GlowConfiguration {
            vsync: crate::dev::var_os("LOGSEEK_NO_VSYNC").is_none(),
            ..Default::default()
        },
        ..Default::default()
    };
    eframe::run_native(
        "LogSeek Studio",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
