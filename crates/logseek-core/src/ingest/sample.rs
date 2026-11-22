//! Sampling a file for the Check before opening screen: the head plus evenly spaced slices across
//! the whole file, each realigned to line boundaries — so a key that only
//! appears near the end of a very large file is still seen in under a second.

use std::fs::File;
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, Copy)]
pub struct SampleOptions {
    pub head_bytes: usize,
    pub slices: usize,
    pub slice_bytes: usize,
}

impl Default for SampleOptions {
    fn default() -> Self {
        SampleOptions {
            head_bytes: 1024 * 1024,
            slices: 64,
            slice_bytes: 256 * 1024,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Sample {
    pub file_len: u64,
    /// First bytes of the file, cut at the last full line (whole file if small).
    pub head: Vec<u8>,
    /// `(source offset, bytes)` — each starts at a line start and ends with `\n`.
    pub slices: Vec<(u64, Vec<u8>)>,
}

impl Sample {
    pub fn total_bytes(&self) -> usize {
        self.head.len() + self.slices.iter().map(|(_, b)| b.len()).sum::<usize>()
    }
}

pub fn sample_file(path: &Path, opts: &SampleOptions) -> std::io::Result<Sample> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let whole = opts.head_bytes as u64 + (opts.slices * opts.slice_bytes) as u64;
    if len <= whole {
        let mut head = Vec::with_capacity(len as usize);
        f.read_to_end(&mut head)?;
        return Ok(Sample {
            file_len: len,
            head,
            slices: Vec::new(),
        });
    }
    let mut head = vec![0u8; opts.head_bytes];
    f.read_exact(&mut head)?;
    if let Some(i) = head.iter().rposition(|&b| b == b'\n') {
        head.truncate(i + 1);
    }
    let first = opts.head_bytes as u64;
    let span = len - first - opts.slice_bytes as u64;
    let n = opts.slices.max(2);
    let mut slices = Vec::with_capacity(n);
    let mut buf = vec![0u8; opts.slice_bytes];
    for i in 0..n {
        let off = first + span * i as u64 / (n as u64 - 1);
        crate::segment::read_at(&f, &mut buf, off)?;
        let Some(nl) = buf.iter().position(|&b| b == b'\n') else {
            continue;
        };
        let start = nl + 1;
        let Some(end) = buf.iter().rposition(|&b| b == b'\n') else {
            continue;
        };
        if end < start {
            continue;
        }
        slices.push((off + start as u64, buf[start..=end].to_vec()));
    }
    Ok(Sample {
        file_len: len,
        head,
        slices,
    })
}
