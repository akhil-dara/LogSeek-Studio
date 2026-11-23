//! Tags: nine marks an analyst puts on rows with one key (1–9). Each has a
//! name, a colour *and* a distinct shape, so a tag never relies on colour
//! alone. Tags are saved next to the index (never in the evidence file),
//! written atomically so a crash cannot corrupt them.

use eframe::egui::{self, Color32, Pos2, Shape, Stroke};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Row id → bit mask of tags (bit 0 = tag 1 … bit 8 = tag 9).
pub type TagMap = BTreeMap<u64, u16>;

pub struct TagDef {
    pub dark: Color32,
    pub light: Color32,
}

const fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// Each tag's colours. Tags have no built-in meaning:
/// they are numbered until the user names them (see `name`).
pub const TAGS: [TagDef; 9] = [
    TagDef {
        dark: hex(0xF5B544),
        light: hex(0x8F5300),
    },
    TagDef {
        dark: hex(0xFF8079),
        light: hex(0xBE2F29),
    },
    TagDef {
        dark: hex(0x8DB6F2),
        light: hex(0x2B5CB0),
    },
    TagDef {
        dark: hex(0x63D394),
        light: hex(0x1D7442),
    },
    TagDef {
        dark: hex(0xC9A7FF),
        light: hex(0x6A3FB8),
    },
    TagDef {
        dark: hex(0x7FD8E0),
        light: hex(0x0F6E76),
    },
    TagDef {
        dark: hex(0xFFA862),
        light: hex(0x9A4E0C),
    },
    TagDef {
        dark: hex(0xF28DC0),
        light: hex(0xA12D6B),
    },
    TagDef {
        dark: hex(0xECE7DA),
        light: hex(0x1B1A18),
    },
];

/// The user's names for the tags (empty: numbered), set from the preferences.
static NAMES: std::sync::RwLock<Vec<String>> = std::sync::RwLock::new(Vec::new());

/// Tests that change the names take turns.
#[cfg(test)]
static NAMES_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Use these names for the tags (from the preferences; blanks keep the number).
pub fn set_names(names: &[String]) {
    *NAMES.write().unwrap_or_else(|e| e.into_inner()) = names.to_vec();
}

/// Tag `i`'s name: the user's, or "Tag 1" … "Tag 9".
pub fn name(i: usize) -> String {
    NAMES
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(i)
        .map(|n| n.trim())
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Tag {}", i + 1))
}

pub fn color(i: usize, dark: bool) -> Color32 {
    if dark {
        TAGS[i].dark
    } else {
        TAGS[i].light
    }
}

