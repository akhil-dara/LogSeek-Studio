//! UTF-16 files: detected by their byte-order mark, decoded correctly where
//! the engine looks at them, and reported plainly (the import does not read
//! them yet).

use logseek_core::ingest::encoding::{
    detect_encoding, head_as_utf8, unreadable_note, utf16_to_utf8, Encoding,
};

fn utf16(text: &str, big: bool, bom: bool) -> Vec<u8> {
    let mut v = Vec::new();
    if bom {
        v.extend(if big { [0xFE, 0xFF] } else { [0xFF, 0xFE] });
    }
    for u in text.encode_utf16() {
        v.extend(if big { u.to_be_bytes() } else { u.to_le_bytes() });
    }
    v
}

#[test]
fn a_utf16_head_decodes_to_the_text_it_holds() {
    let text = "User,When\r\nadmin,2026-11-21 10:00:00\r\n\u{20ac} \u{1F600}\r\n";
    for big in [false, true] {
        let raw = utf16(text, big, true);
        assert!(detect_encoding(&raw).is_utf16());
        assert_eq!(&*head_as_utf8(&raw), text.as_bytes(), "big endian: {big}");
    }
}

#[test]
fn a_sample_cut_in_the_middle_of_a_character_is_trimmed_not_garbled() {
    let raw = utf16("ab\u{1F600}cd", false, true);
    // cut after the first half of the emoji pair (BOM 2 + a,b 4 + high surrogate 2)
    assert_eq!(&*head_as_utf8(&raw[..8]), b"ab");
    // cut after one byte of a unit
    assert_eq!(&*head_as_utf8(&raw[..5]), b"a");
    assert_eq!(&*head_as_utf8(&raw), "ab\u{1F600}cd".as_bytes());
}

#[test]
fn other_heads_are_returned_as_they_are() {
    let utf8 = b"a,b\n1,2\n";
    assert!(matches!(
        head_as_utf8(utf8),
        std::borrow::Cow::Borrowed(b) if b == utf8
    ));
    assert_eq!(detect_encoding(utf8), Encoding::Utf8);
    assert!(unreadable_note(utf8).is_none());
    assert!(unreadable_note(&[0xEF, 0xBB, 0xBF, b'a']).is_none());
}

#[test]
fn a_utf16_file_is_called_what_it_is_in_plain_words() {
    let le = unreadable_note(&utf16("a,b\r\n", false, true)).unwrap();
    assert!(le.contains("UTF-16 LE") && le.contains("cannot read"), "{le}");
    let be = unreadable_note(&utf16("a,b\r\n", true, true)).unwrap();
    assert!(be.contains("UTF-16 BE"), "{be}");
}

#[test]
fn a_lone_last_byte_or_a_broken_pair_is_counted_not_dropped() {
    let (s, bad) = utf16_to_utf8(&[b'a', 0, b'b'], false);
    assert_eq!((s.as_str(), bad), ("a\u{FFFD}", 1));
    let (s, bad) = utf16_to_utf8(&[0x3D, 0xD8, b'a', 0], false);
    assert_eq!((s.as_str(), bad), ("\u{FFFD}a", 1));
}
