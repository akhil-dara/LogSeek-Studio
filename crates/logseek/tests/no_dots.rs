//! The UI never shows "…" (the user reads dots as redacted text): no string
//! literal in the app or the engine may contain an ellipsis.

use std::path::Path;

fn check_dir(dir: &Path, bad: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            check_dir(&p, bad);
        } else if p.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&p).unwrap();
            for (n, line) in text.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                // look only inside quoted text, before any trailing comment
                let code = match code.find(" // ") {
                    Some(i) => &code[..i],
                    None => code,
                };
                let in_quotes = code
                    .split('"')
                    .skip(1)
                    .step_by(2)
                    .any(|q| q.contains('…') || q.contains("..."));
                if in_quotes {
                    bad.push(format!("{}:{}: {}", p.display(), n + 1, line.trim()));
                }
            }
        }
    }
}

#[test]
fn no_ellipsis_in_any_text_the_user_can_see() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut bad = Vec::new();
    check_dir(&root.join("src"), &mut bad);
    check_dir(&root.join("../logseek-core/src"), &mut bad);
    assert!(bad.is_empty(), "dots in UI text:\n{}", bad.join("\n"));
}
