//! Previewing a file before it is imported.
use logseek_core::ingest::preview::{preview, Preview};
use logseek_core::store::SourceSpec;
use std::io::Write;

fn write(name: &str, body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join(name);
    std::fs::File::create(&p)
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    (d, p)
}

fn names(p: &Preview) -> Vec<&str> {
    p.columns.iter().map(|s| s.as_str()).collect()
}

#[test]
fn csv_preview_lists_rows_and_every_time_field_with_how_full_it_is() {
    let mut body = String::from("Time,timestamp,user,msg\n");
    for i in 0..10 {
        let t = if i == 0 { "2024-03-01 10:00:00" } else { "" };
        body += &format!("{t},{},alice,hello {i}\n", 1_709_287_200 + i);
    }
    let (_d, p) = write("a.csv", &body);
    let pv = preview(&p, 50).unwrap();
    assert!(matches!(pv.spec, SourceSpec::Csv { .. }));
    assert_eq!(names(&pv), ["Time", "timestamp", "user", "msg"]);
    assert_eq!(pv.rows.len(), 10);
    assert_eq!(pv.rows[3][3].as_deref(), Some("hello 3"));
    let t: Vec<(&str, u32)> = pv
        .time
        .iter()
        .map(|c| (c.name.as_str(), (c.filled * 100.0).round() as u32))
        .collect();
    assert_eq!(t, [("Time", 10), ("timestamp", 100)]);
    // the same rule as the import: a sparse named time loses to a full epoch
    assert_eq!(
        pv.guess.map(|g| pv.time[g].name.as_str()),
        Some("timestamp")
    );
    let ts = &pv.time[1];
    assert_eq!(ts.epoch_unit_us, Some(1_000_000));
    assert_eq!(ts.first, Some(1_709_287_200_000_000));
    assert_eq!(ts.last, Some(1_709_287_209_000_000));
}

#[test]
fn json_lines_preview_reads_nested_fields() {
    let body = "{\"@timestamp\":\"2024-03-01T10:00:00Z\",\"host\":{\"name\":\"web1\"}}\n\
                {\"@timestamp\":\"2024-03-01T10:00:05Z\",\"host\":{\"name\":\"web2\"}}\n";
    let (_d, p) = write("a.json", body);
    let pv = preview(&p, 50).unwrap();
    assert!(matches!(pv.spec, SourceSpec::Json { .. }));
    assert!(names(&pv).contains(&"host.name"), "{:?}", names(&pv));
    assert_eq!(pv.rows.len(), 2);
    assert_eq!(
        pv.guess.map(|g| pv.time[g].name.as_str()),
        Some("@timestamp")
    );
}

#[test]
fn preview_stops_at_max_rows_but_counts_fill_on_the_whole_sample() {
    let mut body = String::from("t,v\n");
    for i in 0..500 {
        body += &format!("2024-03-01 10:00:{:02},{i}\n", i % 60);
    }
    let (_d, p) = write("b.csv", &body);
    let pv = preview(&p, 50).unwrap();
    assert_eq!(pv.rows.len(), 50);
    assert_eq!(pv.sampled_rows, 500);
}

#[test]
fn big_file_preview_reads_only_the_head() {
    let mut body = String::from("t,v\n");
    while body.len() < 3 << 20 {
        body += "2024-03-01 10:00:00,some value that pads the line out\n";
    }
    let (_d, p) = write("big.csv", &body);
    let pv = preview(&p, 50).unwrap();
    assert_eq!(pv.rows.len(), 50);
    assert!(
        pv.sampled_rows > 1000 && pv.sampled_rows < 30_000,
        "{}",
        pv.sampled_rows
    );
    assert_eq!(pv.file_len, body.len() as u64);
}

#[test]
fn repeated_and_empty_column_names_are_reported_not_renamed_silently() {
    let (_d, p) = write(
        "dup.csv",
        "Time,Host,Time,,Host,Time\n2024-03-01 10:00:00,a,2024-03-01 10:00:01,x,b,2024-03-01 10:00:02\n",
    );
    let pv = preview(&p, 50).unwrap();
    assert_eq!(
        names(&pv),
        ["Time", "Host", "Time_2", "column_4", "Host_2", "Time_3"]
    );
    // (name in the file, the name it was given)
    let got: Vec<(&str, &str)> = pv
        .renamed
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("Time", "Time_2"),
            ("", "column_4"),
            ("Host", "Host_2"),
            ("Time", "Time_3")
        ]
    );
    // a clean header reports nothing
    let (_d2, p2) = write("ok.csv", "a,b\n1,2\n");
    assert!(preview(&p2, 5).unwrap().renamed.is_empty());
}
