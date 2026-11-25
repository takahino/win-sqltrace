//! 統合試験で共有する手順: 試験のアプリの選び方、起動モードでのトレース、アタッチの相手の起動。
// 試験の補助なので、失敗はその場で panic してよい。
#![allow(clippy::unwrap_used)]

use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use sqltrace_core::{Arch, LaunchSpec, SqlEvent, TargetSpec, TraceEvent};
use sqltrace_session::{run, spawn, Session, SessionConfig, StopSignal, TraceOptions};

/// 試験のアプリ。`arch` は sqltrace から見たアーキテクチャ（試験の確かめに使う）。
#[derive(Clone, Copy, Debug)]
pub struct App {
    pub path: &'static str,
    pub arch: Arch,
}

/// sqltrace と同じアーキテクチャの試験のアプリ。`exe` は `env!("CARGO_BIN_EXE_odbc_caller")` を渡す
/// （この変数は試験の crate の中でしか読めない）。
pub fn native(exe: &'static str) -> App {
    App {
        path: exe,
        arch: crate_arch(),
    }
}

fn crate_arch() -> Arch {
    if cfg!(target_arch = "x86") {
        Arch::X86
    } else {
        Arch::X64
    }
}

/// [`crate::artifacts`] でビルドした試験のアプリ。
pub fn built(target: &str, name: &str, arch: Arch) -> App {
    let path = crate::artifacts(target).join(format!("{name}.exe"));
    // 試験のプロセスが終わるまで使うので、手放さなくてよい。
    let path: &'static str = Box::leak(path.to_string_lossy().into_owned().into_boxed_str());
    App { path, arch }
}

/// x64 版から見た 32 ビット（WOW64）の試験のアプリ。
#[cfg(target_arch = "x86_64")]
pub fn wow64() -> App {
    built(crate::WOW64_TARGET, "odbc_caller", Arch::X86)
}

/// `emulate = false` では入口のエミュレーションを切り、必ずステップ実行の経路（ほかのスレッドの中断と順番待ち）を通す。
/// odbc32 の入口はどれもエミュレーションできる命令なので、切らないとその経路を試せない。
pub fn config(emulate: bool) -> SessionConfig {
    SessionConfig {
        trace: TraceOptions {
            emulate_prologue: emulate,
        },
        ..SessionConfig::default()
    }
}

/// 起動モードでトレースし、対象が終わるまでの出来事を返す。
pub fn launch(app: App, arguments: &str, emulate: bool) -> Vec<TraceEvent> {
    crate::kill_children_on_exit();
    let mut spec = LaunchSpec::new(app.path);
    spec.arguments = arguments.into();
    // 試験ではコンソールを開かない。
    spec.new_console = false;
    let mut events = Vec::new();
    run(
        &TargetSpec::Launch(spec),
        &config(emulate),
        &StopSignal::new(),
        &mut |e| events.push(e),
    )
    .expect("トレースに失敗");
    events
}

pub fn sql(events: &[TraceEvent]) -> Vec<&SqlEvent> {
    events
        .iter()
        .filter_map(|e| match e {
            TraceEvent::Sql(s) => Some(s),
            _ => None,
        })
        .collect()
}

pub fn warnings(events: &[TraceEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            TraceEvent::Warning(w) => Some(w.message.clone()),
            _ => None,
        })
        .collect()
}

/// アタッチの相手として動かしておくアプリ。初期化を終える（`ready` を出す）まで待ってから返す。
/// 落とすと終わらせる。
pub struct Running {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl Running {
    pub fn start(app: App, args: &[&str]) -> Self {
        crate::kill_children_on_exit();
        let mut child = Command::new(app.path)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready.trim(), "ready", "試験のアプリが初期化を終えない");
        let stdin = child.stdin.take();
        Self { child, stdin }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn exited(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().unwrap()
    }

    /// `--until-stdin-closes` のアプリに終わりを伝え、終わるのを待つ。
    pub fn finish(mut self, timeout: Duration) -> Option<ExitStatus> {
        drop(self.stdin.take());
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(status) = self.exited() {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 別のスレッドでアタッチしたセッションと、出来事の受け口。
pub struct Attached {
    pub session: Session,
    pub events: Receiver<TraceEvent>,
}

pub fn attach(pid: u32, emulate: bool) -> Attached {
    let (sender, events) = mpsc::channel();
    let session = spawn(TargetSpec::Attach { pid }, config(emulate), move |e| {
        let _ = sender.send(e);
    })
    .unwrap();
    Attached { session, events }
}

impl Attached {
    /// SQL の出来事が `count` 件届くまで待つ。警告が来たら失敗にする。
    pub fn wait_for_sql(&self, count: usize, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let mut hits = 0;
        let mut others = Vec::new();
        while hits < count {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(remaining) {
                Ok(TraceEvent::Sql(_)) => hits += 1,
                Ok(TraceEvent::Warning(w)) => return Err(format!("警告: {}", w.message)),
                Ok(other) => others.push(other),
                Err(_) => return Err(format!("SQL が {hits} 件しか届きません（届いたもの: {others:?}）")),
            }
        }
        Ok(())
    }
}

/// アタッチとデタッチを繰り返す試験は 1 つずつ回す。
///
/// 1 つのプロセスの中で複数のスレッドが同時にアタッチとデタッチを繰り返すと、CPU に余裕がないときに
/// `DebugActiveProcess` が戻らなくなることがあった（100 回の試験を 4 つ並べたとき。1 つずつなら同じ負荷でも
/// 起きない。OS の中のどこで待つのかは未確定）。sqltrace 自身は 1 プロセスに 1 つのデバッガなので、
/// 試験もそれに合わせる。
pub fn one_attacher_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    // 前の試験が失敗（panic）していても、順番を守るためだけの鍵なので使い続けてよい。
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
