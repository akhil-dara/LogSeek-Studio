//! Free space and the disk budget (own test binary: it sets an environment variable).

use logseek_core::sysmem::{disk_budget, disk_free, DISK_SAFETY};

#[test]
fn disk_free_reports_space_and_the_budget_keeps_a_reserve() {
    let d = tempfile::tempdir().unwrap();
    assert!(disk_free(d.path()).is_some_and(|f| f > 0));
    // a folder that does not exist yet is measured on its drive
    assert!(disk_free(&d.path().join("not/yet/made")).is_some());
    std::env::set_var("LOGSEEK_FAKE_FREE_MB", "500");
    assert_eq!(disk_free(d.path()), Some(500 << 20));
    assert_eq!(disk_budget(d.path(), None), (500 << 20) - DISK_SAFETY);
    assert_eq!(
        disk_budget(d.path(), Some(100 << 20)),
        100 << 20,
        "a user cap below the free space wins"
    );
    std::env::set_var("LOGSEEK_FAKE_FREE_MB", "150");
    assert_eq!(
        disk_budget(d.path(), None),
        0,
        "less than the 200 MB reserve: nothing may be written"
    );
    std::env::remove_var("LOGSEEK_FAKE_FREE_MB");
}
