//! デバッグイベントのループ: アタッチ / 起動、イベントの振り分け、デタッチ。
//!
//! デバッグ API はアタッチ（または起動）したスレッドからしか呼べないので、[`trace`] を呼んだスレッドが
//! 最後までデバッガになる。止めるときは [`StopSignal`] に旗を立て、ループが 100ms ごとに見る。

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use sqltrace_core::pe::read_headers;
use sqltrace_core::text::read_text;
use sqltrace_core::{pattern, Arch, BufferMemory, CallSite, Encoding, LaunchSpec, Memory, TargetSpec, TextOptions};
use windows_sys::Win32::Foundation::{
    CloseHandle, DBG_CONTINUE, DBG_EXCEPTION_NOT_HANDLED, ERROR_INVALID_PARAMETER, ERROR_SEM_TIMEOUT,
    EXCEPTION_BREAKPOINT, EXCEPTION_SINGLE_STEP, HANDLE, NTSTATUS, STATUS_WX86_BREAKPOINT, STATUS_WX86_SINGLE_STEP,
};
use windows_sys::Win32::Storage::FileSystem::{GetFinalPathNameByHandleW, FILE_NAME_NORMALIZED};
use windows_sys::Win32::System::Diagnostics::Debug::{
    CheckRemoteDebuggerPresent, ContinueDebugEvent, DebugActiveProcess, DebugActiveProcessStop,
    DebugSetProcessKillOnExit, WaitForDebugEventEx, CREATE_PROCESS_DEBUG_EVENT, CREATE_PROCESS_DEBUG_INFO,
    CREATE_THREAD_DEBUG_EVENT, DEBUG_EVENT, EXCEPTION_DEBUG_EVENT, EXCEPTION_DEBUG_INFO, EXIT_PROCESS_DEBUG_EVENT,
    EXIT_THREAD_DEBUG_EVENT, LOAD_DLL_DEBUG_EVENT, LOAD_DLL_DEBUG_INFO, RIP_EVENT, UNLOAD_DLL_DEBUG_EVENT,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::codepage::WindowsDecoder;
use crate::context::{ContextKind, ThreadContext};
use crate::error::{last_error, DebugError};
use crate::inspector::{BreakpointHit, Inspector, ModuleLoaded, Notice, StopRequest, StopSignal, TraceOptions};
use crate::journal::{creation_time, tracer_of, Journal};
use crate::launch::{self, strip_verbatim};
use crate::lock::TargetLock;
use crate::memory::ProcessMemory;
use crate::process::{BreakpointKind, Process};

const POLL: Duration = Duration::from_millis(100);
/// デタッチの前に、外す前に int3 を実行していたスレッドのイベントを受け取りきる時間の上限。
const DRAIN_LIMIT: Duration = Duration::from_secs(2);
/// これだけイベントが来なければ、受け取りきったとみなす。
const DRAIN_QUIET: Duration = Duration::from_millis(100);

/// この sqltrace 自身のアーキテクチャ。
pub fn host_arch() -> Arch {
    if cfg!(target_arch = "x86") {
        Arch::X86
    } else {
        Arch::X64
    }
}

/// そのアーキテクチャのプロセスのレジスタの読み方。この版で扱えなければ `None`。
/// x64 版は x64 と 32 ビット（WOW64）を扱う。x86 版は x86 だけ（64 ビットのプロセスはデバッグできない）。
fn context_kind(arch: Arch) -> Option<ContextKind> {
    match (host_arch(), arch) {
        (host, target) if host == target => Some(ContextKind::Native),
        #[cfg(target_arch = "x86_64")]
        (Arch::X64, Arch::X86) => Some(ContextKind::Wow64),
        _ => None,
    }
}

fn supports(arch: Option<Arch>) -> bool {
    arch.and_then(context_kind).is_some()
}

/// この sqltrace でトレースできるアーキテクチャか。
pub fn can_trace(arch: Arch) -> bool {
    supports(Some(arch))
}

/// 対象をトレースする。対象が終わるか、`stop` でデタッチ（または終了）するまで戻らない。
pub fn trace(
    target: &TargetSpec,
    options: &TraceOptions,
    stop: &StopSignal,
    inspector: &mut dyn Inspector,
) -> Result<(), DebugError> {
    let mut debugger = Debugger {
        inspector,
        stop,
        processes: HashMap::new(),
        targets: HashSet::new(),
        launched: false,
        pending: None,
        killing: false,
        abort: None,
        silent: false,
        options: *options,
        locks: Vec::new(),
    };
    debugger.start(target)?;
    debugger.run()
}

struct Debugger<'a> {
    inspector: &'a mut dyn Inspector,
    stop: &'a StopSignal,
    processes: HashMap<u32, Process>,
    /// デバッグを始めた PID。CREATE_PROCESS のイベントが届く前から入る。
    targets: HashSet<u32>,
    launched: bool,
    /// 受け取ってまだ続行していないイベントと、そのときに返すべき値。panic で抜けたとき Drop で続行する。
    pending: Option<(u32, u32, NTSTATUS)>,
    killing: bool,
    /// 続けられないと分かった理由。イベントを続行した後でデタッチして返す。
    abort: Option<DebugError>,
    /// panic の後始末中。呼び出し側（壊れているかもしれない）を呼ばない。
    silent: bool,
    options: TraceOptions,
    /// トレースしている PID のロック（ほかの sqltrace が同じ PID に付かないように）。後始末（Drop）でデタッチ
    /// した後に手放される（フィールドは Drop の本体の後に落ちる）。
    locks: Vec<TargetLock>,
}

