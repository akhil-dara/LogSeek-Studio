//! Decide how to read a source from its first bytes (format-agnostic defaults
//! that the Check before opening screen shows and the user can override).

use super::csv::{read_header, sniff};
use crate::schema::suggest_record_path;
use crate::store::SourceSpec;

pub fn detect_spec(head: &[u8]) -> SourceSpec {
    let first = head
        .iter()
        .copied()
        .find(|b| !b.is_ascii_whitespace() && *b != 0xEF && *b != 0xBB && *b != 0xBF);
    match first {
        Some(b'[') => SourceSpec::Json {
            record_path: None,
            array: true,
            stream: false,
            bulk: false,
        },
        Some(b'{') => {
            // NDJSON if the first line is a complete object; otherwise a wrapper
            // document or a stream of multi-line objects.
            let line_end = head.iter().position(|&b| b == b'\n').unwrap_or(head.len());
            let first_line = head[..line_end].trim_ascii();
            let complete = serde_json::from_slice::<serde::de::IgnoredAny>(first_line).is_ok();
            if complete && line_end < head.len() {
                SourceSpec::Json {
                    record_path: None,
                    array: false,
                    stream: false,
                    bulk: is_bulk(head),
                }
            } else {
                match suggest_record_path(head) {
                    Some(k) => SourceSpec::Json {
                        record_path: Some(k),
                        array: false,
                        stream: false,
                        bulk: false,
                    },
                    // not one object per line and no wrapper array: objects spread over
                    // lines (pretty-printed, possibly several back to back)
                    None => SourceSpec::Json {
                        record_path: None,
                        array: false,
                        stream: true,
                        bulk: false,
                    },
                }
            }
        }
        _ => {
            let opts = sniff(head);
            let header = read_header(head, &opts);
            // lines read as their parts when the file names them on a #Fields:
            // line, or when that gives more columns than the delimiter and
            // the delimiter does not split the lines cleanly (or the first
            // line is no header): a clean CSV stays a CSV
            let clean_csv = super::csv::consistency(head, &opts) >= 0.9 && opts.has_header;
            match super::textlayout::infer(head) {
                Some(l)
                    if has_fields_line(head) || (l.parts.len() > header.len() && !clean_csv) =>
                {
                    SourceSpec::Text {
                        parts: l.parts,
                        header: l.names,
                    }
                }
                _ => SourceSpec::Csv { opts, header },
            }
        }
    }
}

/// The file names its own columns on a `#Fields:` line (a W3C extended log).
fn has_fields_line(head: &[u8]) -> bool {
    head.split(|&b| b == b'\n')
        .take(50)
        .any(|l| l.starts_with(b"#Fields:"))
}

/// Elasticsearch bulk: at least 4 complete lines and 90 % of the odd ones
/// (1st, 3rd, ...) are instruction lines.
fn is_bulk(head: &[u8]) -> bool {
    let lines: Vec<&[u8]> = head
        .split(|&b| b == b'\n')
        .map(|l| l.trim_ascii())
        .filter(|l| !l.is_empty())
        .collect();
    // the last line may be cut by the sample
    let complete = &lines[..lines.len().saturating_sub(1)];
    if complete.len() < 4 {
        return false;
    }
    let odd: Vec<&&[u8]> = complete.iter().step_by(2).collect();
    let hits = odd
        .iter()
        .filter(|l| super::json::bulk_action(l).is_some())
        .count();
    hits * 10 >= odd.len() * 9
}
