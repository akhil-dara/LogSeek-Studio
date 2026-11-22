//! What an import noticed along the way, kept with the index.

use serde::{Deserialize, Serialize};

/// Everything an import noticed. Persisted in the manifest; shown on the
/// Check before opening screen.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ImportReport {
    pub rows: u64,
    /// Rows whose field count differed from the header (short or long).
    pub ragged_rows: u64,
    /// Extra fields kept in `_overflow` (never truncated).
    pub overflow_fields: u64,
    /// Byte ranges of lines with an unbalanced quote, re-read without quoting.
    pub bad_quote_spans: Vec<(u64, u64)>,
    /// Fields that were not valid UTF-8 and were decoded as Windows-1252.
    pub decode_errors: u64,
    /// Lines identical to the header (common when files were concatenated).
    pub header_repeats: u64,
    /// JSON records that failed to parse (kept raw).
    pub invalid_json: u64,
    /// Byte ranges whose chunk crashed the parser: skipped, listed here and
    /// viewable raw, while the rest of the import carries on.
    ///
    /// Also a record too long to hold in memory in one piece: it is cut at
    /// its last line end, read line by line, and the cut span is listed here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreadable_spans: Vec<(u64, u64)>,
    /// A file read through one key's array of records (`{"Records":[..]}`):
    /// bytes after that array that were not read (a second array, a second
    /// document, trailing data; the wrapper's own closing brace and white
    /// space are not counted).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unread_after_records: u64,
    /// Bytes of the last record when the file ends while it is still open
    /// (a truncated or partly written file): that record is not in the index.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unfinished_record_bytes: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl ImportReport {
    pub fn merge(&mut self, o: &ImportReport) {
        self.rows += o.rows;
        self.ragged_rows += o.ragged_rows;
        self.overflow_fields += o.overflow_fields;
        self.bad_quote_spans.extend_from_slice(&o.bad_quote_spans);
        self.decode_errors += o.decode_errors;
        self.header_repeats += o.header_repeats;
        self.invalid_json += o.invalid_json;
        self.unreadable_spans.extend_from_slice(&o.unreadable_spans);
        self.unread_after_records += o.unread_after_records;
        self.unfinished_record_bytes += o.unfinished_record_bytes;
    }
    pub fn is_clean(&self) -> bool {
        self.ragged_rows == 0
            && self.bad_quote_spans.is_empty()
            && self.decode_errors == 0
            && self.invalid_json == 0
            && self.unreadable_spans.is_empty()
            && self.unread_after_records == 0
            && self.unfinished_record_bytes == 0
    }
}