impl Debugger<'_> {
    fn start(&mut self, target: &TargetSpec) -> Result<(), DebugError> {
        let pid = match target {
            TargetSpec::Attach { pid } => {
                let pid = *pid;
                // 先に sqltrace どうしのロックを見る（相手が sqltrace なら、その PID まで伝えられる）。
                let lock = TargetLock::acquire(pid).ok_or_else(|| DebugError::AlreadyTraced {
                    pid,
                    tracer: tracer_of(pid),
                })?;
                if let Some(running) = inspect_running(pid) {
                    if let Some(arch) = running.arch.filter(|&a| !supports(Some(a))) {
                        return Err(DebugError::UnsupportedArch {
                            pid: Some(pid),
                            arch: Some(arch),
                        });
                    }
                    if running.debugged {
                        return Err(DebugError::OtherDebugger { pid });
                    }
                    // 初期化の途中（作られて間もない）のプロセスにアタッチすると、OS がアタッチのために作る
                    // スレッドで対象が落ちることがある（x86 版から WOW64 のプロセスで見た）。初期化が終わるまで待つ。
                    if let Some(created) = running.created {
                        std::thread::sleep(settle_delay(created, now_filetime()));
                    }
                }
                // SAFETY: 引数は値だけ。
                if unsafe { DebugActiveProcess(pid) } == 0 {
                    let code = last_error();
                    // 既にデバッガが付いているプロセスへのアタッチは ERROR_INVALID_PARAMETER で失敗する
                    // （事前の確認は、権限が足りないと「付いていない」と答える）。プロセスがまだあるなら、そちらと見る。
                    if code == ERROR_INVALID_PARAMETER && inspect_running(pid).is_some() {
                        return Err(DebugError::OtherDebugger { pid });
                    }
                    return Err(DebugError::Attach { pid, code });
                }
                self.locks.push(lock);
                pid
            }
            TargetSpec::Launch(spec) => {
                let pid = self.launch(spec)?;
                // 起動したばかりの PID なので必ず取れる（取れなくても、トレースは続けてよい）。
                self.locks.extend(TargetLock::acquire(pid));
                pid
            }
        };
        self.targets.insert(pid);
        // sqltrace が終わっても（強制終了でも）対象を道連れにしない。デバッグを始めた後に呼ぶ必要がある。
        // SAFETY: 引数は値だけ。
        unsafe { DebugSetProcessKillOnExit(0) };
        Ok(())
    }

    fn launch(&mut self, spec: &LaunchSpec) -> Result<u32, DebugError> {
        let exe = launch::resolve_exe(&spec.exe)?;
        let arch = file_arch(&exe);
        if arch.is_some() && !supports(arch) {
            return Err(DebugError::UnsupportedArch { pid: None, arch });
        }
        self.launched = true;
        launch::launch(spec, &exe)
    }

    fn run(&mut self) -> Result<(), DebugError> {
        while !self.targets.is_empty() {
            if let Some(error) = self.abort.take() {
                self.detach_all();
                return Err(error);
            }
            match self.stop.requested() {
                StopRequest::Detach => {
                    self.detach_all();
                    break;
                }
                StopRequest::Kill if !self.killing => {
                    self.killing = true;
                    let failed: Vec<u32> = self
                        .processes
                        .values()
                        .filter(|p| !p.terminate())
                        .map(|p| p.pid)
                        .collect();
                    for pid in failed {
                        self.notify(Notice::Warning {
                            pid: Some(pid),
                            message: "対象を終了できませんでした。タスク マネージャーから終了してください".into(),
                        });
                    }
                }
                _ => {}
            }
            if let Some(event) = wait(POLL)? {
                self.handle(&event)?;
            }
        }
        Ok(())
    }

    fn notify(&mut self, notice: Notice) {
        if !self.silent {
            self.inspector.notice(notice);
        }
    }

    fn handle(&mut self, event: &DEBUG_EVENT) -> Result<(), DebugError> {
        let (pid, tid) = (event.dwProcessId, event.dwThreadId);
        let default = if event.dwDebugEventCode == EXCEPTION_DEBUG_EVENT {
            DBG_EXCEPTION_NOT_HANDLED
        } else {
            DBG_CONTINUE
        };
        self.pending = Some((pid, tid, default));
        let status = self.dispatch(event);
        self.pending = None;
        // SAFETY: いま受け取ったイベントを 1 回だけ続行する。
        if unsafe { ContinueDebugEvent(pid, tid, status) } == 0 {
            return Err(DebugError::last_os("デバッグイベントの続行"));
        }
        let warnings = self
            .processes
            .get_mut(&pid)
            .map(|p| std::mem::take(&mut p.warnings))
            .unwrap_or_default();
        for message in warnings {
            self.notify(Notice::Warning {
                pid: Some(pid),
                message,
            });
        }
        Ok(())
    }

    fn dispatch(&mut self, event: &DEBUG_EVENT) -> NTSTATUS {
        let (pid, tid) = (event.dwProcessId, event.dwThreadId);
        // SAFETY: 共用体のどの部分が有効かは dwDebugEventCode で決まり、それに従って読む。
        unsafe {
            match event.dwDebugEventCode {
                CREATE_PROCESS_DEBUG_EVENT => self.on_create_process(pid, tid, &event.u.CreateProcessInfo),
                CREATE_THREAD_DEBUG_EVENT => {
                    if let Some(p) = self.processes.get_mut(&pid) {
                        p.add_thread(tid, event.u.CreateThread.hThread);
                    }
                }
                EXIT_THREAD_DEBUG_EVENT => {
                    if let Some(p) = self.processes.get_mut(&pid) {
                        p.remove_thread(tid);
                    }
                }
                LOAD_DLL_DEBUG_EVENT => self.on_load_dll(pid, &event.u.LoadDll),
                UNLOAD_DLL_DEBUG_EVENT => {
                    let base = event.u.UnloadDll.lpBaseOfDll as u64;
                    let module = self.processes.get_mut(&pid).and_then(|p| p.unload(base));
                    if let Some(module) = module {
                        self.notify(Notice::Unloaded { pid, module });
                    }
                }
                EXIT_PROCESS_DEBUG_EVENT => {
                    if let Some(mut p) = self.processes.remove(&pid) {
                        p.exited();
                    }
                    self.targets.remove(&pid);
                    let exit_code = event.u.ExitProcess.dwExitCode;
                    self.notify(Notice::Exited { pid, exit_code });
                }
                EXCEPTION_DEBUG_EVENT => return self.on_exception(pid, tid, &event.u.Exception),
                RIP_EVENT => self.notify(Notice::Warning {
                    pid: Some(pid),
                    message: format!("対象でシステムのエラーが起きました（RIP {}）", event.u.RipInfo.dwError),
                }),
                // OUTPUT_DEBUG_STRING など。何もしない。
                _ => {}
            }
        }
        DBG_CONTINUE
    }

    fn on_create_process(&mut self, pid: u32, tid: u32, info: &CREATE_PROCESS_DEBUG_INFO) {
        let path = final_path(info.hFile);
        close_file(info.hFile);
        let memory = ProcessMemory::new(info.hProcess);
        let base = info.lpBaseOfImage as u64;
        let arch = read_headers(&memory, base).ok().and_then(|h| h.arch);
        let Some((arch, context)) = arch.and_then(|a| context_kind(a).map(|k| (a, k))) else {
            self.abort = Some(DebugError::UnsupportedArch { pid: Some(pid), arch });
            return;
        };
        let journal = creation_time(info.hProcess).and_then(|created| match Journal::create(pid, created) {
            Ok(journal) => Some(journal),
            Err(error) => {
                self.notify(Notice::Warning {
                    pid: Some(pid),
                    message: format!("ジャーナルを作れませんでした（{error}）。強制終了されると修復できません"),
                });
                None
            }
        });
        let mut process = Process::new(
            pid,
            info.hProcess,
            arch,
            context,
            journal,
            self.options.emulate_prologue,
        );
        process.add_thread(tid, info.hThread);
        self.processes.insert(pid, process);
        self.notify(Notice::Started {
            pid,
            arch,
            exe: path.clone(),
            launched: self.launched,
        });
        self.load_module(pid, base, path);
    }

    fn on_load_dll(&mut self, pid: u32, info: &LOAD_DLL_DEBUG_INFO) {
        let mut path = final_path(info.hFile);
        close_file(info.hFile);
        let Some(process) = self.processes.get(&pid) else {
            return;
        };
        if path.is_empty() {
            path = image_name(
                &process.memory,
                process.arch,
                info.lpImageName as u64,
                info.fUnicode != 0,
            );
        }
        self.load_module(pid, info.lpBaseOfDll as u64, path);
    }

    fn load_module(&mut self, pid: u32, base: u64, path: String) {
        let Some(process) = self.processes.get_mut(&pid) else {
            return;
        };
        let name = pattern::file_name(&path).to_owned();
        let headers = read_headers(&process.memory, base).ok();
        let size = headers.map_or(0, |h| h.size_of_image);
        process.add_module(base, size, name.clone());
        // 対象と違うアーキテクチャのモジュール（WOW64 の 64 ビット側の DLL）は見ない。
        if self.silent || process.detaching() || headers.and_then(|h| h.arch) != Some(process.arch) {
            return;
        }
        let requests = self.inspector.module_loaded(&ModuleLoaded {
            pid,
            arch: process.arch,
            base,
            size,
            path: &path,
            name: &name,
            memory: &process.memory,
        });
        if requests.is_empty() {
            return;
        }
        let tags = process.arm(&requests);
        if !tags.is_empty() {
            self.notify(Notice::Armed {
                pid,
                module: name,
                tags,
            });
        }
    }

    fn on_exception(&mut self, pid: u32, tid: u32, info: &EXCEPTION_DEBUG_INFO) -> NTSTATUS {
        let record = &info.ExceptionRecord;
        // WOW64 のプロセスの 32 ビットのコードでは、int3 と TF の例外が別のコードで届く。
        if matches!(record.ExceptionCode, EXCEPTION_BREAKPOINT | STATUS_WX86_BREAKPOINT) {
            return self.on_breakpoint(pid, tid, record.ExceptionAddress as u64);
        }
        let Some(process) = self.processes.get_mut(&pid) else {
            return DBG_EXCEPTION_NOT_HANDLED;
        };
        let single_step = matches!(record.ExceptionCode, EXCEPTION_SINGLE_STEP | STATUS_WX86_SINGLE_STEP);
        if single_step && process.on_single_step(tid) {
            return DBG_CONTINUE;
        }
        // DBG_CONTROL_C なども含め、こちらのものでない例外はすべて対象の例外処理に渡す。
        process.on_other_exception(tid);
        DBG_EXCEPTION_NOT_HANDLED
    }

    fn on_breakpoint(&mut self, pid: u32, tid: u32, address: u64) -> NTSTATUS {
        let Some(process) = self.processes.get_mut(&pid) else {
            return DBG_EXCEPTION_NOT_HANDLED;
        };
        let (tag, ours) = match process.classify(address) {
            BreakpointKind::Ours(tag) => (tag, true),
            BreakpointKind::Retired(tag) => (tag, false),
            BreakpointKind::System => return DBG_CONTINUE,
            BreakpointKind::Foreign => return DBG_EXCEPTION_NOT_HANDLED,
        };
        let Some(thread) = process.thread(tid) else {
            return DBG_EXCEPTION_NOT_HANDLED;
        };
        // int3 を実行した後なので IP は入口 + 1。入口に戻してから、それ以外のことをする（途中で panic しても、
        // 後始末で元のバイトを戻せば正しく続く）。
        let Some(mut context) = ThreadContext::get(thread, process.context) else {
            process.warnings.push(format!("スレッド {tid} のレジスタを読めません"));
            return DBG_EXCEPTION_NOT_HANDLED;
        };
        sqltrace_core::prologue::Cpu::set_ip(&mut context, address);
        if !context.set(thread) {
            process.warnings.push(format!("スレッド {tid} の IP を戻せません"));
            return DBG_EXCEPTION_NOT_HANDLED;
        }
        if let Some(pending) = &mut self.pending {
            pending.2 = DBG_CONTINUE;
        }
        if !self.silent {
            let site = CallSite {
                arch: process.arch,
                regs: context.registers(),
                memory: &process.memory,
            };
            let caller = site.return_address().map(|a| process.caller(a));
            self.inspector.breakpoint(BreakpointHit {
                time: SystemTime::now(),
                pid,
                tid,
                tag,
                site,
                caller,
            });
        }
        if ours && !process.detaching() {
            process.proceed(tid, address, context);
        }
        DBG_CONTINUE
    }

    /// すべての対象から離れる。int3 を戻し、外す前に int3 を実行していたスレッドのイベントを受け取りきって
    /// から `DebugActiveProcessStop` を呼ぶ（受け取らずに離れると、そのスレッドは入口 + 1 から走って落ちる）。
    fn detach_all(&mut self) {
        for process in self.processes.values_mut() {
            process.begin_detach();
        }
        let deadline = Instant::now() + DRAIN_LIMIT;
        let mut last_event = Instant::now();
        while !self.processes.is_empty() && Instant::now() < deadline {
            let stepping = self.processes.values().any(Process::stepping);
            if !stepping && last_event.elapsed() >= DRAIN_QUIET {
                break;
            }
            match wait(Duration::from_millis(20)) {
                Ok(Some(event)) => {
                    if self.handle(&event).is_err() {
                        break;
                    }
                    last_event = Instant::now();
                }
                Ok(None) => {}
                Err(_) => break,
            }
        }
        let processes: Vec<Process> = self.processes.drain().map(|(_, p)| p).collect();
        for mut process in processes {
            process.finish_detach();
            self.targets.remove(&process.pid);
            // SAFETY: 引数は値だけ。対象が既に終わっていれば失敗するが、それでよい。
            unsafe { DebugActiveProcessStop(process.pid) };
            self.notify(Notice::Detached { pid: process.pid });
        }
        // CREATE_PROCESS のイベントを受け取る前に止められた分。
        for pid in std::mem::take(&mut self.targets) {
            // SAFETY: 同上。
            unsafe { DebugActiveProcessStop(pid) };
            self.notify(Notice::Detached { pid });
        }
    }
}

