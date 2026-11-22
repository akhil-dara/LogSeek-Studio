//! Sort keys and the external sorter.
use logseek_core::query::sort::{key_for, Key, Sorter};
use logseek_core::types::ColType;

fn sorted(vals: &[Option<&str>], ty: ColType, desc: bool, run_bytes: usize) -> Vec<u64> {
    let d = tempfile::tempdir().unwrap();
    let mut s = Sorter::new(d.path(), run_bytes, desc);
    for (i, v) in vals.iter().enumerate() {
        s.push(key_for(ty, *v), i as u64).unwrap();
    }
    let o = s.finish().unwrap();
    assert_eq!(o.len(), vals.len() as u64);
    o.page(0, vals.len() + 5)
}

#[test]
fn numbers_sort_as_numbers_and_empty_values_go_last_either_way() {
    let v = [Some("10"), Some("2"), None, Some("33"), Some(""), Some("2")];
    // 2 (row 1), 2 (row 5), 10, 33, then the empty ones in file order
    assert_eq!(sorted(&v, ColType::Int, false, 1 << 20), [1, 5, 0, 3, 2, 4]);
    assert_eq!(sorted(&v, ColType::Int, true, 1 << 20), [3, 0, 1, 5, 2, 4]);
}

#[test]
fn text_sorts_without_regard_to_capitals() {
    let v = [Some("banana"), Some("Apple"), Some("cherry"), Some("apple")];
    assert_eq!(sorted(&v, ColType::Text, false, 1 << 20), [1, 3, 0, 2]);
}

#[test]
fn times_sort_by_when_they_happened() {
    let v = [
        Some("2026-11-12 10:00:05"),
        Some("2026-11-12T09:59:59Z"),
        Some("2026-11-13 00:00:00"),
    ];
    assert_eq!(sorted(&v, ColType::Time, false, 1 << 20), [1, 0, 2]);
}

#[test]
fn a_sort_too_big_for_memory_runs_on_disk_and_matches() {
    // 50,000 values, a 64 KB memory allowance: many runs, merged
    let vals: Vec<String> = (0..50_000u64)
        .map(|i| format!("v{:05}", (i * 7919) % 50_000))
        .collect();
    let opt: Vec<Option<&str>> = vals.iter().map(|s| Some(s.as_str())).collect();
    let got = sorted(&opt, ColType::Text, false, 64 << 10);
    let mut want: Vec<u64> = (0..50_000).collect();
    want.sort_by(|&a, &b| vals[a as usize].cmp(&vals[b as usize]).then(a.cmp(&b)));
    assert_eq!(got, want);
    // and reads by page from anywhere
    let d = tempfile::tempdir().unwrap();
    let mut s = Sorter::new(d.path(), 64 << 10, true);
    for (i, v) in vals.iter().enumerate() {
        s.push(key_for(ColType::Text, Some(v)), i as u64).unwrap();
    }
    let o = s.finish().unwrap();
    let mid = o.page(25_000, 3);
    let rev: Vec<u64> = want.iter().rev().copied().collect();
    assert_eq!(mid, &rev[25_000..25_003]);
    assert!(o.page(50_000, 10).is_empty());
}

#[test]
fn a_number_column_with_stray_text_keeps_every_row() {
    let v = [Some("7"), Some("n/a"), Some("3")];
    assert_eq!(sorted(&v, ColType::Int, false, 1 << 20), [2, 0, 1]);
    assert!(matches!(key_for(ColType::Int, Some("n/a")), Key::Text(_)));
}
