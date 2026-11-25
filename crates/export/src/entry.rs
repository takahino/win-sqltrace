//! 出力の 1 行（[`Entry`]）。[`TraceEvent`] は静的な定義を参照するが、こちらは値をすべて自分で持つので、
//! NDJSON に保存して後で読み戻し、HTML や Excel に書き出せる。

use std::time::{SystemTime, UNIX_EPOCH};

use sqltrace_core::{SqlKind, Status, TraceEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Prepare,
    Execute,
}

impl Kind {
    pub fn id(self) -> &'static str {
        match self {
            Kind::Prepare => "prepare",
            Kind::Execute => "execute",
        }
    }

    /// 表に出す名前。
    pub fn label(self) -> &'static str {
        match self {
            Kind::Prepare => "準備",
            Kind::Execute => "実行",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "prepare" => Some(Kind::Prepare),
            "execute" => Some(Kind::Execute),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    pub address: u64,
    pub module: Option<String>,
    pub offset: u64,
}

impl Caller {
    /// `app.exe+0x2363` の形（モジュールが分からなければアドレスだけ）。
    pub fn display(&self) -> String {
        match &self.module {
            Some(module) => format!("{module}+0x{:x}", self.offset),
            None => format!("0x{:x}", self.address),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SqlEntry {
    /// UNIX 時刻（ミリ秒、UTC）。
    pub time_ms: i64,
    pub pid: u32,
    pub tid: u32,
    /// 層の id（`odbc` など）と表示名。
    pub layer: String,
    pub layer_name: String,
    pub function: String,
    pub kind: Kind,
    /// `None` は、実行したハンドルの SQL が分からない（準備がトレースの開始前だった、など）。
    pub sql: Option<String>,
    pub truncated: bool,
    pub handle: Option<u64>,
    pub caller: Option<Caller>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Sql(SqlEntry),
    /// 状態の知らせ（アタッチした、モジュールに置いた、終わった、など）。`event` は種類の id。
    Status {
        time_ms: i64,
        pid: Option<u32>,
        event: String,
        message: String,
    },
    Warning {
        time_ms: i64,
        pid: Option<u32>,
        message: String,
    },
}

impl Entry {
    pub fn time_ms(&self) -> i64 {
        match self {
            Entry::Sql(s) => s.time_ms,
            Entry::Status { time_ms, .. } | Entry::Warning { time_ms, .. } => *time_ms,
        }
    }

    /// 出来事を写す。状態と警告は時刻を持たないので、受け取った時刻 `now` を使う。
    pub fn from_event(event: &TraceEvent, now: SystemTime) -> Self {
        match event {
            TraceEvent::Sql(s) => Entry::Sql(SqlEntry {
                time_ms: unix_ms(s.time),
                pid: s.pid,
                tid: s.tid,
                layer: s.layer.id.to_owned(),
                layer_name: s.layer.name.to_owned(),
                function: s.function.to_owned(),
                kind: match s.kind {
                    SqlKind::Prepare => Kind::Prepare,
                    SqlKind::Execute => Kind::Execute,
                },
                sql: s.sql.clone(),
                truncated: s.truncated,
                handle: s.handle,
                caller: s.caller.as_ref().map(|c| Caller {
                    address: c.address,
                    module: c.module.clone(),
                    offset: c.offset,
                }),
            }),
            TraceEvent::Status(status) => {
                let (pid, event, message) = describe(status);
                Entry::Status {
                    time_ms: unix_ms(now),
                    pid: Some(pid),
                    event: event.to_owned(),
                    message,
                }
            }
            TraceEvent::Warning(w) => Entry::Warning {
                time_ms: unix_ms(now),
                pid: w.pid,
                message: w.message.clone(),
            },
        }
    }
}

fn describe(status: &Status) -> (u32, &'static str, String) {
    match status {
        Status::Attached { pid, arch } => (*pid, "attached", format!("PID {pid}（{arch}）にアタッチしました")),
        Status::Launched { pid, arch, exe } => {
            (*pid, "launched", format!("{exe} を PID {pid}（{arch}）で起動しました"))
        }
        Status::ModuleArmed { pid, module, functions } => (
            *pid,
            "armed",
            format!("{module} の {} を捕まえます", functions.join(", ")),
        ),
        Status::Detached { pid } => (
            *pid,
            "detached",
            format!("PID {pid} から離れました（対象は動き続けています）"),
        ),
        Status::Exited { pid, exit_code } => (
            *pid,
            "exited",
            format!("PID {pid} が終了しました（終了コード 0x{exit_code:x}）"),
        ),
    }
}

pub fn unix_ms(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    }
}
