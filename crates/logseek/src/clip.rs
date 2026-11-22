//! Copying to the clipboard, always said: every copy in the app goes through
//! `copy`, which remembers what was copied so a notice above the status line
//! can say it ("Copied the value of user (12 characters)"). A copy that says
//! nothing looks like a copy that did not happen.

use eframe::egui;

fn key() -> egui::Id {
    egui::Id::new("just-copied")
}

/// Put `text` on the clipboard; `what` says what it is ("the value of user").
pub fn copy(ctx: &egui::Context, text: String, what: &str) {
    let n = text.chars().count();
    ctx.copy_text(text);
    let now = ctx.input(|i| i.time);
    ctx.data_mut(|d| d.insert_temp(key(), (words(what, n), now)));
}

/// The words for a copy: what it was and how long.
pub fn words(what: &str, chars: usize) -> String {
    let len = if chars == 1 {
        "1 character".to_string()
    } else {
        format!("{} characters", crate::moment::group(chars as u64))
    };
    format!("Copied {what} ({len})")
}

/// The last copy's words and when it was made (UI time), if any.
pub fn last(ctx: &egui::Context) -> Option<(String, f64)> {
    ctx.data(|d| d.get_temp::<(String, f64)>(key()))
}

/// Forget the last copy (its notice was dismissed or has run its time).
pub fn forget(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<(String, f64)>(key()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_is_always_said_with_what_it_was() {
        assert_eq!(
            words("the value of user", 12),
            "Copied the value of user (12 characters)"
        );
        assert_eq!(
            words("the column name", 1),
            "Copied the column name (1 character)"
        );
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            copy(ui.ctx(), "alice".into(), "the value of user");
        });
        out.textures_delta.clear();
        // the text went to the clipboard, and the copy is remembered for its notice
        let copied = out
            .platform_output
            .commands
            .iter()
            .any(|c| matches!(c, egui::OutputCommand::CopyText(t) if t == "alice"));
        assert!(copied);
        assert_eq!(
            last(&ctx).map(|l| l.0),
            Some("Copied the value of user (5 characters)".into())
        );
        forget(&ctx);
        assert!(last(&ctx).is_none());
    }
}
