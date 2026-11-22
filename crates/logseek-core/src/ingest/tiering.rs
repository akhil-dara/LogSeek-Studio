//! Which columns an index keeps when the disk budget cannot hold them all.
//! The rest are read from the source (see `rebuild`), so every row stays
//! searchable; cheap columns (small dictionaries, numbers, times) are kept
//! first, heavy free text goes to the source first.

/// Pick the columns to keep for the rest of the file.
///
/// `cols`: (column id, bytes it takes in this segment); `always`: kept
/// whatever it costs (row offsets, the time column); `fixed`: bytes every
/// segment writes regardless (header, Bloom filters); `seg_src`: source bytes
/// this segment covers; `remaining_src`: source bytes from this segment to the
/// end; `remaining_budget`: disk bytes still allowed. The projection scales
/// this segment's costs to the rest of the file.
pub fn choose_kept(
    cols: &[(u32, u64)],
    always: &[u32],
    fixed: u64,
    seg_src: u64,
    remaining_src: u64,
    remaining_budget: u64,
) -> Vec<u32> {
    let scale = remaining_src.max(seg_src) as f64 / seg_src.max(1) as f64;
    let need = |bytes: u64| (fixed + bytes) as f64 * scale;
    let all: u64 = cols.iter().map(|c| c.1).sum();
    let mut ids: Vec<u32> = if need(all) <= remaining_budget as f64 {
        cols.iter().map(|c| c.0).collect()
    } else {
        let mut kept: Vec<u32> = cols
            .iter()
            .filter(|c| always.contains(&c.0))
            .map(|c| c.0)
            .collect();
        let mut used: u64 = cols
            .iter()
            .filter(|c| always.contains(&c.0))
            .map(|c| c.1)
            .sum();
        let mut rest: Vec<(u32, u64)> = cols
            .iter()
            .filter(|c| !always.contains(&c.0))
            .copied()
            .collect();
        rest.sort_by_key(|c| (c.1, c.0));
        for (id, b) in rest {
            if need(used + b) <= remaining_budget as f64 {
                used += b;
                kept.push(id);
            }
        }
        kept
    };
    ids.sort_unstable();
    ids
}
