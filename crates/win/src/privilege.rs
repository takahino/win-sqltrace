//! SeDebugPrivilege を有効にする（管理者として動いているとき、別のユーザーのプロセスにアタッチできる）。

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LUID};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_DEBUG_NAME, SE_PRIVILEGE_ENABLED,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::error::last_error;

/// 有効にできたら `true`。管理者でなければ持っていないので `false`（そのままでも自分のプロセスには付ける）。
pub fn enable_debug_privilege() -> bool {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: 自分のプロセスのトークンを開くだけ。
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES, &mut token) } == 0 {
        return false;
    }
    let mut luid = LUID::default();
    // SAFETY: 名前は windows-sys の定数（NUL 終端）で、出力先は有効な LUID。
    let found = unsafe { LookupPrivilegeValueW(std::ptr::null(), SE_DEBUG_NAME, &mut luid) } != 0;
    let mut enabled = false;
    if found {
        let privileges = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        // SAFETY: 1 件の特権を渡し、前の状態は受け取らない。
        let ok =
            unsafe { AdjustTokenPrivileges(token, 0, &privileges, 0, std::ptr::null_mut(), std::ptr::null_mut()) } != 0;
        // 一部だけ付けられなかったときも成功を返すので、最後のエラーで確かめる（ERROR_NOT_ALL_ASSIGNED = 1300）。
        enabled = ok && last_error() != 1300;
    }
    // SAFETY: 上で開いたハンドルを 1 回だけ閉じる。
    unsafe { CloseHandle(token) };
    enabled
}
