//! A JSON file that is one object holding several lists: its parts, and one
//! of them written out as rows.

use logseek_core::ingest::jsondoc::{is_doc, parts, write_part, Kind, Part, TOP};
use std::sync::atomic::{AtomicBool, AtomicU64};

const DOC: &str = r#"{
  "version": "1.2",
  "basics": { "name": "Mina Okoro", "location": { "city": "Leeds" } },
  "work": [
    { "company": "Northwind", "role": "Analyst", "highlights": ["a", "b"] },
    { "company": "Contoso", "role": "Lead" }
  ],
  "skills": ["triage", "forensics", "python"],
  "awards": []
}"#;

fn write(name: &str, text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join(name);
    std::fs::write(&p, text).unwrap();
    (d, p)
}

fn part(name: &str, kind: Kind, rows: u64) -> Part {
    Part {
        name: name.into(),
        kind,
        rows,
    }
}

#[test]
fn the_parts_of_a_document_are_listed_with_their_rows() {
    let (_d, p) = write("resume.json", DOC);
    assert_eq!(
        parts(&p).unwrap(),
        vec![
            part(TOP, Kind::Values, 1),
            part("basics", Kind::Record, 1),
            part("work", Kind::List, 2),
            part("skills", Kind::List, 3),
            part("awards", Kind::List, 0),
        ]
    );
    assert!(is_doc(&p));
}

#[test]
fn a_list_is_written_one_record_a_line_with_what_is_nested_kept() {
    let (d, p) = write("resume.json", DOC);
    let out = d.path().join("work.rows");
    let (stop, done) = (AtomicBool::new(false), AtomicU64::new(0));
    assert_eq!(write_part(&p, "work", &out, &stop, &done).unwrap(), 2);
    let text = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines,
        vec![
            r#"{"company":"Northwind","role":"Analyst","highlights":["a","b"]}"#,
            r#"{"company":"Contoso","role":"Lead"}"#,
        ]
    );
}

#[test]
fn plain_items_an_object_and_the_values_at_the_top_each_give_rows() {
    let (d, p) = write("resume.json", DOC);
    let (stop, done) = (AtomicBool::new(false), AtomicU64::new(0));
    let out = d.path().join("x.rows");
    assert_eq!(write_part(&p, "skills", &out, &stop, &done).unwrap(), 3);
    assert_eq!(
        std::fs::read_to_string(&out).unwrap().lines().next().unwrap(),
        r#"{"value":"triage"}"#
    );
    assert_eq!(write_part(&p, "basics", &out, &stop, &done).unwrap(), 1);
    assert_eq!(
        std::fs::read_to_string(&out).unwrap().trim(),
        r#"{"name":"Mina Okoro","location":{"city":"Leeds"}}"#
    );
    assert_eq!(write_part(&p, TOP, &out, &stop, &done).unwrap(), 1);
    assert_eq!(
        std::fs::read_to_string(&out).unwrap().trim(),
        r#"{"version":"1.2"}"#
    );
    // an empty list is a part with no rows, not an error
    assert_eq!(write_part(&p, "awards", &out, &stop, &done).unwrap(), 0);
    // a name the file does not have is said, and nothing is left behind
    assert!(write_part(&p, "nope", &out, &stop, &done).is_err());
    assert!(!out.exists());
}

#[test]
fn files_that_are_rows_already_are_not_documents() {
    // one record per line
    let (_a, p) = write("a.json", "{\"a\":1,\"b\":[1,2]}\n{\"a\":2,\"b\":[3]}\n");
    assert!(!is_doc(&p));
    // a list at the top
    let (_b, p) = write("b.json", "[{\"a\":1},{\"a\":2}]");
    assert!(!is_doc(&p));
    // one wrapped list and nothing else
    let (_c, p) = write("c.json", "{\"records\":[{\"a\":1},{\"a\":2}]}");
    assert!(!is_doc(&p));
    // one flat record
    let (_e, p) = write("e.json", "{\n  \"a\": 1,\n  \"b\": \"x\"\n}");
    assert!(!is_doc(&p));
    // not JSON by its name, or not readable as JSON
    let (_f, p) = write("f.txt", DOC);
    assert!(!is_doc(&p));
    let (_g, p) = write("g.json", "{ \"work\": [ {\"a\": 1}, ");
    assert!(!is_doc(&p));
    // a wrapped list with a count beside it is two parts: opened part by part
    let (_h, p) = write("h.json", "{\n \"total\": 2,\n \"records\": [{\"a\":1},{\"a\":2}]\n}");
    assert!(is_doc(&p));
}

#[test]
fn a_stop_leaves_nothing_behind() {
    let (d, p) = write("resume.json", DOC);
    let out = d.path().join("w.rows");
    let r = write_part(&p, "work", &out, &AtomicBool::new(true), &AtomicU64::new(0));
    assert!(r.is_err());
    assert!(!out.exists());
}
