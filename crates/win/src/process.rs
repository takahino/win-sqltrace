//! デバッグ中の 1 つのプロセスの状態: スレッド、モジュール、ブレークポイントと、その状態機械。
//!
//! ブレークポイントで止めた後の続け方は 2 通り。
//! - 入口の命令をエミュレーションできる（[`sqltrace_core::prologue`]）なら、int3 を書いたまま命令の効果だけを
//!   コンテキストに施して続ける。ほかのスレッドは止めない。
//! - できなければ、元のバイトを戻して TF で 1 命令だけ実行させ、単一ステップの例外で int3 を書き直す。
//!   その間はほかのスレッドをすべて中断する（戻したバイトの上を素通りさせないため）。ステップさせるのは
//!   同時に 1 本だけで、その間に別のスレッドの bp のイベントが届いたら、順番待ちに入れて後でステップさせる。

use std::collections::{BTreeMap, HashMap, VecDeque};

use sqltrace_core::prologue::{self, Emulation};
use sqltrace_core::{Arch, CallerInfo, Memory};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Threading::{ResumeThread, SuspendThread, TerminateProcess};

use crate::context::{ContextKind, ThreadContext};
use crate::inspector::BreakpointRequest;
use crate::journal::Journal;
use crate::memory::ProcessMemory;

pub(crate) const INT3: u8 = 0xCC;
/// エミュレーションの判定に読む、入口のバイト数。
const ENTRY_BYTES: usize = 16;

struct Module {
    size: u32,
    name: String,
}

struct Breakpoint {
    original: u8,
    tag: usize,
    emulation: Option<Emulation>,
}

#[derive(Clone, Copy)]
struct Step {
    tid: u32,
    address: u64,
}

/// 例外のアドレスにあったのは何の int3 か。
pub(crate) enum BreakpointKind {
    Ours(usize),
    /// デタッチのために外した後に届いた（外す前に int3 を実行していた）。
    Retired(usize),
    /// ntdll がデバッガに向けて出す int3（アタッチと起動の初期ブレークポイントなど）。続行するだけでよい。
    /// 回数は決まっていない: 初期化の途中の WOW64 のプロセスにアタッチすると 2 回来ることがあり、1 回目だけを
    /// 続行して 2 回目を対象に返すと、対象が落ちる。
    System,
    /// 対象自身の int3（`__debugbreak` など）。対象の例外処理に渡す。
    Foreign,
}

pub(crate) struct Process {
    pub(crate) pid: u32,
    handle: HANDLE,
    pub(crate) arch: Arch,
    /// レジスタの読み方（同じアーキテクチャか WOW64 か）。
    pub(crate) context: ContextKind,
    pub(crate) memory: ProcessMemory,
    threads: HashMap<u32, HANDLE>,
    modules: BTreeMap<u64, Module>,
    breakpoints: HashMap<u64, Breakpoint>,
    retired: HashMap<u64, usize>,
    step: Option<Step>,
    waiting: VecDeque<Step>,
    /// こちらが中断したスレッド（それぞれ 1 回だけ中断し、1 回だけ再開する）。
    suspended: Vec<u32>,
    journal: Option<Journal>,
    detaching: bool,
    /// 入口の命令をエミュレーションする（切ると必ずステップ実行になる。試験と切り分け用）。
    emulate: bool,
    /// 呼び出し側に伝える警告。デバッガがイベントごとに取り出す。
    pub(crate) warnings: Vec<String>,
}

impl Process {
    pub(crate) fn new(
        pid: u32,
        handle: HANDLE,
        arch: Arch,
        context: ContextKind,
        journal: Option<Journal>,
        emulate: bool,
    ) -> Self {
        Self {
            pid,
            handle,
            arch,
            context,
            memory: ProcessMemory::new(handle),
            threads: HashMap::new(),
            modules: BTreeMap::new(),
            breakpoints: HashMap::new(),
            retired: HashMap::new(),
            step: None,
            waiting: VecDeque::new(),
            suspended: Vec::new(),
            emulate,
            journal,
            detaching: false,
            warnings: Vec::new(),
        }
    }

