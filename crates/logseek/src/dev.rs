//! Developer switches: environment variables that script the app for
//! screenshots, stress runs and tests (`LOGSEEK_*`).
//!
//! They are read only in debug builds, or in a build made with the
//! `devtools` feature. A release build ignores every one of them, so nothing
//! in a user's environment can skip a check, move the indexes or change what
//! the app believes about the disk.

/// Whether this build reads the developer switches.
pub const ON: bool = cfg!(any(debug_assertions, feature = "devtools"));

/// The value of a developer switch, when this build reads them.
pub fn var(name: &str) -> Result<String, std::env::VarError> {
    if ON {
        std::env::var(name)
    } else {
        Err(std::env::VarError::NotPresent)
    }
}

/// As [`var`], for values that need not be text.
pub fn var_os(name: &str) -> Option<std::ffi::OsString> {
    if ON {
        std::env::var_os(name)
    } else {
        None
    }
}
