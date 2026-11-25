//! odbc32.dll の入口のトレース（起動モードとアタッチ）。DB は要らない（試験のアプリが NULL のハンドルで呼ぶ）。
#![cfg(windows)]
// 補助の関数も含めて試験なので unwrap でよい（clippy.toml の許可は #[test] の関数の中だけに効く）。
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use sqltrace_core::{SqlKind, Status, TraceEvent};
use sqltrace_itest::harness::{attach, launch, one_attacher_at_a_time, sql, warnings, App, Running};

fn launch_captures_direct_and_prepared_sql_from_the_app_only(app: App) {
    let events = launch(app, "--repeat 3", true);
    assert!(warnings(&events).is_empty(), "警告: {:?}", warnings(&events));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, TraceEvent::Status(Status::Launched { arch, .. }) if *arch == app.arch)),
        "{} として起動していない",
        app.arch
    );
    let got: Vec<(&str, SqlKind, Option<&str>)> = sql(&events)
        .iter()
        .map(|s| (s.function, s.kind, s.sql.as_deref()))
        .collect();
    let mut expected = Vec::new();
    for i in 0..3 {
        let w = format!("SELECT 0-{i} W");
        let a = format!("SELECT 0-{i} A");
        let p = format!("SELECT 0-{i} P");
        expected.push(("SQLExecDirectW", SqlKind::Execute, Some(w)));
        // odbc32 の中で A 系から W 系を呼んでいても、内側の分は出ない。
        expected.push(("SQLExecDirect", SqlKind::Execute, Some(a)));
        expected.push(("SQLPrepareW", SqlKind::Prepare, Some(p.clone())));
        // SQLExecute には SQL がないが、同じハンドルの準備から付く。
        expected.push(("SQLExecute", SqlKind::Execute, Some(p)));
    }
    let expected: Vec<(&str, SqlKind, Option<&str>)> =
        expected.iter().map(|(f, k, s)| (*f, *k, s.as_deref())).collect();
    assert_eq!(got, expected);
    for s in sql(&events) {
        let module = s.caller.as_ref().and_then(|c| c.module.as_deref());
        assert_eq!(module, Some("odbc_caller.exe"), "{}", s.function);
    }
    assert!(events
        .iter()
        .any(|e| matches!(e, TraceEvent::Status(Status::Exited { exit_code: 0, .. }))));
    assert!(events.iter().any(|e| matches!(
        e,
        TraceEvent::Status(Status::ModuleArmed { module, .. }) if module.eq_ignore_ascii_case("odbc32.dll")
    )));
}

fn every_call_from_many_threads_is_seen_once(app: App, emulate: bool) {
    let threads = 4;
    let repeat = 200;
    let events = launch(
        app,
        &format!("--threads {threads} --repeat {repeat} --direct-only"),
        emulate,
    );
    let mut seen: Vec<String> = sql(&events).iter().filter_map(|s| s.sql.clone()).collect();
    assert_eq!(seen.len(), threads * repeat);
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), threads * repeat, "同じ呼び出しを 2 回数えている");
    assert!(events
        .iter()
        .any(|e| matches!(e, TraceEvent::Status(Status::Exited { exit_code: 0, .. }))));
}

fn attach_and_detach_repeatedly_leaves_the_app_running(app: App, emulate: bool) {
    let _turn = one_attacher_at_a_time();
    let mut target = Running::start(
        app,
        &["--repeat", "0", "--interval-ms", "2", "--threads", "2", "--direct-only"],
    );
    for round in 0..5 {
        let attached = attach(target.pid(), emulate);
        if let Err(message) = attached.wait_for_sql(20, Duration::from_secs(10)) {
            panic!("{round} 回目: {message}");
        }
        attached.session.detach();
        attached.session.join().unwrap();
        assert!(
            attached
                .events
                .try_iter()
                .any(|e| matches!(e, TraceEvent::Status(Status::Detached { .. }))),
            "{round} 回目: デタッチの知らせがない"
        );
        // デタッチの後も対象が動き続ける（int3 や TF を残していない）。
        std::thread::sleep(Duration::from_millis(300));
        assert!(target.exited().is_none(), "{round} 回目のデタッチの後で対象が終わった");
    }
}

/// 複数行の SQL（CRLF と LF が混ざる）が、改行も含めてそのまま届く。
fn multiline_sql_is_kept_as_is(app: App) {
    let events = launch(app, "--multiline", true);
    assert!(warnings(&events).is_empty(), "警告: {:?}", warnings(&events));
    let got: Vec<(&str, Option<&str>, bool)> = sql(&events)
        .iter()
        .map(|s| (s.function, s.sql.as_deref(), s.truncated))
        .collect();
    let expected = Some(sqltrace_itest::MULTILINE_SQL);
    assert_eq!(
        got,
        [("SQLExecDirectW", expected, false), ("SQLExecDirect", expected, false)]
    );
}

/// 同じ試験を、sqltrace と同じアーキテクチャのアプリと、x64 版から見た 32 ビット（WOW64）のアプリで回す。
macro_rules! suite {
    ($name:ident, $app:expr) => {
        mod $name {
            use super::*;

            #[test]
            fn launch_captures_sql() {
                launch_captures_direct_and_prepared_sql_from_the_app_only($app);
            }

            #[test]
            fn multiline_sql() {
                multiline_sql_is_kept_as_is($app);
            }

            #[test]
            fn many_threads_with_emulation() {
                every_call_from_many_threads_is_seen_once($app, true);
            }

            #[test]
            fn many_threads_with_single_step() {
                every_call_from_many_threads_is_seen_once($app, false);
            }

            #[test]
            fn attach_detach_with_emulation() {
                attach_and_detach_repeatedly_leaves_the_app_running($app, true);
            }

            #[test]
            fn attach_detach_with_single_step() {
                attach_and_detach_repeatedly_leaves_the_app_running($app, false);
            }
        }
    };
}

suite!(
    same_arch,
    sqltrace_itest::harness::native(env!("CARGO_BIN_EXE_odbc_caller"))
);

#[cfg(target_arch = "x86_64")]
suite!(wow64, sqltrace_itest::harness::wow64());