    pub(crate) fn detaching(&self) -> bool {
        self.detaching
    }

    pub(crate) fn thread(&self, tid: u32) -> Option<HANDLE> {
        self.threads.get(&tid).copied()
    }

    pub(crate) fn add_thread(&mut self, tid: u32, handle: HANDLE) {
        self.threads.insert(tid, handle);
        // ステップ中に生まれたスレッドも、戻したバイトの上を通らないように止める。
        if self.step.is_some() && !self.detaching {
            self.suspend(tid);
        }
    }

    pub(crate) fn remove_thread(&mut self, tid: u32) {
        // 終わるスレッドのハンドルは OS が閉じる。中断の記録だけを消す。
        self.threads.remove(&tid);
        self.suspended.retain(|&t| t != tid);
        self.waiting.retain(|s| s.tid != tid);
        if self.step.is_some_and(|s| s.tid == tid) {
            self.finish_step();
        }
    }

    pub(crate) fn add_module(&mut self, base: u64, size: u32, name: String) {
        self.modules.insert(base, Module { size, name });
    }

    /// モジュールが外された。その中のブレークポイントは書き戻さずに捨てる（メモリはもうない）。
    pub(crate) fn unload(&mut self, base: u64) -> Option<String> {
        let module = self.modules.remove(&base)?;
        let end = base + u64::from(module.size);
        let inside = |address: u64| (base..end).contains(&address);
        let before = self.breakpoints.len();
        self.breakpoints.retain(|&a, _| !inside(a));
        self.retired.retain(|&a, _| !inside(a));
        self.waiting.retain(|s| !inside(s.address));
        if self.breakpoints.len() != before {
            self.rewrite_journal();
        }
        Some(module.name)
    }

    fn module_at(&self, address: u64) -> Option<(u64, &Module)> {
        let (&base, module) = self.modules.range(..=address).next_back()?;
        (address < base + u64::from(module.size)).then_some((base, module))
    }

    pub(crate) fn caller(&self, address: u64) -> CallerInfo {
        match self.module_at(address) {
            Some((base, module)) => CallerInfo {
                address,
                module: Some(module.name.clone()),
                offset: address - base,
            },
            None => CallerInfo {
                address,
                module: None,
                offset: address,
            },
        }
    }

    /// ブレークポイントを置き、置けたものの `tag` を返す。同じアドレス（別名）は 1 つにまとめる。
    pub(crate) fn arm(&mut self, requests: &[BreakpointRequest]) -> Vec<usize> {
        let mut planned: Vec<(BreakpointRequest, Vec<u8>)> = Vec::new();
        for &request in requests {
            let duplicate = self.breakpoints.contains_key(&request.address)
                || planned.iter().any(|(p, _)| p.address == request.address);
            if duplicate {
                continue;
            }
            let Some(bytes) = self.read_entry(request.address) else {
                self.warnings.push(format!(
                    "0x{:x} を読めないのでブレークポイントを置けません",
                    request.address
                ));
                continue;
            };
            if bytes[0] == INT3 {
                // 別のデバッガや、前回の sqltrace が残したもの（`sqltrace repair` で戻せる）。
                self.warnings.push(format!(
                    "0x{:x} は既に int3 です。前回の sqltrace が残した可能性があります（sqltrace repair {}）",
                    request.address, self.pid
                ));
                continue;
            }
            planned.push((request, bytes));
        }
        // 書く前にジャーナルに残す（書いた直後に落ちても戻せるように）。
        let entries: Vec<(u64, u8)> = planned.iter().map(|(r, b)| (r.address, b[0])).collect();
        if let Some(journal) = &mut self.journal {
            if let Err(error) = journal.record(&entries) {
                self.warnings.push(format!(
                    "ジャーナルに書けませんでした（{error}）。強制終了されると修復できません"
                ));
            }
        }
        let mut armed = Vec::new();
        for (request, bytes) in planned {
            if !self.memory.write_code(request.address, &[INT3]) {
                self.warnings
                    .push(format!("0x{:x} にブレークポイントを書けませんでした", request.address));
                continue;
            }
            self.breakpoints.insert(
                request.address,
                Breakpoint {
                    original: bytes[0],
                    tag: request.tag,
                    emulation: self.emulate.then(|| prologue::decode(self.arch, &bytes)).flatten(),
                },
            );
            armed.push(request.tag);
        }
        armed
    }

