//! Writing segments to disk and reading every column type back.
use logseek_core::budget::Budget;
use logseek_core::segment::{ColumnBuilder, Encoding, SegmentMeta, SegmentReader, SegmentWriter};
use logseek_core::types::{ColType, Value};
use rand::{Rng, SeedableRng};

fn budget() -> Budget {
    Budget::new(256 * 1024 * 1024)
}

fn text_rows(n: usize) -> Vec<Option<String>> {
    (0..n)
        .map(|i| {
            if i % 17 == 0 {
                None
            } else {
                Some(format!("user-{i} ünïcødé \"q\""))
            }
        })
        .collect()
}

#[test]
fn roundtrip_all_types() {
    let n = 10_000;
    let texts = text_rows(n);
    let mut t = ColumnBuilder::new(0, ColType::Text);
    let mut i = ColumnBuilder::new(1, ColType::Int);
    let mut f = ColumnBuilder::new(2, ColType::Float);
    let mut b = ColumnBuilder::new(3, ColType::Bool);
    let mut tm = ColumnBuilder::new(4, ColType::Time);
    for r in 0..n {
        match &texts[r] {
            Some(s) => t.push(Value::Text(s)),
            None => t.push(Value::Null),
        }
        if r % 13 == 0 {
            i.push(Value::Null)
        } else {
            i.push(Value::Int(r as i64 * -3 + 7))
        }
        f.push(Value::Float(r as f64 / 7.0));
        b.push(Value::Bool(r % 3 == 0));
        tm.push(Value::Time(1_794_564_646_100_000 + r as i64 * 1_000));
    }
    // a value that fails the column type is kept as text, never dropped
    let mut i2 = ColumnBuilder::new(5, ColType::Int);
    i2.push(Value::Int(1));
    i2.push(Value::Text("N/A"));
    i2.push(Value::Int(3));
    for _ in 3..n {
        i2.push(Value::Int(9));
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("000000.kseg");
    let cols = vec![
        t.finish(),
        i.finish(),
        f.finish(),
        b.finish(),
        tm.finish(),
        i2.finish(),
    ];
    let meta = SegmentMeta::new(0, n as u32, 0, 123_456);
    SegmentWriter::write(&path, &cols, meta).unwrap();

    let rd = SegmentReader::open(&path).unwrap();
    assert_eq!(rd.meta().rows, n as u32);
    let bud = budget();
    let tc = rd.column(0, &bud).unwrap().unwrap();
    let ic = rd.column(1, &bud).unwrap().unwrap();
    let fc = rd.column(2, &bud).unwrap().unwrap();
    let bc = rd.column(3, &bud).unwrap().unwrap();
    let tmc = rd.column(4, &bud).unwrap().unwrap();
    let i2c = rd.column(5, &bud).unwrap().unwrap();
    for r in 0..n {
        assert_eq!(tc.text(r).as_deref(), texts[r].as_deref(), "row {r}");
        if r % 13 == 0 {
            assert!(ic.is_null(r))
        } else {
            assert_eq!(ic.int(r), Some(r as i64 * -3 + 7))
        }
        assert_eq!(fc.float(r), Some(r as f64 / 7.0));
        assert_eq!(
            bc.text(r).as_deref(),
            Some(if r % 3 == 0 { "true" } else { "false" })
        );
        assert_eq!(tmc.int(r), Some(1_794_564_646_100_000 + r as i64 * 1_000));
    }
    assert_eq!(i2c.text(1).as_deref(), Some("N/A"));
    assert_eq!(i2c.int(2), Some(3));
    // stats
    let st = rd.meta().col(4).unwrap();
    assert_eq!(st.min_i, Some(1_794_564_646_100_000));
    assert_eq!(
        st.max_i,
        Some(1_794_564_646_100_000 + (n as i64 - 1) * 1_000)
    );
    assert_eq!(rd.meta().col(1).unwrap().nulls, ((n + 12) / 13) as u32);
}

#[test]
fn absent_column_reads_as_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.kseg");
    let mut c = ColumnBuilder::new(7, ColType::Text);
    c.push(Value::Text("x"));
    SegmentWriter::write(&path, &[c.finish()], SegmentMeta::new(0, 1, 0, 1)).unwrap();
    let rd = SegmentReader::open(&path).unwrap();
    assert!(rd.column(3, &budget()).unwrap().is_none());
    assert!(rd.column(7, &budget()).unwrap().is_some());
}

#[test]
fn dict_chosen_for_low_cardinality() {
    let mut c = ColumnBuilder::new(0, ColType::Text);
    for i in 0..10_000 {
        c.push(Value::Text(["Security", "System", "Application"][i % 3]));
    }
    assert_eq!(c.finish().encoding(), Encoding::Dict);
}

#[test]
fn plain_chosen_for_unique_text() {
    let mut c = ColumnBuilder::new(0, ColType::Text);
    for i in 0..10_000 {
        c.push(Value::Text(&format!("cmd.exe /c run {i}")));
    }
    assert_eq!(c.finish().encoding(), Encoding::Plain);
}

#[test]
fn bloom_no_false_negatives_and_low_fpr() {
    let mut rng = rand::rngs::StdRng::seed_from_u64(7);
    let vals: Vec<String> = (0..5_000)
        .map(|i| format!(r"C:\Windows\System32\proc{i}.exe -k {}", rng.gen::<u32>()))
        .collect();
    let mut c = ColumnBuilder::new(0, ColType::Text);
    for v in &vals {
        c.push(Value::Text(v));
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.kseg");
    SegmentWriter::write(
        &path,
        &[c.finish()],
        SegmentMeta::new(0, vals.len() as u32, 0, 1),
    )
    .unwrap();
    let rd = SegmentReader::open(&path).unwrap();
    for v in vals.iter().step_by(37) {
        let lower = v.to_ascii_lowercase();
        for w in [3usize, 5, 9] {
            if lower.len() >= w {
                let sub = &lower[lower.len() - w..];
                assert!(rd.may_contain_substring(0, sub), "false negative for {sub}");
            }
        }
        assert!(rd.may_contain_substring(0, &lower));
    }
    let mut fp = 0;
    let trials = 2_000;
    for t in 0..trials {
        let needle = format!("zq{:06x}xj", t * 7919);
        if rd.may_contain_substring(0, &needle) {
            fp += 1;
        }
    }
    assert!((fp as f64 / trials as f64) < 0.02, "fpr {fp}/{trials}");
}

#[test]
fn reading_one_column_does_not_decode_others() {
    let mut a = ColumnBuilder::new(0, ColType::Text);
    let mut b = ColumnBuilder::new(1, ColType::Int);
    for i in 0..1000 {
        a.push(Value::Text("a"));
        b.push(Value::Int(i));
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("d.kseg");
    SegmentWriter::write(
        &path,
        &[a.finish(), b.finish()],
        SegmentMeta::new(0, 1000, 0, 1),
    )
    .unwrap();
    let rd = SegmentReader::open(&path).unwrap();
    assert_eq!(rd.decodes(), 0);
    let _ = rd.column(1, &budget()).unwrap();
    assert_eq!(rd.decodes(), 1);
}

#[test]
fn decode_refused_when_budget_too_small() {
    let mut a = ColumnBuilder::new(0, ColType::Text);
    for i in 0..10_000 {
        a.push(Value::Text(&format!("value number {i}")));
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("e.kseg");
    SegmentWriter::write(&path, &[a.finish()], SegmentMeta::new(0, 10_000, 0, 1)).unwrap();
    let rd = SegmentReader::open(&path).unwrap();
    assert!(rd.column(0, &Budget::new(1024)).is_err());
}
