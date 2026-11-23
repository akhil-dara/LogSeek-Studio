//! Prints whether an index is stale against its source file.
use logseek_core::store::{Fingerprint, Store};
fn main() {
    let st = Store::open(std::path::Path::new(&std::env::args().nth(1).unwrap())).unwrap();
    let m = st.manifest();
    println!("source {:?}", m.source);
    println!("stored fp {:?}", m.fingerprint);
    println!("now    fp {:?}", Fingerprint::of(&m.source));
    println!("stale {}", st.is_stale());
    println!("open {:?}", st.source_file().map(|_| "ok"));
    let s0 = st.segments()[0].meta();
    println!(
        "seg0 bytes {}..{} rows {}",
        s0.byte_start, s0.byte_end, s0.rows
    );
}
