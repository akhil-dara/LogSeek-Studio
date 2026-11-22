//! Trigram Bloom filter: lets substring search skip a segment's column when
//! some trigram of the (ASCII-lowercased) needle never occurs in it.
//! No posting lists — the build cost is about one hash per input byte.

use xxhash_rust::xxh3::xxh3_64;

const K: u32 = 7;
const MIN_BITS: usize = 512;
const MAX_BITS: usize = 64 * 1024 * 8;

#[inline]
fn tri_hash(t: &[u8]) -> u64 {
    xxh3_64(t)
}

/// Lowercase ASCII trigrams of `s`, calling `f` with each trigram hash.
#[inline]
pub fn for_each_trigram(s: &[u8], mut f: impl FnMut(u64)) {
    if s.len() < 3 {
        return;
    }
    let mut w = [0u8; 3];
    for win in s.windows(3) {
        w[0] = win[0].to_ascii_lowercase();
        w[1] = win[1].to_ascii_lowercase();
        w[2] = win[2].to_ascii_lowercase();
        f(tri_hash(&w));
    }
}

/// Tiny HyperLogLog (p = 10) used only to size the filter.
struct Hll {
    reg: [u8; 1024],
}
impl Hll {
    fn new() -> Self {
        Hll { reg: [0; 1024] }
    }
    fn add(&mut self, h: u64) {
        let idx = (h >> 54) as usize;
        let rest = (h << 10) | (1 << 9);
        let rank = rest.leading_zeros() as u8 + 1;
        if rank > self.reg[idx] {
            self.reg[idx] = rank;
        }
    }
    fn estimate(&self) -> f64 {
        let m = 1024.0f64;
        let mut sum = 0.0;
        let mut zeros = 0;
        for &r in &self.reg {
            sum += 2f64.powi(-(r as i32));
            if r == 0 {
                zeros += 1;
            }
        }
        let e = 0.7213 / (1.0 + 1.079 / m) * m * m / sum;
        if e <= 2.5 * m && zeros > 0 {
            m * (m / zeros as f64).ln()
        } else {
            e
        }
    }
}

pub struct Bloom {
    bits: Vec<u64>,
    mask: u64,
}

impl Bloom {
    /// Build from an iterator over the column's distinct (or all) values.
    pub fn build<'a, I>(values: I) -> Bloom
    where
        I: Iterator<Item = &'a [u8]> + Clone,
    {
        let mut hll = Hll::new();
        for v in values.clone() {
            for_each_trigram(v, |h| hll.add(h));
        }
        let n = hll.estimate().max(1.0) as usize;
        let nbits = (n * 10).next_power_of_two().clamp(MIN_BITS, MAX_BITS);
        let mut b = Bloom {
            bits: vec![0; nbits / 64],
            mask: nbits as u64 - 1,
        };
        for v in values {
            for_each_trigram(v, |h| b.insert(h));
        }
        b
    }

    #[inline]
    fn insert(&mut self, h: u64) {
        let h1 = h as u32 as u64;
        let h2 = (h >> 32) | 1;
        for i in 0..K as u64 {
            let bit = (h1.wrapping_add(i.wrapping_mul(h2))) & self.mask;
            self.bits[(bit >> 6) as usize] |= 1 << (bit & 63);
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.bits.len() * 8);
        for w in &self.bits {
            out.extend_from_slice(&w.to_le_bytes());
        }
        out
    }
}

/// Query a serialized filter without materialising a `Bloom`.
pub fn may_contain(bytes: &[u8], needle_lower: &[u8]) -> bool {
    if needle_lower.len() < 3 || bytes.len() < 8 || bytes.len() % 8 != 0 {
        return true;
    }
    let mask = (bytes.len() as u64 * 8) - 1;
    let mut all = true;
    for_each_trigram(needle_lower, |h| {
        if !all {
            return;
        }
        let h1 = h as u32 as u64;
        let h2 = (h >> 32) | 1;
        for i in 0..K as u64 {
            let bit = (h1.wrapping_add(i.wrapping_mul(h2))) & mask;
            let byte = bytes[(bit >> 3) as usize];
            if byte & (1 << (bit & 7)) == 0 {
                all = false;
                return;
            }
        }
    });
    all
}
