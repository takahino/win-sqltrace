//! 試験用の oci.dll。OCI（8 以降）と OCI7 の、sqlTrace が捕まえる入口だけを持つ。何もせずに成功を返す。
//!
//! 関数の型は Oracle のヘッダー（oci.h、ociapr.h）に合わせ、呼び出し規約は本物と同じ cdecl にする。
//! 引数は読まない（sqlTrace がデバッガとして入口で読む）。ただし OCIStmtPrepare2 は文のハンドルを返す。
#![cfg(windows)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

type Handle = *mut c_void;

const OCI_SUCCESS: i32 = 0;

/// 呼ばれた回数。関数ごとに違う変数を数えるのは、中身が同じ関数をリンカーが 1 つにまとめないようにするため
/// （まとめられると別名になり、ブレークポイントが 1 つしか置けない）。
static CALLS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];

fn count(index: usize) -> i32 {
    CALLS[index].fetch_add(1, Ordering::Relaxed);
    OCI_SUCCESS
}

/// OCIStmtPrepare2 が返す文のハンドル（本物では確保した構造体。試験では値だけでよい）。
static NEXT_STATEMENT: AtomicUsize = AtomicUsize::new(0x5000);

#[no_mangle]
pub extern "C" fn OCIStmtPrepare(
    _stmtp: Handle,
    _errhp: Handle,
    _stmt: *const u8,
    _stmt_len: u32,
    _language: u32,
    _mode: u32,
) -> i32 {
    count(0)
}

/// # Safety
/// `stmtp` は書き込める文のハンドルの置き場所であること。
#[no_mangle]
pub unsafe extern "C" fn OCIStmtPrepare2(
    _svchp: Handle,
    stmtp: *mut Handle,
    _errhp: Handle,
    _stmt: *const u8,
    _stmt_len: u32,
    _key: *const u8,
    _key_len: u32,
    _language: u32,
    _mode: u32,
) -> i32 {
    let statement = NEXT_STATEMENT.fetch_add(1, Ordering::Relaxed);
    // SAFETY: 呼び出し側の約束（上の Safety）。
    unsafe { *stmtp = statement as Handle };
    count(1)
}

#[no_mangle]
#[allow(clippy::too_many_arguments)] // OCI の宣言どおり。
pub extern "C" fn OCIStmtExecute(
    _svchp: Handle,
    _stmtp: Handle,
    _errhp: Handle,
    _iters: u32,
    _rowoff: u32,
    _snap_in: Handle,
    _snap_out: Handle,
    _mode: u32,
) -> i32 {
    count(2)
}

// OCI7。cda は呼び出し側が持つ構造体（Cursor Data Area）で、これがハンドルの役をする。

#[no_mangle]
pub extern "C" fn oparse(_cda: Handle, _sqlstm: *const u8, _sqllen: i32, _defflg: i32, _lngflg: u32) -> i32 {
    count(3)
}

#[no_mangle]
pub extern "C" fn osql3(_cda: Handle, _sqlstm: *const u8, _sqllen: i32) -> i32 {
    count(4)
}

#[no_mangle]
pub extern "C" fn oexec(_cda: Handle) -> i32 {
    count(5)
}

#[no_mangle]
pub extern "C" fn oexn(_cda: Handle, _iters: i32, _rowoff: i32) -> i32 {
    count(6)
}

#[no_mangle]
pub extern "C" fn oexfet(_cda: Handle, _nrows: u32, _cancel: i32, _exact: i32) -> i32 {
    count(7)
}
