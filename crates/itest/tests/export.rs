//! 実際のトレースを、NDJSON / HTML / Excel に書き出す（cli と GUI が使う Recorder の通し試験）。
#![cfg(windows)]
#![allow(clippy::unwrap_used)]

use std::fs::File;
use std::io::BufReader;

use sqltrace_core::{LaunchSpec, TargetSpec};
use sqltrace_export::{ndjson, Console, Entry, OutputPlan, Recorder, TimeZone};
use sqltrace_session::{run, SessionConfig, StopSignal};

#[test]
fn a_real_trace_is_written_to_every_format_and_reads_back() {
    sqltrace_itest::kill_children_on_exit();
    let dir = std::env::temp_dir().join(format!("sqltrace-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let plan = OutputPlan {
        console: Console::Off,
        color: false,
        ndjson: Some(dir.join("r.ndjson")),
        html: Some(dir.join("r.html")),
        xlsx: Some(dir.join("r.xlsx")),
    };
    let mut recorder = Recorder::open(&plan, TimeZone { offset_minutes: 540 }, "試験").unwrap();
    let mut spec = LaunchSpec::new(env!("CARGO_BIN_EXE_odbc_caller"));
    spec.arguments = "--repeat 5".into();
    spec.new_console = false;
    run(
        &TargetSpec::Launch(spec),
        &SessionConfig::default(),
        &StopSignal::new(),
        &mut |e| recorder.record(&e),
    )
    .unwrap();
    let written = recorder.finish().unwrap();
    assert_eq!(written.len(), 3);

    let entries = ndjson::read(BufReader::new(File::open(dir.join("r.ndjson")).unwrap())).unwrap();
    let sql: Vec<&str> = entries
        .iter()
        .filter_map(|e| match e {
            Entry::Sql(s) => s.sql.as_deref(),
            _ => None,
        })
        .collect();
    // 5 回 × (W、A、準備、実行)。
    assert_eq!(sql.len(), 20);
    assert_eq!(sql[0], "SELECT 0-0 W");
    let html = std::fs::read_to_string(dir.join("r.html")).unwrap();
    // HTML の SQL は色分けの <span> に分かれるので、同じように色分けした形で探す。
    assert!(html.contains(&sqltrace_export::html::highlighted("SELECT 0-4 P")));
    let xlsx = std::fs::read(dir.join("r.xlsx")).unwrap();
    assert_eq!(&xlsx[..2], b"PK");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 複数行の SQL を、実際のトレースから 4 つの出力に通す。
#[test]
fn multiline_sql_survives_every_format() {
    use sqltrace_export::{xlsx::cell_text, ConsoleWriter};

    sqltrace_itest::kill_children_on_exit();
    let dir = std::env::temp_dir().join(format!("sqltrace-multiline-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let plan = OutputPlan {
        console: Console::Off,
        color: false,
        ndjson: Some(dir.join("m.ndjson")),
        html: Some(dir.join("m.html")),
        xlsx: Some(dir.join("m.xlsx")),
    };
    let zone = TimeZone::default();
    let mut recorder = Recorder::open(&plan, zone, "複数行").unwrap();
    let mut spec = LaunchSpec::new(env!("CARGO_BIN_EXE_odbc_caller"));
    spec.arguments = "--multiline".into();
    spec.new_console = false;
    run(
        &TargetSpec::Launch(spec),
        &SessionConfig::default(),
        &StopSignal::new(),
        &mut |e| recorder.record(&e),
    )
    .unwrap();
    recorder.finish().unwrap();
    let expected = sqltrace_itest::MULTILINE_SQL;

    // NDJSON: 改行はエスケープされ、読み戻すと 1 文字も変わらない。
    let text = std::fs::read_to_string(dir.join("m.ndjson")).unwrap();
    assert!(text.contains(r"b -- note\r\nFROM t\nWHERE"));
    let entries = ndjson::read(text.as_bytes()).unwrap();
    let sql: Vec<&Entry> = entries.iter().filter(|e| matches!(e, Entry::Sql(_))).collect();
    assert_eq!(sql.len(), 2);
    for entry in &sql {
        let Entry::Sql(s) = entry else { unreachable!() };
        assert_eq!(s.sql.as_deref(), Some(expected));
    }

    // HTML: <pre> の中に改行ごと入る（色分けの <span> を外すと元の SQL で、引用符はエスケープされる）。
    let html = std::fs::read_to_string(dir.join("m.html")).unwrap();
    let start = html.find("<pre>").unwrap() + "<pre>".len();
    let end = start + html[start..].find("</pre>").unwrap();
    let mut text = String::new();
    let mut in_tag = false;
    for c in html[start..end].chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    assert_eq!(
        text,
        "SELECT a,\r\n       b -- note\r\nFROM t\nWHERE x = &#39;one\r\ntwo&#39;\r\n"
    );
    assert!(html.contains("<span class=\"sk\">SELECT</span>"));

    // コンソール: どの行も同じ字下げで、CR は残らない。
    let mut out = Vec::new();
    let mut console = ConsoleWriter::new(&mut out, std::io::sink(), zone);
    console.write(sql[0]).unwrap();
    let printed = String::from_utf8(out).unwrap();
    let body: Vec<&str> = printed.lines().skip(1).collect();
    assert_eq!(
        body,
        [
            "    SELECT a,",
            "           b -- note",
            "    FROM t",
            "    WHERE x = 'one",
            "    two'"
        ]
    );
    assert!(!printed.contains('\r'));

    // Excel: CRLF は LF にそろう（セルの中で改行して見える）。
    assert_eq!(cell_text(expected), expected.replace("\r\n", "\n"));
    let _ = std::fs::remove_dir_all(&dir);
}
