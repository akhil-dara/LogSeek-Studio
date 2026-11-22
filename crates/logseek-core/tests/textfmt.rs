//! Text log formats: the step executor and the Apache formats.
//!
//! Some tests read sample logs that are not part of the repository. They run
//! when `LOGSEEK_TEST_CORPUS` names the folder that holds them, and pass
//! without checking anything otherwise.

use logseek_core::ingest::textfmt::{builtin, compile_apache, parse_line, Format, ParseStatus};

/// The sample-log folder, when one is given.
fn corpus() -> Option<std::path::PathBuf> {
    std::env::var_os("LOGSEEK_TEST_CORPUS").map(std::path::PathBuf::from)
}

fn val(f: &Format, line: &[u8], name: &str) -> Option<String> {
    let i = f
        .fields()
        .iter()
        .position(|n| n == name)
        .unwrap_or_else(|| panic!("no field {name}"));
    parse_line(f, line).values[i]
        .as_ref()
        .map(|v| v.to_string())
}

fn lines(file: &str) -> Vec<Vec<u8>> {
    let dir = corpus().expect("asked only when a sample folder is given");
    let data = std::fs::read(dir.join("apache-user-samples").join(file)).unwrap();
    data.split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| l.to_vec())
        .collect()
}

#[test]
fn apache_combined_reads_the_user_samples() {
    if corpus().is_none() {
        return;
    }
    let f = builtin("apache_combined").unwrap();
    for l in lines("case_sample_access_log.txt") {
        let p = parse_line(&f, &l);
        assert!(
            matches!(p.status, ParseStatus::Full),
            "{}",
            String::from_utf8_lossy(&l)
        );
    }
    let first = &lines("case_sample_access_log.txt")[0];
    assert_eq!(val(&f, first, "status").as_deref(), Some("200"));
    assert_eq!(val(&f, first, "request").as_deref(), Some("GET / HTTP/1.1"));
    assert_eq!(
        val(&f, first, "time").as_deref(),
        Some("29/Oct/2026:23:59:59 +0000")
    );
}

