//! 負荷とデタッチの試験: アタッチとデタッチの 100 回の繰り返し、負荷をかけたままのデタッチ、スレッドの
//! 生成と終了、対象自身の例外の素通し、対象の終了。
//!
//! どれも「対象が最後まで壊れずに動き、自分で正常に終わる（終了コード 0）」ことまで確かめる。int3 や TF を
//! 残したり、例外を横取りしたりすると、終了コードが変わる。
#![cfg(windows)]
#![allow(clippy::unwrap_used)]

use std::collections::HashSet;
use std::time::Duration;

use sqltrace_core::{Status, TraceEvent};
use sqltrace_itest::harness::{attach, launch, one_attacher_at_a_time, sql, warnings, App, Running};

const BUSY: &[&str] = &["--until-stdin-closes", "--threads", "2", "--direct-only"];

/// 2 スレッドが休まず呼び続けるところに、アタッチとデタッチを 100 回繰り返す。最後に対象を正常に終わらせる。
/// （スレッドを増やすと、並べて走る試験どうしで CPU を取り合って遅くなるだけで、確かめる経路は変わらない。）
fn hundred_attach_detach_cycles_under_load(app: App, emulate: bool) {
    let _turn = one_attacher_at_a_time();
    let mut target = Running::start(app, BUSY);
    for round in 0..100 {
        let attached = attach(target.pid(), emulate);
        if let Err(message) = attached.wait_for_sql(5, Duration::from_secs(10)) {
            panic!("{round} 回目: {message}");
        }
        attached.session.detach();
        if let Err(error) = attached.session.join() {
            panic!("{round} 回目のデタッチに失敗: {error}");
        }
        let events: Vec<TraceEvent> = attached.events.try_iter().collect();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, TraceEvent::Status(Status::Detached { .. }))),
            "{round} 回目: デタッチの知らせがない"
        );
        assert!(
            warnings(&events).is_empty(),
            "{round} 回目: 警告 {:?}",
            warnings(&events)
        );
        assert!(target.exited().is_none(), "{round} 回目のデタッチの後で対象が終わった");
    }
    let status = target.finish(Duration::from_secs(10)).expect("対象が終わらない");
    assert!(status.success(), "対象が異常に終わった: {status:?}");
}

/// 1 回ごとにスレッドを作って終わらせる。ステップ実行の最中に生まれたスレッドも止め、終わったスレッドの
/// 中断の記録を残さないことを確かめる。
fn threads_come_and_go(app: App, emulate: bool) {
    let (threads, repeat) = (4, 50);
    let events = launch(
        app,
        &format!("--churn --threads {threads} --repeat {repeat} --direct-only"),
        emulate,
    );
    let seen: HashSet<String> = sql(&events).iter().filter_map(|s| s.sql.clone()).collect();
    assert_eq!(sql(&events).len(), threads * repeat);
    assert_eq!(seen.len(), threads * repeat, "同じ呼び出しを 2 回数えている");
    assert!(events
        .iter()
        .any(|e| matches!(e, TraceEvent::Status(Status::Exited { exit_code: 0, .. }))));
}

/// 対象が自分で起こした例外と int3 は、対象のハンドラに渡す。横取りすると、アプリが数の違いを見つけて
/// 終了コード 3 で終わる。
fn own_exceptions_reach_the_app(app: App, emulate: bool) {
    let events = launch(app, "--raise --threads 2 --repeat 50 --direct-only", emulate);
    assert_eq!(sql(&events).len(), 100);
    let exit = events.iter().find_map(|e| match e {
        TraceEvent::Status(Status::Exited { exit_code, .. }) => Some(*exit_code),
        _ => None,
    });
    assert_eq!(
        exit,
        Some(0),
        "対象の例外が対象に届いていない（終了コード 3 は数の違い）"
    );
}

/// 対象も終了させる止め方。
fn kill_terminates_the_target(app: App) {
    let _turn = one_attacher_at_a_time();
    let mut target = Running::start(app, BUSY);
    let attached = attach(target.pid(), true);
    attached.wait_for_sql(5, Duration::from_secs(10)).unwrap();
    attached.session.kill();
    attached.session.join().unwrap();
    assert!(attached
        .events
        .try_iter()
        .any(|e| matches!(e, TraceEvent::Status(Status::Exited { .. }))));
    std::thread::sleep(Duration::from_millis(200));
    assert!(target.exited().is_some(), "対象が終わっていない");
}

macro_rules! suite {
    ($name:ident, $app:expr) => {
        mod $name {
            use super::*;

            #[test]
            fn hundred_cycles_with_emulation() {
                hundred_attach_detach_cycles_under_load($app, true);
            }

            #[test]
            fn hundred_cycles_with_single_step() {
                hundred_attach_detach_cycles_under_load($app, false);
            }

            #[test]
            fn thread_churn_with_emulation() {
                threads_come_and_go($app, true);
            }

            #[test]
            fn thread_churn_with_single_step() {
                threads_come_and_go($app, false);
            }

            #[test]
            fn exceptions_with_emulation() {
                own_exceptions_reach_the_app($app, true);
            }

            #[test]
            fn exceptions_with_single_step() {
                own_exceptions_reach_the_app($app, false);
            }

            #[test]
            fn kill() {
                kill_terminates_the_target($app);
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