/// Draw tag `i`'s shape centred at `c` with radius `r`.
pub fn draw(p: &egui::Painter, i: usize, c: Pos2, r: f32, col: Color32) {
    let poly = |pts: Vec<Pos2>| Shape::convex_polygon(pts, col, Stroke::NONE);
    let v = |dx: f32, dy: f32| c + egui::vec2(dx, dy) * r;
    match i {
        0 => {
            p.circle_filled(c, r * 0.85, col);
        }
        1 => {
            p.add(poly(vec![v(0.0, -1.0), v(0.95, 0.75), v(-0.95, 0.75)]));
        }
        2 => {
            p.add(poly(vec![
                v(-0.78, -0.78),
                v(0.78, -0.78),
                v(0.78, 0.78),
                v(-0.78, 0.78),
            ]));
        }
        3 => {
            p.add(poly(vec![
                v(0.0, -1.0),
                v(1.0, 0.0),
                v(0.0, 1.0),
                v(-1.0, 0.0),
            ]));
        }
        4 => {
            // five-point star: two overlapping convex halves would leave gaps, so fan from the centre
            let pts: Vec<Pos2> = (0..10)
                .map(|k| {
                    let a = std::f32::consts::PI * (k as f32 / 5.0) - std::f32::consts::FRAC_PI_2;
                    let rr = if k % 2 == 0 { 1.0 } else { 0.45 };
                    v(a.cos() * rr, a.sin() * rr)
                })
                .collect();
            for k in 0..10 {
                p.add(poly(vec![c, pts[k], pts[(k + 1) % 10]]));
            }
        }
        5 => {
            let pts: Vec<Pos2> = (0..6)
                .map(|k| {
                    let a = std::f32::consts::PI / 3.0 * k as f32;
                    v(a.cos() * 0.92, a.sin() * 0.92)
                })
                .collect();
            p.add(poly(pts));
        }
        6 => {
            // plus
            let t = 0.32;
            p.add(poly(vec![
                v(-t, -0.95),
                v(t, -0.95),
                v(t, 0.95),
                v(-t, 0.95),
            ]));
            p.add(poly(vec![
                v(-0.95, -t),
                v(0.95, -t),
                v(0.95, t),
                v(-0.95, t),
            ]));
        }
        7 => {
            p.circle_stroke(c, r * 0.72, Stroke::new(r * 0.38, col));
        }
        _ => {
            // flag
            p.line_segment([v(-0.6, -0.95), v(-0.6, 0.95)], Stroke::new(r * 0.28, col));
            p.add(poly(vec![v(-0.6, -0.95), v(0.85, -0.55), v(-0.6, -0.1)]));
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
struct TagFile {
    version: u32,
    /// Size of the source when the tags were saved: a different size means the
    /// rows may not line up any more.
    source_bytes: u64,
    rows: BTreeMap<String, u16>,
}

pub fn file_for(store_dir: &Path) -> std::path::PathBuf {
    store_dir.join("tags.json")
}

/// (tags, saved against a different source size?)
pub fn load(store_dir: &Path, source_bytes: u64) -> (TagMap, bool) {
    let Ok(text) = std::fs::read_to_string(file_for(store_dir)) else {
        return (TagMap::new(), false);
    };
    let Ok(f) = serde_json::from_str::<TagFile>(&text) else {
        return (TagMap::new(), false);
    };
    let rows = f
        .rows
        .into_iter()
        .filter_map(|(k, v)| Some((k.parse().ok()?, v & 0x1FF)))
        .filter(|(_, v)| *v != 0)
        .collect();
    (rows, f.source_bytes != source_bytes)
}

/// tmp → flush → rename: either the old file or the new one, never half.
pub fn save(store_dir: &Path, source_bytes: u64, tags: &TagMap) -> std::io::Result<()> {
    std::fs::create_dir_all(store_dir)?;
    let f = TagFile {
        version: 1,
        source_bytes,
        rows: tags
            .iter()
            .filter(|(_, v)| **v != 0)
            .map(|(k, v)| (k.to_string(), *v))
            .collect(),
    };
    let tmp = store_dir.join("tags.json.tmp");
    {
        use std::io::Write;
        let mut w = std::fs::File::create(&tmp)?;
        w.write_all(
            serde_json::to_string(&f)
                .map_err(std::io::Error::other)?
                .as_bytes(),
        )?;
        w.sync_all()?;
    }
    std::fs::rename(&tmp, file_for(store_dir))
}

/// Toggle tag `i` (0-based) on `row`; returns the new mask.
pub fn toggle(tags: &mut TagMap, row: u64, i: usize) -> u16 {
    let m = tags.entry(row).or_insert(0);
    *m ^= 1 << i;
    let out = *m;
    if out == 0 {
        tags.remove(&row);
    }
    out
}

/// Rows carrying any tag in `mask` (all tagged rows when mask = 0x1FF).
pub fn rows_with(tags: &TagMap, mask: u16) -> Vec<u64> {
    tags.iter()
        .filter(|(_, m)| **m & mask != 0)
        .map(|(r, _)| *r)
        .collect()
}

/// Rows per tag.
pub fn counts(tags: &TagMap) -> [u64; 9] {
    let mut c = [0u64; 9];
    for m in tags.values() {
        for (i, n) in c.iter_mut().enumerate() {
            if m >> i & 1 == 1 {
                *n += 1;
            }
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_named_by_number_until_the_user_names_them() {
        let _g = NAMES_TEST.lock().unwrap_or_else(|e| e.into_inner());
        set_names(&[]);
        assert_eq!(name(0), "Tag 1");
        assert_eq!(name(8), "Tag 9");
        // the user's own words, kept; an emptied name goes back to its number
        set_names(&["Seen before".into(), "  ".into(), "Exfil".into()]);
        assert_eq!(name(0), "Seen before");
        assert_eq!(name(1), "Tag 2");
        assert_eq!(name(2), "Exfil");
        assert_eq!(name(3), "Tag 4");
        set_names(&[]);
    }

    #[test]
    fn toggling_and_counting() {
        let mut t = TagMap::new();
        assert_eq!(toggle(&mut t, 7, 1), 0b10);
        assert_eq!(toggle(&mut t, 7, 0), 0b11);
        toggle(&mut t, 9, 1);
        assert_eq!(counts(&t)[..2], [1, 2]);
        assert_eq!(rows_with(&t, 0b10), vec![7, 9]);
        toggle(&mut t, 7, 0);
        toggle(&mut t, 7, 1);
        assert!(!t.contains_key(&7), "untagged rows leave no trace");
    }

    #[test]
    fn tags_survive_a_save_and_reload_and_flag_a_changed_source() {
        let d = std::env::temp_dir().join(format!("logseek-tags-{}", std::process::id()));
        let mut t = TagMap::new();
        toggle(&mut t, 42, 4);
        toggle(&mut t, 1_000_000_007, 8);
        save(&d, 1234, &t).unwrap();
        let (back, changed) = load(&d, 1234);
        assert_eq!(back, t);
        assert!(!changed);
        let (_, changed) = load(&d, 999);
        assert!(changed, "a different source size is reported");
        assert!(!d.join("tags.json.tmp").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn tag_colours_meet_contrast_on_both_themes() {
        for i in 0..9 {
            assert!(
                crate::theme::contrast(color(i, true), crate::theme::DARK.bg) >= 3.0,
                "dark tag {i}"
            );
            assert!(
                crate::theme::contrast(color(i, false), crate::theme::LIGHT.bg) >= 3.0,
                "light tag {i}"
            );
        }
    }
}