impl Drop for Debugger<'_> {
    /// エラーや panic で抜けたときの後始末。対象に int3 を残さない。
    fn drop(&mut self) {
        if self.targets.is_empty() {
            return;
        }
        self.silent = true;
        if let Some((pid, tid, status)) = self.pending.take() {
            // SAFETY: 受け取ったまま続行していないイベントを続行する。
            unsafe { ContinueDebugEvent(pid, tid, status) };
        }
        self.detach_all();
    }
}

/// デバッグイベントを待つ。時間切れなら `None`。
fn wait(timeout: Duration) -> Result<Option<DEBUG_EVENT>, DebugError> {
    let mut event = DEBUG_EVENT::default();
    // SAFETY: 出力先は有効な DEBUG_EVENT。
    if unsafe { WaitForDebugEventEx(&mut event, timeout.as_millis() as u32) } != 0 {
        return Ok(Some(event));
    }
    match last_error() {
        ERROR_SEM_TIMEOUT => Ok(None),
        code => Err(DebugError::Os {
            what: "デバッグイベントの待機",
            code,
        }),
    }
}

/// アタッチする前に調べる、動いているプロセスの様子。
struct Running {
    arch: Option<Arch>,
    /// 作成時刻（FILETIME、100ns 単位）。
    created: Option<u64>,
    /// ほかのデバッガが付いている。
    debugged: bool,
}

