//! Puts the program's icon and version details into the Windows exe, so
//! Explorer, the taskbar and the installer show them.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/brand/logseek.ico");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/brand/logseek.ico");
        res.set("ProductName", "LogSeek Studio");
        res.set("FileDescription", "LogSeek Studio");
        res.set(
            "LegalCopyright",
            "Copyright (C) 2026 LogSeek Studio contributors. Licensed under AGPL-3.0-or-later.",
        );
        if let Err(e) = res.compile() {
            // the program still builds without its icon (no resource compiler found)
            println!("cargo:warning=the exe icon was not embedded: {e}");
        }
    }
}
