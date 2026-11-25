//! Oracle の OCI と OCI7 の入口のトレース。Oracle のクライアントは要らない（試験用の oci.dll を使う）。
#![cfg(windows)]
#![allow(clippy::unwrap_used)]

use sqltrace_core::{Arch, SqlKind, Status, TraceEvent};
use sqltrace_itest::harness::{built, launch, sql, warnings};

fn oci_calls_are_traced_and_correlated(target: &str, arch: Arch, emulate: bool) {
    let app = built(target, "oci_caller", arch);
    let dir = sqltrace_itest::artifacts(target);
    let events = launch(app, &format!("--dll-dir \"{}\"", dir.display()), emulate);
    assert!(warnings(&events).is_empty(), "警告: {:?}", warnings(&events));
    let got: Vec<(&str, &str, SqlKind, Option<&str>)> = sql(&events)
        .iter()
        .map(|s| (s.layer.id, s.function, s.kind, s.sql.as_deref()))
        .collect();
    use SqlKind::{Execute, Prepare};
    let expected: Vec<(&str, &str, SqlKind, Option<&str>)> = vec![
        ("oci", "OCIStmtPrepare", Prepare, Some("SELECT 1 FROM dual")),
        ("oci", "OCIStmtExecute", Execute, Some("SELECT 1 FROM dual")),
        // OCI_DESCRIBE_ONLY の実行は出ない。
        ("oci", "OCIStmtPrepare", Prepare, Some("SELECT '日本' FROM dual")),
        ("oci", "OCIStmtExecute", Execute, Some("SELECT '日本' FROM dual")),
        ("oci", "OCIStmtPrepare", Prepare, Some("SELECT N'日本' FROM dual")),
        ("oci", "OCIStmtExecute", Execute, Some("SELECT N'日本' FROM dual")),
        ("oci", "OCIStmtPrepare2", Prepare, Some("SELECT 2 FROM dual")),
        // OCIStmtPrepare2 の文のハンドルは戻りで返るので、まだ SQL を付けられない（手順 9）。
        ("oci", "OCIStmtExecute", Execute, None),
        ("oci7", "oparse", Prepare, Some("SELECT 7 FROM dual")),
        ("oci7", "oexec", Execute, Some("SELECT 7 FROM dual")),
        ("oci7", "osql3", Prepare, Some("UPDATE t SET x = 1")),
        ("oci7", "oexn", Execute, Some("UPDATE t SET x = 1")),
        ("oci7", "oexfet", Execute, Some("SELECT 7 FROM dual")),
        // sqora32.dll（ODBC ドライバ）の中からの OCI は出ない。
    ];
    assert_eq!(got, expected);
    let armed = events.iter().find_map(|e| match e {
        TraceEvent::Status(Status::ModuleArmed { module, functions, .. }) if module.eq_ignore_ascii_case("oci.dll") => {
            Some(functions.clone())
        }
        _ => None,
    });
    let mut armed = armed.expect("oci.dll にブレークポイントを置いていない");
    armed.sort_unstable();
    assert_eq!(
        armed,
        [
            "OCIStmtExecute",
            "OCIStmtPrepare",
            "OCIStmtPrepare2",
            "oexec",
            "oexfet",
            "oexn",
            "oparse",
            "osql3"
        ]
    );
    assert!(events
        .iter()
        .any(|e| matches!(e, TraceEvent::Status(Status::Exited { exit_code: 0, .. }))));
}

macro_rules! suite {
    ($name:ident, $target:expr, $arch:expr) => {
        mod $name {
            use super::*;

            #[test]
            fn with_emulation() {
                oci_calls_are_traced_and_correlated($target, $arch, true);
            }

            #[test]
            fn with_single_step() {
                oci_calls_are_traced_and_correlated($target, $arch, false);
            }
        }
    };
}

suite!(
    same_arch,
    sqltrace_itest::NATIVE_TARGET,
    if cfg!(target_arch = "x86") {
        Arch::X86
    } else {
        Arch::X64
    }
);

#[cfg(target_arch = "x86_64")]
suite!(wow64, sqltrace_itest::WOW64_TARGET, Arch::X86);