/// 作られてからこれだけたてば、初期化（ローダー、WOW64 の切り替え）は終わっているとみなす。経験で決めた値。
const SETTLE_TIME: Duration = Duration::from_millis(500);

/// 作成時刻 `created` のプロセスが [`SETTLE_TIME`] だけたつまでの残り（どちらも FILETIME の 100ns 単位）。
fn settle_delay(created: u64, now: u64) -> Duration {
    let age = Duration::from_nanos(now.saturating_sub(created).saturating_mul(100));
    SETTLE_TIME.saturating_sub(age)
}

fn now_filetime() -> u64 {
    let mut now = windows_sys::Win32::Foundation::FILETIME::default();
    // SAFETY: 出力先は有効な FILETIME。
    unsafe { windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime(&mut now) };
    (u64::from(now.dwHighDateTime) << 32) | u64::from(now.dwLowDateTime)
}

/// 開けなければ `None`（アタッチの失敗で詳しく伝える）。
fn inspect_running(pid: u32) -> Option<Running> {
    // デバッガが付いているかを調べるには PROCESS_QUERY_INFORMATION が要る（LIMITED では調べられず、「付いて
    // いない」と答える）。別のユーザーのプロセスなどで開けなければ、LIMITED で開けるものだけを調べる。
    // SAFETY: 引数は値だけ。失敗すれば NULL。
    let mut handle = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION, 0, pid) };
    if handle.is_null() {
        // SAFETY: 同上。
        handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    }
    if handle.is_null() {
        return None;
    }
    let arch = crate::processes::arch_of(handle);
    let created = creation_time(handle);
    let mut debugged = 0;
    // SAFETY: 出力先は有効な BOOL。調べられなければ 0 のまま（付いていないとみなし、アタッチの誤りに任せる）。
    unsafe { CheckRemoteDebuggerPresent(handle, &mut debugged) };
    let debugged = debugged != 0;
    // SAFETY: 上で開いたハンドルを 1 回だけ閉じる。
    unsafe { CloseHandle(handle) };
    Some(Running {
        arch,
        created,
        debugged,
    })
}

