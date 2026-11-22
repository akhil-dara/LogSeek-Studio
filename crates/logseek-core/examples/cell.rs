//! `cargo run --example cell -- <store> <row> <col>`: rendered cell vs raw source line.
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let st = logseek_core::store::Store::open(std::path::Path::new(&a[1])).unwrap();
    let (r, c): (u64, usize) = (a[2].parse().unwrap(), a[3].parse().unwrap());
    println!("cell: {:?}", st.cell_text(r, c).unwrap());
    let raw = st.raw_line(r).unwrap();
    println!(
        "raw : {}",
        String::from_utf8_lossy(&raw[..raw.len().min(160)])
    );
}
