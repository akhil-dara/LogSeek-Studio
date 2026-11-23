//! Text encoding detection and lossless fallbacks.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    Cp1252,
}

impl Encoding {
    /// Bytes of byte-order mark to skip at the start of the file.
    pub fn bom_len(self) -> usize {
        match self {
            Encoding::Utf8Bom => 3,
            Encoding::Utf16Le | Encoding::Utf16Be => 2,
            _ => 0,
        }
    }
    /// UTF-16 in either byte order: detected, but not yet read by the import.
    pub fn is_utf16(self) -> bool {
        matches!(self, Encoding::Utf16Le | Encoding::Utf16Be)
    }
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf8Bom => "UTF-8 (BOM)",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
            Encoding::Cp1252 => "Windows-1252",
        }
    }
}

pub fn detect_encoding(head: &[u8]) -> Encoding {
    if head.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Encoding::Utf8Bom
    } else if head.starts_with(&[0xFF, 0xFE]) {
        Encoding::Utf16Le
    } else if head.starts_with(&[0xFE, 0xFF]) {
        Encoding::Utf16Be
    } else {
        Encoding::Utf8
    }
}

/// The start of a file as text, for showing and sniffing: a UTF-16 head
/// (marked by its byte-order mark) is decoded to UTF-8, with the mark dropped
/// and a cut at the end of the sample (half a character) left off; any other
/// head is returned as it is.
pub fn head_as_utf8(head: &[u8]) -> Cow<'_, [u8]> {
    let enc = detect_encoding(head);
    if !enc.is_utf16() {
        return Cow::Borrowed(head);
    }
    let mut body = &head[enc.bom_len()..];
    // a sample is cut anywhere: an odd last byte, or the first half of a pair
    if body.len() % 2 == 1 {
        body = &body[..body.len() - 1];
    }
    let big = enc == Encoding::Utf16Be;
    if let Some(last) = body.chunks_exact(2).next_back() {
        let u = if big {
            u16::from_be_bytes([last[0], last[1]])
        } else {
            u16::from_le_bytes([last[0], last[1]])
        };
        if (0xD800..0xDC00).contains(&u) {
            body = &body[..body.len() - 2];
        }
    }
    Cow::Owned(utf16_to_utf8(body, big).0.into_bytes())
}

/// A plain sentence for the user when the file is UTF-16: the import reads
/// bytes as UTF-8 or Windows-1252, so a UTF-16 file would be searched and
/// split as text with a zero byte between its letters. `None` for any other
/// encoding. (Until the import itself transcodes, this is the way to tell the
/// user, rather than show them garbage.)
pub fn unreadable_note(head: &[u8]) -> Option<String> {
    let enc = detect_encoding(head);
    enc.is_utf16().then(|| {
        format!(
            "this file is {}: LogSeek cannot read it yet, and its text would show with gaps between the letters and find nothing. The file is not changed. Save a copy as UTF-8 (in Notepad, Save As and choose UTF-8; in PowerShell, Export-Csv -Encoding UTF8) and open that",
            enc.label()
        )
    })
}

/// Windows-1252 code points for bytes 0x80..=0x9F (0 = undefined → U+FFFD... kept as Latin-1 C1).
const CP1252_HI: [u32; 32] = [
    0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
    0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
];

/// Decode bytes as Windows-1252. Total: every byte maps to one char, so it is lossless.
pub fn decode_cp1252(b: &[u8]) -> String {
    b.iter()
        .map(|&c| match c {
            0x80..=0x9F => char::from_u32(CP1252_HI[(c - 0x80) as usize]).unwrap_or('\u{FFFD}'),
            _ => c as char,
        })
        .collect()
}

/// UTF-8 if valid, otherwise Windows-1252 (second value = fell back).
pub fn to_text(b: &[u8]) -> (Cow<'_, str>, bool) {
    match std::str::from_utf8(b) {
        Ok(s) => (Cow::Borrowed(s), false),
        Err(_) => (Cow::Owned(decode_cp1252(b)), true),
    }
}

/// Transcode UTF-16 to UTF-8 (lossy on unpaired surrogates, which are counted).
pub fn utf16_to_utf8(b: &[u8], big_endian: bool) -> (String, u64) {
    let units = b.chunks_exact(2).map(|c| {
        if big_endian {
            u16::from_be_bytes([c[0], c[1]])
        } else {
            u16::from_le_bytes([c[0], c[1]])
        }
    });
    let mut bad = 0;
    let mut s: String = char::decode_utf16(units)
        .map(|r| {
            r.unwrap_or_else(|_| {
                bad += 1;
                '\u{FFFD}'
            })
        })
        .collect();
    // a last lone byte is not a character: counted and shown, not dropped quietly
    if b.len() % 2 == 1 {
        bad += 1;
        s.push('\u{FFFD}');
    }
    (s, bad)
}
