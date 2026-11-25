//! 出力（コンソール、HTML、Excel、GUI）が受け取る出来事。

use std::time::SystemTime;

use crate::capture::CallerInfo;
use crate::probe::Layer;
use crate::Arch;

#[derive(Clone, Debug, PartialEq)]
pub enum TraceEvent {
    Sql(SqlEvent),
    Status(Status),
    Warning(Warning),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SqlKind {
    /// ハンドルに準備した（まだ実行していない）。
    Prepare,
    /// 実行した。
    Execute,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SqlEvent {
    pub time: SystemTime,
    pub pid: u32,
    pub tid: u32,
    pub layer: &'static Layer,
    pub function: &'static str,
    pub kind: SqlKind,
    /// `None` は、実行したハンドルの SQL が分からない（準備をトレース開始前に済ませていた、など）。
    pub sql: Option<String>,
    pub truncated: bool,
    pub handle: Option<u64>,
    pub caller: Option<CallerInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Attached {
        pid: u32,
        arch: Arch,
    },
    Launched {
        pid: u32,
        arch: Arch,
        exe: String,
    },
    /// モジュールが読み込まれ、その中の関数にブレークポイントを置いた。
    ModuleArmed {
        pid: u32,
        module: String,
        functions: Vec<&'static str>,
    },
    Detached {
        pid: u32,
    },
    Exited {
        pid: u32,
        exit_code: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    pub pid: Option<u32>,
    pub message: String,
}