/// exe のファイルの PE ヘッダーから見たアーキテクチャ。読めなければ `None`（起動の失敗で詳しく伝える）。
fn file_arch(exe: &Path) -> Option<Arch> {
    use std::io::Read;
    let mut head = vec![0u8; 4096];
    let mut file = std::fs::File::open(exe).ok()?;
    let length = file.read(&mut head).ok()?;
    head.truncate(length);
    // ヘッダーはファイルの先頭とイメージの先頭で同じ位置にある。
    let memory = BufferMemory::new().with(0, head);
    read_headers(&memory, 0).ok()?.arch
}

/// デバッグイベントでもらったファイルのハンドルからパスを得る。
fn final_path(file: HANDLE) -> String {
    if file.is_null() {
        return String::new();
    }
    let mut buffer = vec![0u16; 1024];
    loop {
        // SAFETY: 出力先は `buffer` で、その長さを渡す。
        let length =
            unsafe { GetFinalPathNameByHandleW(file, buffer.as_mut_ptr(), buffer.len() as u32, FILE_NAME_NORMALIZED) }
                as usize;
        if length == 0 {
            return String::new();
        }
        // 足りないときは、要る長さ（NUL を含む）が返る。
        if length >= buffer.len() {
            buffer.resize(length + 1, 0);
            continue;
        }
        buffer.truncate(length);
        return strip_verbatim(buffer);
    }
}