#[test]
fn extras_and_forwarded_chains() {
    if corpus().is_none() {
        return;
    }
    let f = builtin("apache_combined").unwrap();
    let l = lines("case_sample_access_log.txt")[0].clone();
    assert_eq!(
        val(&f, &l, "client").as_deref(),
        Some("192.0.2.85, 198.51.100.135, 198.51.100.18, 203.0.113.10:47350"),
        "the whole forwarded chain"
    );
    assert_eq!(
        val(&f, &l, "_rest").as_deref(),
        Some("respTime=48541;"),
        "text after the last field is kept"
    );
    assert!(matches!(parse_line(&f, &l).status, ParseStatus::Full));
    let quoted_tail = br#"192.0.2.15 - - [10/Oct/2000:13:55:41 -0700] "GET /p HTTP/1.1" 200 5 "-" "UA" "203.0.113.9, 10.0.0.1""#;
    assert_eq!(
        val(&f, quoted_tail, "_rest").as_deref(),
        Some(r#""203.0.113.9, 10.0.0.1""#)
    );
}

#[test]
fn odd_lines_are_kept() {
    if corpus().is_none() {
        return;
    }
    let f = builtin("apache_combined").unwrap();
    let all = lines("synthetic_test_lines.log");
    let find = |needle: &str| {
        all.iter()
            .find(|l| String::from_utf8_lossy(l).contains(needle))
            .unwrap()
            .clone()
    };
    // IPv6 client
    assert_eq!(
        val(&f, &find("/v6"), "client").as_deref(),
        Some("2001:db8::1")
    );
    // a "-" request
    let dash = find("\"-\" 408");
    assert_eq!(val(&f, &dash, "request").as_deref(), Some("-"));
    assert_eq!(val(&f, &dash, "status").as_deref(), Some("408"));
    // TLS bytes sent as the request stay as written
    assert_eq!(val(&f, &find("x16"), "status").as_deref(), Some("400"));
    // a truncated line: partial, with where it stopped
    let t = find("truncated-line");
    let p = parse_line(&f, &t);
    assert!(matches!(p.status, ParseStatus::Partial), "truncated");
    assert!(p.stopped_at.is_some());
    // a common-format line under combined: partial, its fields kept
    let common = find("404 -");
    assert!(matches!(
        parse_line(&f, &common).status,
        ParseStatus::Partial
    ));
    assert_eq!(val(&f, &common, "status").as_deref(), Some("404"));
    // garbage and JSON lines do not fit
    assert!(matches!(
        parse_line(&f, &find("garbage")).status,
        ParseStatus::Unparsed
    ));
    assert!(matches!(
        parse_line(&f, &find("{\"time\"")).status,
        ParseStatus::Unparsed
    ));
    // vhost_combined reads the vhost:port line
    let v = builtin("apache_vhost_combined").unwrap();
    let vl = find("www.example.com:443");
    assert_eq!(val(&v, &vl, "vhost").as_deref(), Some("www.example.com"));
    assert_eq!(val(&v, &vl, "port").as_deref(), Some("443"));
    assert_eq!(
        val(&v, &vl, "user_agent").as_deref(),
        Some(r#"UA "quoted" x"#),
        "escaped quotes inside a quoted field"
    );
}

#[test]
fn crlf_is_not_part_of_a_field() {
    let f = builtin("apache_combined").unwrap();
    let l = b"192.0.2.10 - - [10/Oct/2000:13:55:36 -0700] \"GET / HTTP/1.0\" 200 5 \"-\" \"UA\"\r";
    assert_eq!(val(&f, l, "user_agent").as_deref(), Some("UA"));
    assert!(matches!(parse_line(&f, l).status, ParseStatus::Full));
    let c = builtin("apache_common").unwrap();
    let l = b"192.0.2.10 - - [10/Oct/2000:13:55:36 -0700] \"GET / HTTP/1.0\" 200 2326\r";
    assert_eq!(val(&c, l, "bytes").as_deref(), Some("2326"));
}

#[test]
fn huge_line_is_whole() {
    let f = builtin("apache_combined").unwrap();
    let ua = "a".repeat(65_536);
    let l = format!(
        "192.0.2.10 - - [10/Oct/2000:13:55:36 -0700] \"GET / HTTP/1.0\" 200 5 \"-\" \"{ua}\""
    );
    assert_eq!(
        val(&f, l.as_bytes(), "user_agent").map(|v| v.len()),
        Some(65_536)
    );
}

#[test]
fn non_utf8_is_kept_lossy() {
    let f = builtin("apache_combined").unwrap();
    let mut l = b"192.0.2.10 - - [10/Oct/2000:13:55:36 -0700] \"GET /caf".to_vec();
    l.push(0xE9);
    l.extend_from_slice(b" HTTP/1.0\" 200 5 \"-\" \"UA\"");
    let p = parse_line(&f, &l);
    assert!(matches!(p.status, ParseStatus::Full), "kept, not unparsed");
    let req = val(&f, &l, "request").unwrap();
    assert!(
        req.starts_with("GET /caf") && req.ends_with(" HTTP/1.0"),
        "{req}"
    );
}

#[test]
fn log_format_strings_compile() {
    let f = compile_apache(r#"%h %l %u %t "%r" %>s %b "%{Referer}i" "%{User-Agent}i" %D"#).unwrap();
    assert_eq!(
        f.fields(),
        [
            "client",
            "ident",
            "user",
            "time",
            "request",
            "status",
            "bytes",
            "referer",
            "user_agent",
            "duration_us",
            "_rest"
        ]
    );
    assert!(
        compile_apache("%h %Q").is_err(),
        "an unknown directive is reported"
    );
}

// ───────────── nginx, key=value, syslog ─────────────

use logseek_core::ingest::textfmt::compile_nginx;

fn get(f: &Format, line: &[u8], name: &str) -> Option<String> {
    val(f, line, name)
}

#[test]
fn nginx_text_matches_its_json_twin() {
    let f = builtin("nginx_combined").unwrap();
    let Some(dir) = corpus() else {
        return;
    };
    let text = std::fs::read(dir.join("nginx-text").join("nginx_logs.log")).unwrap();
    let json =
        std::fs::read_to_string(dir.join("nginx-json").join("nginx_json_logs.json")).unwrap();
    let all: Vec<&[u8]> = text
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .collect();
    let full = all
        .iter()
        .filter(|l| matches!(parse_line(&f, l).status, ParseStatus::Full))
        .count();
    assert_eq!(full, all.len(), "every line of the nginx file reads fully");
    assert_eq!(all.len(), 51_462);
    let tl: Vec<&[u8]> = all.iter().take(200).copied().collect();
    for (t, j) in tl.iter().zip(json.lines().take(200)) {
        let p = parse_line(&f, t);
        assert!(
            matches!(p.status, ParseStatus::Full),
            "{}",
            String::from_utf8_lossy(t)
        );
        let v: serde_json::Value = serde_json::from_str(j).unwrap();
        assert_eq!(
            get(&f, t, "remote_addr").unwrap(),
            v["remote_ip"].as_str().unwrap()
        );
        assert_eq!(get(&f, t, "status").unwrap(), v["response"].to_string());
        assert_eq!(
            get(&f, t, "body_bytes_sent").unwrap(),
            v["bytes"].to_string()
        );
        assert_eq!(
            get(&f, t, "request").unwrap(),
            v["request"].as_str().unwrap()
        );
        assert_eq!(
            get(&f, t, "http_user_agent").unwrap(),
            v["agent"].as_str().unwrap()
        );
        assert_eq!(
            get(&f, t, "time_local").unwrap(),
            v["time"].as_str().unwrap()
        );
    }
}

#[test]
fn nginx_log_format_strings_compile() {
    let f = compile_nginx(r#"$remote_addr - $remote_user [$time_local] "$request" $status $body_bytes_sent "$http_referer" "$http_user_agent" "$http_x_forwarded_for" rt=$request_time"#).unwrap();
    assert_eq!(
        f.fields(),
        [
            "remote_addr",
            "remote_user",
            "time_local",
            "request",
            "status",
            "body_bytes_sent",
            "http_referer",
            "http_user_agent",
            "http_x_forwarded_for",
            "request_time",
            "_rest"
        ]
    );
    let l = br#"10.0.0.1 - - [17/May/2015:08:05:32 +0000] "GET / HTTP/1.1" 200 5 "-" "UA" "203.0.113.9, 10.0.0.1" rt=0.012"#;
    assert_eq!(
        get(&f, l, "http_x_forwarded_for").as_deref(),
        Some("203.0.113.9, 10.0.0.1")
    );
    assert_eq!(get(&f, l, "request_time").as_deref(), Some("0.012"));
    assert!(compile_nginx("no variables here").is_err());
}

#[test]
fn kv_handles_quotes_and_separators() {
    let f = builtin("kv").unwrap();
    let p = parse_line(&f, br#"a=1 b="x y" c='z';d=4,e=5"#);
    assert!(matches!(p.status, ParseStatus::Full));
    let pairs: Vec<(String, String)> = p
        .extra
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    assert_eq!(
        pairs,
        [("a", "1"), ("b", "x y"), ("c", "z"), ("d", "4"), ("e", "5")]
            .map(|(k, v)| (k.to_string(), v.to_string()))
    );
    // an Apache line with one trailing pair is not a key=value line
    let apache = br#"192.0.2.10 - - [10/Oct/2000:13:55:36 -0700] "GET / HTTP/1.0" 200 5 "-" "UA" respTime=12"#;
    assert!(matches!(
        parse_line(&f, apache).status,
        ParseStatus::Unparsed
    ));
    // a short header then pairs: partial, pairs kept, the rest in _rest
    let fw = br#"devname=FGT date=2016-08-24 time=18:27:37 note srcip=10.0.0.5 action="accept""#;
    let p = parse_line(&f, fw);
    assert!(matches!(p.status, ParseStatus::Partial));
    assert_eq!(p.extra.len(), 5);
    assert_eq!(get(&f, fw, "_rest").as_deref(), Some("note"));
}

#[test]
fn syslog_both_flavours() {
    let s = builtin("syslog_3164").unwrap();
    let l = b"<34>Oct 11 22:14:15 mymachine su[230]: 'su root' failed for lonvick on /dev/pts/8";
    assert!(matches!(parse_line(&s, l).status, ParseStatus::Full));
    assert_eq!(get(&s, l, "priority").as_deref(), Some("34"));
    assert_eq!(get(&s, l, "time").as_deref(), Some("Oct 11 22:14:15"));
    assert_eq!(get(&s, l, "host").as_deref(), Some("mymachine"));
    assert_eq!(get(&s, l, "app").as_deref(), Some("su"));
    assert_eq!(get(&s, l, "pid").as_deref(), Some("230"));
    assert_eq!(
        get(&s, l, "message").as_deref(),
        Some("'su root' failed for lonvick on /dev/pts/8")
    );
    let l = b"Oct  9 08:00:01 web01 CRON: (root) CMD (run-parts)";
    assert_eq!(get(&s, l, "time").as_deref(), Some("Oct  9 08:00:01"));
    assert_eq!(get(&s, l, "app").as_deref(), Some("CRON"));
    assert_eq!(get(&s, l, "pid"), None);
    assert_eq!(
        get(&s, l, "message").as_deref(),
        Some("(root) CMD (run-parts)")
    );
    assert!(matches!(
        parse_line(&s, b"not a syslog line at all").status,
        ParseStatus::Unparsed
    ));

    let n = builtin("syslog_5424").unwrap();
    let l = b"<165>1 2003-08-24T05:14:15.000003-07:00 192.0.2.1 myproc 8710 - - %% It's time to make the do-nuts.";
    assert!(matches!(parse_line(&n, l).status, ParseStatus::Full));
    assert_eq!(get(&n, l, "priority").as_deref(), Some("165"));
    assert_eq!(
        get(&n, l, "time").as_deref(),
        Some("2003-08-24T05:14:15.000003-07:00")
    );
    assert_eq!(get(&n, l, "app").as_deref(), Some("myproc"));
    assert_eq!(get(&n, l, "procid").as_deref(), Some("8710"));
    assert_eq!(get(&n, l, "msgid"), None, "- means no value");
    assert_eq!(
        get(&n, l, "message").as_deref(),
        Some("%% It's time to make the do-nuts.")
    );
    let l = br#"<165>1 2003-10-11T22:14:15.003Z mymachine.example.com evntslog - ID47 [exampleSDID@32473 iut="3" eventSource="App]lication" eventID="1011"] An application event log entry"#;
    assert_eq!(get(&n, l, "msgid").as_deref(), Some("ID47"));
    assert_eq!(
        get(&n, l, "structured_data").as_deref(),
        Some(r#"[exampleSDID@32473 iut="3" eventSource="App]lication" eventID="1011"]"#),
        "a ] inside a quoted value does not end the element"
    );
    assert_eq!(
        get(&n, l, "message").as_deref(),
        Some("An application event log entry")
    );
}
