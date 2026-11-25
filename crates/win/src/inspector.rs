//! デバッグ層が呼び出す側（session）に渡すもの。デバッグ層は SQL を知らず、「どこで止めるか」と
//! 「止まったら何を読むか」は [`Inspector`] が決める。

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use sqltrace_core::{Arch, CallSite, CallerInfo, Memory};

/// 読み込まれたモジュール。対象と同じアーキテクチャのものだけを渡す。
pub struct ModuleLoaded<'a> {
    pub pid: u32,
    pub arch: Arch,
    pub base: u64,
    pub size: u32,
    /// フルパス。取れなかったときは空。
    pub path: &'a str,
    /// ファイル名（`ODBC32.dll` など）。
    pub name: &'a str,
    pub memory: &'a dyn Memory,
}

/// 止めたいアドレス。`tag` は止まったときにそのまま返す（何の入口かを呼び出し側が引くため）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BreakpointRequest {
    pub address: u64,
    pub tag: usize,
}

/// ブレークポイントで止まった。`site` は関数の最初の命令を実行する前の状態。
pub struct BreakpointHit<'a> {
    pub time: SystemTime,
    pub pid: u32,
    pub tid: u32,
    pub tag: usize,
    pub site: CallSite<'a>,
    pub caller: Option<CallerInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// デバッグを始めた。`launched` は起動モード。
    Started {
        pid: u32,
        arch: Arch,
        exe: String,
        launched: bool,
    },
    /// モジュールにブレークポイントを置いた。
    Armed {
        pid: u32,
        module: String,
        tags: Vec<usize>,
    },
    Unloaded {
        pid: u32,
        module: String,
    },
    Exited {
        pid: u32,
        exit_code: u32,
    },
    Detached {
        pid: u32,
    },
    Warning {
        pid: Option<u32>,
        message: String,
    },
}

pub trait Inspector {
    /// モジュールが読み込まれた。止めたいアドレスを返す。
    fn module_loaded(&mut self, module: &ModuleLoaded<'_>) -> Vec<BreakpointRequest>;
    fn breakpoint(&mut self, hit: BreakpointHit<'_>);
    fn notice(&mut self, notice: Notice);
}

/// 止め方。デバッグのスレッドは 100ms ごとにこれを見る（デバッグ API はアタッチしたスレッドからしか
/// 呼べないので、ほかのスレッドや Ctrl+C のハンドラは旗を立てるだけにする）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopRequest {
    None,
    /// ブレークポイントを外し、対象を動かしたまま離れる。
    Detach,
    /// 対象を終了させる。
    Kill,
}

#[derive(Clone, Debug, Default)]
pub struct StopSignal(Arc<AtomicU8>);

impl StopSignal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn detach(&self) {
        // Kill を Detach で上書きしない（強いほうを残す）。
        let _ = self.0.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst);
    }

    pub fn kill(&self) {
        self.0.store(2, Ordering::SeqCst);
    }

    pub fn requested(&self) -> StopRequest {
        match self.0.load(Ordering::SeqCst) {
            0 => StopRequest::None,
            1 => StopRequest::Detach,
            _ => StopRequest::Kill,
        }
    }
}

/// デバッグ層の動きの設定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceOptions {
    /// 入口の命令をエミュレーションして、ステップ実行を避ける（既定）。
    pub emulate_prologue: bool,
}

impl Default for TraceOptions {
    fn default() -> Self {
        Self { emulate_prologue: true }
    }
}