/// LOAD_DLL で渡されるファイルのハンドルは、デバッガが閉じる約束。
fn close_file(file: HANDLE) {
    if !file.is_null() {
        // SAFETY: デバッグイベントでもらい、こちらが閉じる約束のハンドルを 1 回だけ閉じる。
        unsafe { CloseHandle(file) };
    }
}

/// ファイルのハンドルからパスが取れないときの代わり。`lpImageName` は対象のメモリにある「文字列へのポインター」
/// へのポインターで、どちらも NULL のことがある。
fn image_name(memory: &dyn Memory, arch: Arch, pointer: u64, unicode: bool) -> String {
    if pointer == 0 {
        return String::new();
    }
    let Some(string) = sqltrace_core::memory::read_pointer(memory, pointer, arch) else {
        return String::new();
    };
    let encoding = if unicode { Encoding::Utf16 } else { Encoding::Ansi };
    let options = TextOptions {
        max_bytes: 2048,
        ..TextOptions::default()
    };
    match read_text(memory, string, None, encoding, &WindowsDecoder, &options) {
        Ok(Some(text)) => text.value,
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_only_for_the_rest_of_the_settle_time() {
        let ms = |n: u64| n * 10_000; // 1ms = 10,000 × 100ns
        assert_eq!(settle_delay(ms(1000), ms(1100)), Duration::from_millis(400));
        assert_eq!(settle_delay(ms(1000), ms(1500)), Duration::ZERO);
        assert_eq!(settle_delay(ms(1000), ms(9000)), Duration::ZERO);
        // 時計が戻った（作成時刻が未来）ときは、まるごと待つ。
        assert_eq!(settle_delay(ms(2000), ms(1000)), SETTLE_TIME);
    }
}
