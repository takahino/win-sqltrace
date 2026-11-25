//! 1 つのプロセスに付けられる sqltrace は 1 つだけ。2 つ目と、ほかのデバッガが付いているときの断り方。
#![cfg(windows)]
#![allow(clippy::unwrap_used)]

use std::sync::mpsc;
use std::time::Duration;

use sqltrace_itest::harness::{attach, native, one_attacher_at_a_time, Running};
use sqltrace_session::{DebugError, SessionError};

const APP: &str = env!("CARGO_BIN_EXE_odbc_caller");
const BUSY: &[&str] = &["--until-stdin-closes", "--interval-ms", "5", "--direct-only"];

#[test]
fn second_sqltrace_on_the_same_process_is_refused_and_named() {
    let _turn = one_attacher_at_a_time();
    let target = Running::start(native(APP), BUSY);
    let pid = target.pid();
    let first = attach(pid, true);
    first.wait_for_sql(3, Duration::from_secs(10)).unwrap();

    // 2 つ目は、アタッチする前に断られる（先の sqltrace はこの試験のプロセス）。
    let second = attach(pid, true);
    match second.session.join() {
        Err(SessionError::Debug(DebugError::AlreadyTraced { pid: p, tracer })) => {
            assert_eq!(p, pid);
            assert_eq!(tracer, Some(std::process::id()));
        }
        other => panic!("2 つ目の sqltrace が断られていない: {other:?}"),
    }
    // トレースの最中は、修復もしない（動いている sqltrace のブレークポイントを戻さない）。
    assert!(matches!(
        sqltrace_win::repair(pid),
        Err(sqltrace_win::RepairError::InUse { .. })
    ));
    // 先の sqltrace は何事もなく続いている。
    first.wait_for_sql(3, Duration::from_secs(10)).unwrap();

    // 離れた後は、また付けられる。
    first.session.detach();
    first.session.join().unwrap();
    let third = attach(pid, true);
    third.wait_for_sql(3, Duration::from_secs(10)).unwrap();
    third.session.detach();
    third.session.join().unwrap();
    let status = target.finish(Duration::from_secs(10)).expect("対象が終わらない");
    assert!(status.success());
}

#[test]
fn another_debugger_is_named() {
    use windows_sys::Win32::System::Diagnostics::Debug::{DebugActiveProcess, DebugActiveProcessStop};

    let _turn = one_attacher_at_a_time();
    let target = Running::start(native(APP), BUSY);
    let pid = target.pid();
    // sqltrace ではないデバッガの代わりに、別のスレッドから直接付ける（付けたスレッドが離れるまで付いたまま）。
    let (attached, attached_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let other = std::thread::spawn(move || {
        // SAFETY: 引数は値だけ。
        let ok = unsafe { DebugActiveProcess(pid) } != 0;
        attached.send(ok).unwrap();
        let _ = release_rx.recv();
        // SAFETY: 同じスレッドで付けたものを外す。
        unsafe { DebugActiveProcessStop(pid) };
    });
    assert!(attached_rx.recv().unwrap(), "ほかのデバッガの代わりを付けられない");

    let sqltrace = attach(pid, true);
    let result = sqltrace.session.join();
    release.send(()).unwrap();
    other.join().unwrap();
    assert!(
        matches!(result, Err(SessionError::Debug(DebugError::OtherDebugger { pid: p })) if p == pid),
        "ほかのデバッガが付いていると分からない: {result:?}"
    );
    drop(target);
}
