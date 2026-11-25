//! スレッドのコンテキスト（レジスタ）の読み書き。アーキテクチャごとの違いはここに閉じる。
//!
//! 種類は 2 つ。
//! - [`ContextKind::Native`]: デバッガと同じアーキテクチャのプロセス（x64 版なら x64、x86 版なら x86）。
//! - [`ContextKind::Wow64`]: x64 版から見た 32 ビットのプロセス。32 ビットのレジスタは
//!   `Wow64GetThreadContext` で読む（`GetThreadContext` では 64 ビット側の状態しか見えない）。

use sqltrace_core::prologue::Cpu;
use sqltrace_core::Registers;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Diagnostics::Debug::{GetThreadContext, SetThreadContext, CONTEXT};

/// EFLAGS の TF。立てると 1 命令ごとに単一ステップの例外が起きる。
const TRAP_FLAG: u32 = 0x100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContextKind {
    Native,
    #[cfg(target_arch = "x86_64")]
    Wow64,
}

/// x64 の `CONTEXT` は 16 バイト境界に置かないと `GetThreadContext` が失敗する。windows-sys の型は
/// 境界を指定していないので包む。
#[repr(C, align(16))]
struct Aligned(CONTEXT);

enum Inner {
    Native(Box<Aligned>),
    #[cfg(target_arch = "x86_64")]
    Wow64(Box<windows_sys::Win32::System::Diagnostics::Debug::WOW64_CONTEXT>),
}

pub(crate) struct ThreadContext {
    inner: Inner,
}

/// x86 のレジスタの並び（x86 版の `CONTEXT` と `WOW64_CONTEXT` はフィールドの名前が同じ）。
macro_rules! x86_access {
    ($module:ident, $context:ty) => {
        mod $module {
            type Context = $context;

            fn slot(c: &mut Context, index: u8) -> &mut u32 {
                match index & 7 {
                    0 => &mut c.Eax,
                    1 => &mut c.Ecx,
                    2 => &mut c.Edx,
                    3 => &mut c.Ebx,
                    4 => &mut c.Esp,
                    5 => &mut c.Ebp,
                    6 => &mut c.Esi,
                    _ => &mut c.Edi,
                }
            }

            pub fn register(c: &Context, index: u8) -> u64 {
                let mut copy = *c;
                u64::from(*slot(&mut copy, index))
            }

            pub fn set_register(c: &mut Context, index: u8, value: u64) {
                // x86 のレジスタは 32 ビット。上位は捨てる。
                *slot(c, index) = value as u32;
            }

            pub fn ip(c: &Context) -> u64 {
                u64::from(c.Eip)
            }

            pub fn set_ip(c: &mut Context, value: u64) {
                c.Eip = value as u32;
            }

            pub fn flags(c: &Context) -> u32 {
                c.EFlags
            }

            pub fn set_flags(c: &mut Context, value: u32) {
                c.EFlags = value;
            }

            /// x86 では引数をレジスタで渡さない（スタックから読む）。
            pub fn args(_: &Context) -> [u64; 4] {
                [0; 4]
            }
        }
    };
}

#[cfg(target_arch = "x86")]
x86_access!(native, windows_sys::Win32::System::Diagnostics::Debug::CONTEXT);

#[cfg(target_arch = "x86")]
const NATIVE_FLAGS: u32 = windows_sys::Win32::System::Diagnostics::Debug::CONTEXT_CONTROL_X86
    | windows_sys::Win32::System::Diagnostics::Debug::CONTEXT_INTEGER_X86;

#[cfg(target_arch = "x86_64")]
x86_access!(wow64, windows_sys::Win32::System::Diagnostics::Debug::WOW64_CONTEXT);

#[cfg(target_arch = "x86_64")]
const NATIVE_FLAGS: u32 = windows_sys::Win32::System::Diagnostics::Debug::CONTEXT_CONTROL_AMD64
    | windows_sys::Win32::System::Diagnostics::Debug::CONTEXT_INTEGER_AMD64;

#[cfg(target_arch = "x86_64")]
mod native {
    use windows_sys::Win32::System::Diagnostics::Debug::CONTEXT;

    fn slot(c: &mut CONTEXT, index: u8) -> &mut u64 {
        match index {
            0 => &mut c.Rax,
            1 => &mut c.Rcx,
            2 => &mut c.Rdx,
            3 => &mut c.Rbx,
            4 => &mut c.Rsp,
            5 => &mut c.Rbp,
            6 => &mut c.Rsi,
            7 => &mut c.Rdi,
            8 => &mut c.R8,
            9 => &mut c.R9,
            10 => &mut c.R10,
            11 => &mut c.R11,
            12 => &mut c.R12,
            13 => &mut c.R13,
            14 => &mut c.R14,
            _ => &mut c.R15,
        }
    }

    pub fn register(c: &CONTEXT, index: u8) -> u64 {
        let mut copy = *c;
        *slot(&mut copy, index)
    }

