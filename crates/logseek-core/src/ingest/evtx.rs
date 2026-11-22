//! Windows event logs (.evtx). Each event is written as one line of JSON
//! that the usual import reads, so an event log works like any other file.
//! The .evtx file itself is only ever read.

use super::packed::{PartGuard, Room};
use serde_json::{Map, Value};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub fn is_evtx(p: &Path) -> bool {
    p.extension()
        .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case("evtx"))
}

/// Put `v` under `name`; a name already taken gets "Data." in front.
fn put(out: &mut Map<String, Value>, name: &str, v: Value) {
    if out.contains_key(name) {
        out.insert(format!("Data.{name}"), v);
    } else {
        out.insert(name.to_string(), v);
    }
}

/// Every value under `v`, named by its path ("Outer.Inner"); attributes and
/// text sit at the level of the element they belong to.
fn spread(out: &mut Map<String, Value>, prefix: &str, v: &Value) {
    let join = |k: &str| {
        if prefix.is_empty() {
            k.to_string()
        } else {
            format!("{prefix}.{k}")
        }
    };
    match v {
        Value::Null => {}
        Value::Object(o) => {
            for (k, x) in o {
                match k.as_str() {
                    "#attributes" => spread(out, prefix, x),
                    "#text" if !prefix.is_empty() => spread(out, prefix, x),
                    _ => spread(out, &join(k), x),
                }
            }
        }
        other => put(
            out,
            if prefix.is_empty() { "value" } else { prefix },
            other.clone(),
        ),
    }
}

/// One event as plain columns, named by the event's own structure: its
/// header (`System`) first, each value named by its place (`EventID`,
/// `TimeCreated.SystemTime`, `Execution.ProcessID`), then the event's own
/// fields under their own names; anything else keeps its element's name in
/// front. No header field is chosen, renamed or left out by hand: which one
/// is the time is found from the values, like any file's.
pub fn plain(event: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    let ev = event.get("Event").unwrap_or(event);
    if let Some(sys) = ev.get("System") {
        spread(&mut out, "", sys);
    }
    if let Some(Value::Object(o)) = Some(ev) {
        for (k, v) in o {
            match k.as_str() {
                "System" | "#attributes" => {}
                "EventData" | "UserData" => spread(&mut out, "", v),
                _ => spread(&mut out, k, v),
            }
        }
    }
    out
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EvtxOut {
    /// Events written.
    pub events: u64,
    /// Events that could not be read (damaged parts of the log).
    pub damaged: u64,
}

/// Write every event of `path` to `out`, one JSON object per line. `done`
/// counts the events so far; `cancel` stops it (the unfinished file is removed).
pub fn evtx_to_jsonl(
    path: &Path,
    out: &Path,
    cancel: &AtomicBool,
    done: &AtomicU64,
) -> Result<EvtxOut, String> {
    let tmp = out.with_extension("part");
    // gone on every way out, a panic included; kept only once renamed into place
    let mut guard = PartGuard::new(&tmp);
    let r = write_events(path, &tmp, cancel, done)?;
    std::fs::rename(&tmp, out).map_err(|e| e.to_string())?;
    guard.disarm();
    Ok(r)
}

fn write_events(
    path: &Path,
    tmp: &Path,
    cancel: &AtomicBool,
    done: &AtomicU64,
) -> Result<EvtxOut, String> {
    let settings = evtx::ParserSettings::new().indent(false).num_threads(1);
    let mut parser = evtx::EvtxParser::from_path(path)
        .map_err(|e| e.to_string())?
        .with_configuration(settings);
    let f = std::fs::File::create(tmp).map_err(|e| e.to_string())?;
    let mut w = BufWriter::with_capacity(1 << 20, f);
    let mut r = EvtxOut::default();
    // a crafted log can write far more than it is large: the drive is watched
    let mut room = Room::new(tmp, "convert this event log");
    for rec in parser.records_json_value() {
        if (r.events + r.damaged) % 1024 == 0 && cancel.load(Ordering::Relaxed) {
            return Err("stopped".into());
        }
        match rec {
            Ok(rec) => {
                // one event, one line (line breaks inside values are escaped by JSON)
                let line = serde_json::to_string(&Value::Object(plain(&rec.data)))
                    .map_err(|e| e.to_string())?;
                w.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
                w.write_all(b"\n").map_err(|e| e.to_string())?;
                room.wrote(line.len() as u64 + 1)?;
                r.events += 1;
                done.store(r.events, Ordering::Relaxed);
            }
            Err(_) => r.damaged += 1,
        }
    }
    w.flush().map_err(|e| e.to_string())?;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_reads_as_columns_named_by_its_own_structure() {
        let ev: Value = serde_json::from_str(
            r##"{"Event":{"#attributes":{"xmlns":"x"},"System":{"Provider":{"#attributes":{"Name":"Microsoft-Windows-Security-Auditing","Guid":"{g}"}},"EventID":{"#attributes":{"Qualifiers":16384},"#text":4688},"Version":2,"Level":0,"TimeCreated":{"#attributes":{"SystemTime":"2026-11-15T18:41:52.1126886Z"}},"EventRecordID":17285037,"Correlation":null,"Execution":{"#attributes":{"ProcessID":4,"ThreadID":792}},"Channel":"Security","Computer":"vm-6","Security":null},"EventData":{"SubjectUserName":"vm-6$","ProcessID":"0x1a","Nested":{"#attributes":{"Kind":"k"},"#text":"t","Inner":{"Leaf":1}},"Gone":null},"RenderingInfo":{"Message":"hello"}}}"##,
        )
        .unwrap();
        let p = plain(&ev);
        let names: Vec<&str> = p.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            [
                // the header, in the event's own order, each value named by
                // its place (no names chosen or shortened by hand)
                "Provider.Name",
                "Provider.Guid",
                "EventID.Qualifiers",
                "EventID",
                "Version",
                "Level",
                "TimeCreated.SystemTime",
                "EventRecordID",
                "Execution.ProcessID",
                "Execution.ThreadID",
                "Channel",
                "Computer",
                // then the event's own fields under their own names
                "SubjectUserName",
                "ProcessID",
                "Nested.Kind",
                "Nested",
                "Nested.Inner.Leaf",
                "RenderingInfo.Message"
            ]
        );
        assert_eq!(p["EventID"], 4688);
        assert_eq!(p["TimeCreated.SystemTime"], "2026-11-15T18:41:52.1126886Z");
        // the event's own ProcessID and the logging process's stay apart
        assert_eq!(
            (&p["Execution.ProcessID"], &p["ProcessID"]),
            (&Value::from(4), &Value::from("0x1a"))
        );
        assert_eq!(p["Nested"], "t");
    }
}
