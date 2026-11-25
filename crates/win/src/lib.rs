//! sqlTrace の Windows のデバッグ層。
//!
//! SQL のことは知らない。Windows のデバッグ API で対象にアタッチ（または起動）し、[`Inspector`] が
//! 求めたアドレスに int3 を置き、止まったらレジスタとメモリを渡して続行する。何を捕まえ、どう読むかは
//! 呼び出し側（session）が `sqltrace-core` で決める。
#![cfg(windows)]

mod clock;
mod codepage;
mod console;
mod context;
mod debugger;
mod dialog;
mod error;
mod inspector;
mod journal;
mod launch;
mod lock;
mod memory;
mod picker;
mod privilege;
mod process;
mod processes;

pub use clock::utc_offset_minutes;
pub use codepage::WindowsDecoder;
pub use console::{enable_console_colors, install_console_handler};
pub use debugger::{can_trace, host_arch, trace};
pub use dialog::{open_file, save_file, Filter};
pub use error::DebugError;
pub use inspector::{
    BreakpointHit, BreakpointRequest, Inspector, ModuleLoaded, Notice, StopRequest, StopSignal, TraceOptions,
};
pub use journal::{journal_dir, repair, RepairError, RepairReport};
pub use launch::quote_argument;
pub use memory::ProcessMemory;
pub use picker::{left_button_down, window_under_cursor, PickedWindow};
pub use privilege::enable_debug_privilege;
pub use processes::{list_processes, ProcessInfo};