    /// 入口のバイト列。モジュールの末尾で 16 バイト読めなければ 1 バイトだけ読む。
    fn read_entry(&self, address: u64) -> Option<Vec<u8>> {
        let mut bytes = vec![0u8; ENTRY_BYTES];
        if self.memory.read(address, &mut bytes) {
            return Some(bytes);
        }
        let mut one = vec![0u8; 1];
        self.memory.read(address, &mut one).then_some(one)
    }

    pub(crate) fn classify(&self, address: u64) -> BreakpointKind {
        if let Some(bp) = self.breakpoints.get(&address) {
            return BreakpointKind::Ours(bp.tag);
        }
        if let Some(&tag) = self.retired.get(&address) {
            return BreakpointKind::Retired(tag);
        }
        let in_ntdll = self
            .module_at(address)
            .is_some_and(|(_, m)| m.name.eq_ignore_ascii_case("ntdll.dll"));
        if in_ntdll {
            return BreakpointKind::System;
        }
        BreakpointKind::Foreign
    }

    /// 入口で止めたスレッドを、関数の最初の命令から続けられるようにする。`context` の IP は入口を指している。
    pub(crate) fn proceed(&mut self, tid: u32, address: u64, mut context: ThreadContext) {
        let (Some(thread), Some(bp)) = (self.thread(tid), self.breakpoints.get(&address)) else {
            return;
        };
        if let Some(emulation) = bp.emulation {
            let memory = self.memory;
            if emulation.apply(self.arch, &mut context, &mut |a, b| memory.write(a, b)) && context.set(thread) {
                return;
            }
        }
        if self.step.is_some() {
            // ほかのスレッドがステップ中。このスレッドは止めておき、順番が来たらステップさせる。
            self.suspend(tid);
            self.waiting.push_back(Step { tid, address });
            return;
        }
        self.start_step(Step { tid, address });
    }

    fn start_step(&mut self, step: Step) {
        let (Some(thread), Some(bp)) = (self.thread(step.tid), self.breakpoints.get(&step.address)) else {
            self.resume_all();
            return;
        };
        let original = bp.original;
        let Some(mut context) = ThreadContext::get(thread, self.context) else {
            self.warnings.push(format!(
                "スレッド {} のレジスタを読めずステップ実行できません",
                step.tid
            ));
            self.resume_all();
            return;
        };
        if !self.memory.write_code(step.address, &[original]) {
            self.warnings.push(format!(
                "0x{:x} の元のバイトを戻せずステップ実行できません",
                step.address
            ));
            self.resume_all();
            return;
        }
        context.set_trap_flag(true);
        if !context.set(thread) {
            // TF を立てられなければ、元に戻したまま int3 を書き直せない。外したまま続ける。
            self.warnings.push(format!(
                "スレッド {} の TF を立てられません。0x{:x} のブレークポイントを外しました",
                step.tid, step.address
            ));
            self.breakpoints.remove(&step.address);
            self.rewrite_journal();
            self.resume_all();
            return;
        }
        self.resume(step.tid);
        let others: Vec<u32> = self.threads.keys().copied().filter(|&t| t != step.tid).collect();
        for tid in others {
            self.suspend(tid);
        }
        self.step = Some(step);
    }

    /// 単一ステップの例外。こちらがステップさせたスレッドなら `true`。
    pub(crate) fn on_single_step(&mut self, tid: u32) -> bool {
        if self.step.is_none_or(|s| s.tid != tid) {
            return false;
        }
        self.finish_step();
        true
    }

    /// ほかの例外。ステップさせた命令が例外を起こしたなら、ステップを終わらせる（TF を残さない）。
    pub(crate) fn on_other_exception(&mut self, tid: u32) {
        if self.step.is_some_and(|s| s.tid == tid) {
            self.finish_step();
        }
    }

