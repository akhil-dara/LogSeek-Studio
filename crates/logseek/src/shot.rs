//! Developer switch for visual checks: `LOGSEEK_SHOT=<file.png>` saves the
//! frame the app itself drew (after `LOGSEEK_SHOT_AFTER` seconds, default 4)
//! and exits. Windows cannot reliably copy an OpenGL window from outside.

use eframe::egui;
use std::io::Write;
use std::time::Instant;

pub struct Shot {
    path: Option<String>,
    after: f64,
    started: Instant,
    requested: bool,
}

impl Shot {
    pub fn from_env() -> Shot {
        Shot {
            path: crate::dev::var("LOGSEEK_SHOT").ok(),
            after: crate::dev::var("LOGSEEK_SHOT_AFTER")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(4.0),
            started: Instant::now(),
            requested: false,
        }
    }

    pub fn tick(&mut self, ctx: &egui::Context) {
        let Some(path) = self.path.clone() else {
            return;
        };
        // LOGSEEK_SHOT_SIZE="1366x768": the window at an exact size (13-inch checks)
        if let Ok(v) = crate::dev::var("LOGSEEK_SHOT_SIZE") {
            std::env::remove_var("LOGSEEK_SHOT_SIZE");
            if let Some((w, h)) = v.split_once('x') {
                if let (Ok(w), Ok(h)) = (w.parse::<f32>(), h.parse::<f32>()) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w, h)));
                }
            }
        }
        if !self.requested && self.started.elapsed().as_secs_f64() >= self.after {
            self.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        let img = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = img {
            let _ = write_png(&path, img.size[0], img.size[1], &img.pixels);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// Minimal RGBA PNG writer (zlib via flate2, CRC-32 by hand).
fn write_png(path: &str, w: usize, h: usize, px: &[egui::Color32]) -> std::io::Result<()> {
    let mut raw = Vec::with_capacity(h * (w * 4 + 1));
    for y in 0..h {
        raw.push(0u8);
        for p in &px[y * w..(y + 1) * w] {
            let [r, g, b, a] = p.to_srgba_unmultiplied();
            raw.extend_from_slice(&[r, g, b, a]);
        }
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    z.write_all(&raw)?;
    let idat = z.finish()?;
    let mut f = std::fs::File::create(path)?;
    f.write_all(b"\x89PNG\r\n\x1a\n")?;
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    for (kind, data) in [
        (&b"IHDR"[..], &ihdr[..]),
        (b"IDAT", &idat[..]),
        (b"IEND", &[][..]),
    ] {
        f.write_all(&(data.len() as u32).to_be_bytes())?;
        let mut c = Vec::with_capacity(4 + data.len());
        c.extend_from_slice(kind);
        c.extend_from_slice(data);
        f.write_all(&c)?;
        f.write_all(&crc32(&c).to_be_bytes())?;
    }
    Ok(())
}

fn crc32(d: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in d {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

#[cfg(test)]
mod tests {
    #[test]
    fn crc_matches_reference() {
        assert_eq!(super::crc32(b"IEND"), 0xAE42_6082);
    }
}