    pub fn set_register(c: &mut CONTEXT, index: u8, value: u64) {
        *slot(c, index) = value;
    }

    pub fn ip(c: &CONTEXT) -> u64 {
        c.Rip
    }

    pub fn set_ip(c: &mut CONTEXT, value: u64) {
        c.Rip = value;
    }

    pub fn flags(c: &CONTEXT) -> u32 {
        c.EFlags
    }

    pub fn set_flags(c: &mut CONTEXT, value: u32) {
        c.EFlags = value;
    }

    /// Win64 の最初の 4 つの引数。
    pub fn args(c: &CONTEXT) -> [u64; 4] {
        [c.Rcx, c.Rdx, c.R8, c.R9]
    }
}

/// 種類ごとの関数を呼び分ける。
macro_rules! dispatch {
    ($self:expr, $function:ident ( $($arg:expr),* )) => {
        match &$self.inner {
            Inner::Native(c) => native::$function(&c.0 $(, $arg)*),
            #[cfg(target_arch = "x86_64")]
            Inner::Wow64(c) => wow64::$function(c $(, $arg)*),
        }
    };
    (mut $self:expr, $function:ident ( $($arg:expr),* )) => {
        match &mut $self.inner {
            Inner::Native(c) => native::$function(&mut c.0 $(, $arg)*),
            #[cfg(target_arch = "x86_64")]
            Inner::Wow64(c) => wow64::$function(c $(, $arg)*),
        }
    };
}

impl ThreadContext {
    /// 止まっている（デバッグイベントを報告中か、中断した）スレッドのコンテキストを読む。
    pub(crate) fn get(thread: HANDLE, kind: ContextKind) -> Option<Self> {
        match kind {
            ContextKind::Native => {
                // SAFETY: CONTEXT は整数と配列だけの C の構造体で、すべて 0 は有効な値。
                let mut c: Box<Aligned> = Box::new(Aligned(unsafe { std::mem::zeroed() }));
                c.0.ContextFlags = NATIVE_FLAGS;
                // SAFETY: `c` は 16 バイト境界にある CONTEXT で、ContextFlags で読む範囲を指定してある。
                let ok = unsafe { GetThreadContext(thread, &mut c.0) } != 0;
                ok.then_some(Self {
                    inner: Inner::Native(c),
                })
            }
            #[cfg(target_arch = "x86_64")]
            ContextKind::Wow64 => {
                use windows_sys::Win32::System::Diagnostics::Debug::{
                    Wow64GetThreadContext, WOW64_CONTEXT, WOW64_CONTEXT_CONTROL, WOW64_CONTEXT_INTEGER,
                };
                // SAFETY: WOW64_CONTEXT は整数と配列だけの C の構造体で、すべて 0 は有効な値。
                let mut c: Box<WOW64_CONTEXT> = Box::new(unsafe { std::mem::zeroed() });
                c.ContextFlags = WOW64_CONTEXT_CONTROL | WOW64_CONTEXT_INTEGER;
                // SAFETY: 出力先は有効な WOW64_CONTEXT で、ContextFlags で読む範囲を指定してある。
                let ok = unsafe { Wow64GetThreadContext(thread, &mut *c) } != 0;
                ok.then_some(Self { inner: Inner::Wow64(c) })
            }
        }
    }

    pub(crate) fn set(&self, thread: HANDLE) -> bool {
        match &self.inner {
            // SAFETY: `get` で読んだコンテキストを（一部を変えて）書き戻すだけ。
            Inner::Native(c) => unsafe { SetThreadContext(thread, &c.0) != 0 },
            #[cfg(target_arch = "x86_64")]
            // SAFETY: 同上。
            Inner::Wow64(c) => unsafe {
                windows_sys::Win32::System::Diagnostics::Debug::Wow64SetThreadContext(thread, &**c) != 0
            },
        }
    }

    pub(crate) fn registers(&self) -> Registers {
        Registers {
            ip: dispatch!(self, ip()),
            sp: dispatch!(self, register(4)),
            args: dispatch!(self, args()),
        }
    }

    pub(crate) fn trap_flag(&self) -> bool {
        dispatch!(self, flags()) & TRAP_FLAG != 0
    }

    pub(crate) fn set_trap_flag(&mut self, on: bool) {
        let flags = dispatch!(self, flags());
        let flags = if on { flags | TRAP_FLAG } else { flags & !TRAP_FLAG };
        dispatch!(mut self, set_flags(flags));
    }
}

impl Cpu for ThreadContext {
    fn register(&self, index: u8) -> u64 {
        dispatch!(self, register(index))
    }

    fn set_register(&mut self, index: u8, value: u64) {
        dispatch!(mut self, set_register(index, value));
    }

    fn ip(&self) -> u64 {
        dispatch!(self, ip())
    }

    fn set_ip(&mut self, value: u64) {
        dispatch!(mut self, set_ip(value));
    }
}
