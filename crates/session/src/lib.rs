//! sqlTrace の組み立て。カタログ（何を捕まえるか）、デバッグ層（どう止めるか）、相関器（どう出すか）を
//! つなぎ、デバッグのスレッドを持つ。cli と gui はここだけを使う。
#![cfg(windows)]

use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::thread::JoinHandle;

use sqltrace_core::pe::{read_module, ExportTarget};
use sqltrace_core::{
    capture, Correlator, HitContext, PlanError, ProbeSet, Selection, Status, TargetSpec, TextOptions, TraceEvent,
    Warning,
};
use sqltrace_win::{BreakpointHit, BreakpointRequest, Inspector, ModuleLoaded, Notice, WindowsDecoder};

pub use sqltrace_win::{DebugError, StopRequest, StopSignal, TraceOptions};

#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub selection: Selection,
    pub text: TextOptions,
    /// DB の API の内側からの呼び出しを捨てる（既定）。調べるときは切る。
    pub suppress_nested: bool,
    pub trace: TraceOptions,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            selection: Selection::default(),
            text: TextOptions::default(),
            suppress_nested: true,
            trace: TraceOptions::default(),
        }
    }
}

#[derive(Debug)]
pub enum SessionError {
    Plan(PlanError),
    Debug(DebugError),
    /// sqltrace の不具合。対象からはデタッチ済み（後始末は済んでいる）。
    Panicked(String),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::Plan(e) => e.fmt(f),
            SessionError::Debug(e) => e.fmt(f),
            SessionError::Panicked(message) => write!(
                f,
                "sqltrace の内部で問題が起きたので、対象から離れました（{message}）。対象は動き続けています"
            ),
        }
    }
}

impl std::error::Error for SessionError {}

/// 対象をトレースし、出来事を `sink` に渡す。呼んだスレッドがデバッガになり、終わるまで戻らない。
pub fn run(
    target: &TargetSpec,
    config: &SessionConfig,
    stop: &StopSignal,
    sink: &mut dyn FnMut(TraceEvent),
) -> Result<(), SessionError> {
    let probes = ProbeSet::plan(&sqltrace_catalog::CATALOG, &config.selection).map_err(SessionError::Plan)?;
    let mut tracer = Tracer {
        correlator: Correlator::new(probes.clone(), config.suppress_nested),
        probes,
        text: config.text,
        sink,
    };
    // panic してもデバッグ層の Drop が int3 を戻してデタッチする。ここではそれを誤りに変えるだけ。
    match panic::catch_unwind(AssertUnwindSafe(|| {
        sqltrace_win::trace(target, &config.trace, stop, &mut tracer)
    })) {
        Ok(result) => result.map_err(SessionError::Debug),
        Err(payload) => Err(SessionError::Panicked(panic_message(payload.as_ref()))),
    }
}

/// 別のスレッドでトレースする（GUI 向け）。
pub fn spawn(
    target: TargetSpec,
    config: SessionConfig,
    mut sink: impl FnMut(TraceEvent) + Send + 'static,
) -> std::io::Result<Session> {
    let stop = StopSignal::new();
    let signal = stop.clone();
    let thread = std::thread::Builder::new()
        .name("sqltrace-debugger".into())
        .spawn(move || run(&target, &config, &signal, &mut sink))?;
    Ok(Session { stop, thread })
}

pub struct Session {
    stop: StopSignal,
    thread: JoinHandle<Result<(), SessionError>>,
}

impl Session {
    pub fn stop_signal(&self) -> &StopSignal {
        &self.stop
    }

    /// ブレークポイントを外して離れる（対象は動き続ける）。
    pub fn detach(&self) {
        self.stop.detach();
    }

    /// 対象を終了させる。
    pub fn kill(&self) {
        self.stop.kill();
    }

    pub fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }

    pub fn join(self) -> Result<(), SessionError> {
        self.thread
            .join()
            .unwrap_or_else(|payload| Err(SessionError::Panicked(panic_message(payload.as_ref()))))
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "不明な panic".into())
}

/// デバッグ層からの呼び出しを、カタログと相関器で SQL の出来事にする。
struct Tracer<'a> {
    probes: ProbeSet,
    correlator: Correlator,
    text: TextOptions,
    sink: &'a mut dyn FnMut(TraceEvent),
}

impl Tracer<'_> {
    fn warn(&mut self, pid: Option<u32>, message: String) {
        (self.sink)(TraceEvent::Warning(Warning { pid, message }));
    }
}

impl Inspector for Tracer<'_> {
    fn module_loaded(&mut self, module: &ModuleLoaded<'_>) -> Vec<BreakpointRequest> {
        let wanted: Vec<_> = self.probes.for_module(module.name).collect();
        if wanted.is_empty() {
            return Vec::new();
        }
        let image = match read_module(module.memory, module.base) {
            Ok(image) => image,
            Err(error) => {
                self.warn(Some(module.pid), format!("{}: {error}", module.name));
                return Vec::new();
            }
        };
        let mut requests = Vec::new();
        for (tag, probe) in wanted {
            // 古いクライアントにない関数（OCIStmtPrepare2 など）は、ないのが普通なので黙って飛ばす。
            match image.export(probe.function).map(|e| &e.target) {
                Some(ExportTarget::Code(rva)) => requests.push(BreakpointRequest {
                    address: module.base + u64::from(*rva),
                    tag,
                }),
                Some(ExportTarget::Forwarder(to)) => self.warn(
                    Some(module.pid),
                    format!(
                        "{} の {} は {to} への転送なので、ここでは捕まえません",
                        module.name, probe.function
                    ),
                ),
                None => {}
            }
        }
        requests
    }

    fn breakpoint(&mut self, hit: BreakpointHit<'_>) {
        let Some(&probe) = self.probes.probes().get(hit.tag) else {
            return;
        };
        let context = HitContext {
            time: hit.time,
            pid: hit.pid,
            tid: hit.tid,
            site: hit.site,
            caller: hit.caller,
        };
        let Some(captured) = capture(probe, context, &WindowsDecoder, &self.text) else {
            return;
        };
        for event in self.correlator.process(captured) {
            (self.sink)(event);
        }
    }

    fn notice(&mut self, notice: Notice) {
        let event = match notice {
            Notice::Started {
                pid,
                arch,
                exe,
                launched,
            } => {
                if launched {
                    Status::Launched { pid, arch, exe }
                } else {
                    Status::Attached { pid, arch }
                }
            }
            Notice::Armed { pid, module, tags } => {
                let probes = self.probes.probes();
                let functions = tags.iter().filter_map(|&t| probes.get(t)).map(|p| p.function).collect();
                Status::ModuleArmed { pid, module, functions }
            }
            Notice::Unloaded { .. } => return,
            Notice::Exited { pid, exit_code } => {
                self.correlator.forget_process(pid);
                Status::Exited { pid, exit_code }
            }
            Notice::Detached { pid } => {
                self.correlator.forget_process(pid);
                Status::Detached { pid }
            }
            Notice::Warning { pid, message } => return self.warn(pid, message),
        };
        (self.sink)(TraceEvent::Status(event));
    }
}
