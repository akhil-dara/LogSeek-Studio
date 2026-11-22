//! What the import could not read cleanly, said in plain sentences. The
//! engine counts these while it reads; this turns the counts into the lines
//! shown above the rows, so a file that was not read whole never looks as if
//! it was.

use crate::moment::group;
use logseek_core::ingest::report::ImportReport;

fn many(n: u64, one: &str, more: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{} {more}", group(n))
    }
}

/// One sentence for each kind of trouble the import met, the most serious
/// first; empty when the file was read cleanly.
pub fn lines(r: &ImportReport) -> Vec<String> {
    let mut out = Vec::new();
    if !r.unreadable_spans.is_empty() {
        let bytes: u64 = r
            .unreadable_spans
            .iter()
            .map(|(a, b)| b.saturating_sub(*a))
            .sum();
        out.push(format!(
            "{} of the file could not be read and {} skipped ({} in all): the rows there are not shown.",
            many(r.unreadable_spans.len() as u64, "part", "parts"),
            if r.unreadable_spans.len() == 1 { "was" } else { "were" },
            many(bytes, "byte", "bytes"),
        ));
    }
    if r.unread_after_records > 0 {
        out.push(format!(
            "{} after the list of records {} not read: the file holds something more there (another list, another document, or stray data).",
            many(r.unread_after_records, "byte", "bytes"),
            if r.unread_after_records == 1 { "was" } else { "were" },
        ));
    }
    if r.unfinished_record_bytes > 0 {
        out.push(format!(
            "The last record is not finished (the file ends {} into it) and is not shown.",
            many(r.unfinished_record_bytes, "byte", "bytes"),
        ));
    }
    if r.invalid_json > 0 {
        out.push(format!(
            "{} not valid JSON and {} kept as raw text.",
            many(r.invalid_json, "record was", "records were"),
            if r.invalid_json == 1 { "is" } else { "are" },
        ));
    }
    if !r.bad_quote_spans.is_empty() {
        out.push(format!(
            "{} an unmatched quote and {} read without quoting.",
            many(r.bad_quote_spans.len() as u64, "line had", "lines had"),
            if r.bad_quote_spans.len() == 1 { "was" } else { "were" },
        ));
    }
    if r.ragged_rows > 0 {
        out.push(format!(
            "{} more or fewer values than the header; extra values are kept in _overflow.",
            many(r.ragged_rows, "row had", "rows had"),
        ));
    }
    if r.decode_errors > 0 {
        out.push(format!(
            "{} not valid UTF-8 and {} read as Windows-1252.",
            many(r.decode_errors, "value was", "values were"),
            if r.decode_errors == 1 { "was" } else { "were" },
        ));
    }
    if r.header_repeats > 0 {
        out.push(format!(
            "{} the header again and {} not counted as a row.",
            many(r.header_repeats, "line was", "lines were"),
            if r.header_repeats == 1 { "is" } else { "are" },
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_import_says_nothing() {
        assert!(lines(&ImportReport::default()).is_empty());
    }

    #[test]
    fn each_kind_of_trouble_gets_its_own_sentence_with_exact_counts() {
        let r = ImportReport {
            unreadable_spans: vec![(0, 1_000), (5_000, 5_500)],
            invalid_json: 1,
            bad_quote_spans: vec![(10, 20)],
            ragged_rows: 12_345,
            decode_errors: 2,
            header_repeats: 1,
            ..Default::default()
        };
        let l = lines(&r);
        assert_eq!(
            l[0],
            "2 parts of the file could not be read and were skipped (1,500 bytes in all): the rows there are not shown."
        );
        assert_eq!(l[1], "1 record was not valid JSON and is kept as raw text.");
        assert_eq!(l[2], "1 line had an unmatched quote and was read without quoting.");
        assert_eq!(
            l[3],
            "12,345 rows had more or fewer values than the header; extra values are kept in _overflow."
        );
        assert_eq!(l[4], "2 values were not valid UTF-8 and were read as Windows-1252.");
        assert_eq!(l[5], "1 line was the header again and is not counted as a row.");
        assert_eq!(l.len(), 6);
    }

    #[test]
    fn data_left_unread_in_a_json_file_is_said() {
        let r = ImportReport {
            unread_after_records: 2_048,
            unfinished_record_bytes: 1,
            ..Default::default()
        };
        let l = lines(&r);
        assert_eq!(
            l[0],
            "2,048 bytes after the list of records were not read: the file holds something more there (another list, another document, or stray data)."
        );
        assert_eq!(
            l[1],
            "The last record is not finished (the file ends 1 byte into it) and is not shown."
        );
    }
}
