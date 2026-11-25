//! 試験用の sqora32.dll（Oracle の ODBC ドライバの名前）。中から oci.dll の OCIStmtPrepare と
//! OCIStmtExecute を呼ぶ。本物のドライバと同じく、ODBC で捕まえた呼び出しの内側で OCI が呼ばれる形を作り、
//! その内側の分を sqlTrace が重複として捨てることを試す。
#![cfg(windows)]

use std::ffi::c_void;

use windows_sys::core::{s, w};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

type Prepare = unsafe extern "C" fn(*mut c_void, *mut c_void, *const u8, u32, u32, u32) -> i32;
type Execute =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, u32, u32, *mut c_void, *mut c_void, u32) -> i32;

/// `stmt` に `sql`（`len` バイト）を準備して実行する。oci.dll は呼び出し側が先に読み込んでおくこと。
///
/// # Safety
/// `sql` は `len` バイト読めること。
#[no_mangle]
pub unsafe extern "C" fn driver_execute(stmt: *mut c_void, sql: *const u8, len: u32) -> i32 {
    // SAFETY: 読み込み済みの oci.dll の関数を、その宣言どおりの型で呼ぶ。
    unsafe {
        let module = GetModuleHandleW(w!("oci.dll"));
        if module.is_null() {
            return -1;
        }
        let (Some(prepare), Some(execute)) = (
            GetProcAddress(module, s!("OCIStmtPrepare")),
            GetProcAddress(module, s!("OCIStmtExecute")),
        ) else {
            return -1;
        };
        let prepare: Prepare = std::mem::transmute::<unsafe extern "system" fn() -> isize, Prepare>(prepare);
        let execute: Execute = std::mem::transmute::<unsafe extern "system" fn() -> isize, Execute>(execute);
        let null = std::ptr::null_mut();
        prepare(stmt, null, sql, len, 1, 0);
        execute(null, stmt, null, 1, 0, null, null, 0)
    }
}