    fn finish_step(&mut self) {
        let Some(step) = self.step.take() else {
            return;
        };
        if let Some(thread) = self.thread(step.tid) {
            if let Some(mut context) = ThreadContext::get(thread, self.context) {
                if context.trap_flag() {
                    context.set_trap_flag(false);
                    context.set(thread);
                }
            }
        }
        if !self.detaching && self.breakpoints.contains_key(&step.address) {
            self.memory.write_code(step.address, &[INT3]);
        }
        while let Some(next) = self.waiting.pop_front() {
            if !self.threads.contains_key(&next.tid) || !self.breakpoints.contains_key(&next.address) {
                continue;
            }
            if self.threads.contains_key(&step.tid) {
                self.suspend(step.tid);
            }
            self.start_step(next);
            return;
        }
        self.resume_all();
    }

    pub(crate) fn stepping(&self) -> bool {
        self.step.is_some()
    }

    fn suspend(&mut self, tid: u32) {
        if self.suspended.contains(&tid) {
            return;
        }
        let Some(thread) = self.thread(tid) else {
            return;
        };
        // SAFETY: デバッグイベントでもらったスレッドのハンドル。
        if unsafe { SuspendThread(thread) } != u32::MAX {
            self.suspended.push(tid);
        }
    }

    fn resume(&mut self, tid: u32) {
        let Some(index) = self.suspended.iter().position(|&t| t == tid) else {
            return;
        };
        self.suspended.swap_remove(index);
        if let Some(thread) = self.thread(tid) {
            // SAFETY: こちらが中断したスレッドを 1 回だけ再開する。
            unsafe { ResumeThread(thread) };
        }
    }

    fn resume_all(&mut self) {
        for tid in std::mem::take(&mut self.suspended) {
            if let Some(thread) = self.thread(tid) {
                // SAFETY: こちらが中断したスレッドを 1 回だけ再開する。
                unsafe { ResumeThread(thread) };
            }
        }
    }

    /// デタッチの前半: 今 int3 になっているところだけを元に戻し、止めたスレッドを再開する。
    /// この後に届く bp のイベント（外す前に int3 を実行していた）は [`BreakpointKind::Retired`] になる。
    pub(crate) fn begin_detach(&mut self) {
        self.detaching = true;
        for (address, bp) in std::mem::take(&mut self.breakpoints) {
            let mut current = [0u8];
            if self.memory.read(address, &mut current) && current[0] == INT3 {
                self.memory.write_code(address, &[bp.original]);
            }
            self.retired.insert(address, bp.tag);
        }
        // 順番待ちのスレッドは IP が入口を指し、元のバイトも戻ったので、そのまま走らせてよい。
        self.waiting.clear();
        self.resume_all();
    }

    /// デタッチの後半: ステップの完了が届かなかったスレッドの TF を落とす（デバッガのいない対象で単一ステップの
    /// 例外が起きると落ちる）。
    pub(crate) fn finish_detach(&mut self) {
        if let Some(step) = self.step.take() {
            if let Some(thread) = self.thread(step.tid) {
                // SAFETY: 走っているスレッドのコンテキストを書き換える間だけ中断する。
                unsafe { SuspendThread(thread) };
                if let Some(mut context) = ThreadContext::get(thread, self.context) {
                    context.set_trap_flag(false);
                    context.set(thread);
                }
                // SAFETY: 直前に中断した分を再開する。
                unsafe { ResumeThread(thread) };
            }
        }
        if let Some(journal) = self.journal.take() {
            journal.remove();
        }
    }

    pub(crate) fn exited(&mut self) {
        if let Some(journal) = self.journal.take() {
            journal.remove();
        }
    }

    pub(crate) fn terminate(&self) -> bool {
        // SAFETY: デバッグイベントでもらったプロセスのハンドル。
        unsafe { TerminateProcess(self.handle, 1) != 0 }
    }

    fn rewrite_journal(&mut self) {
        let entries: Vec<(u64, u8)> = self.breakpoints.iter().map(|(&a, b)| (a, b.original)).collect();
        if let Some(journal) = &mut self.journal {
            if let Err(error) = journal.rewrite(&entries) {
                self.warnings
                    .push(format!("ジャーナルを書き直せませんでした（{error}）"));
            }
        }
    }
}
