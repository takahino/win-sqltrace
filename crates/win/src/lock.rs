//! 1 つのプロセスをトレースする sqltrace は 1 つまで、にするための名前付きの mutex（PID ごと）。
//!
//! 同じ PID への 2 つ目のデバッガは Windows がもともと拒否するが、その誤りは「アクセスが拒否されました」で
//! 理由が分からない。sqltrace どうしならアタッチの前にここで見分け、相手の sqltrace の PID も伝える。
//! mutex は持っている sqltrace が終われば（強制終了でも）OS が片付けるので、ロックが残り続けることはない。
//! 名前は `Local\`（同じログオンのセッション）に置く。`Global\` は一般の利用者では作れない。

use windows_sys::core::PCWSTR;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, HANDLE};
use windows_sys::Win32::System::Threading::{CreateMutexW, OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};

use crate::error::last_error;

fn name(pid: u32) -> Vec<u16> {
    format!("Local\\sqltrace-pid-{pid}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

/// その PID をトレースしている間、持ち続ける。落とすと手放す。
pub(crate) struct TargetLock {
    handle: HANDLE,
}

impl TargetLock {
    /// 取れなければ `None`（ほかの sqltrace がその PID をトレースしている）。
    pub(crate) fn acquire(pid: u32) -> Option<Self> {
        let name = name(pid);
        // 持ち主になる必要はない（名前の付いた mutex があること自体が「使用中」の印）。持ち主にすると、
        // 作ったスレッドが終わったときに「放棄」の扱いになり、意味が変わってしまう。
        // SAFETY: 名前は NUL 終端。セキュリティ属性は既定。
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr() as PCWSTR) };
        let error = last_error();
        if handle.is_null() {
            // 管理者で動いている sqltrace の mutex は、一般の利用者からは開けない（使用中とみなす）。
            return None;
        }
        if error == ERROR_ALREADY_EXISTS {
            // SAFETY: 上で開いたハンドルを 1 回だけ閉じる。
            unsafe { CloseHandle(handle) };
            return None;
        }
        Some(Self { handle })
    }
}

impl Drop for TargetLock {
    fn drop(&mut self) {
        // SAFETY: acquire で作ったハンドルを 1 回だけ閉じる。
        unsafe { CloseHandle(self.handle) };
    }
}

/// ほかの sqltrace がその PID をトレースしているか（repair が、動いている sqltrace の邪魔をしないため）。
pub(crate) fn is_held(pid: u32) -> bool {
    let name = name(pid);
    // SAFETY: 名前は NUL 終端。開くだけで、何も変えない。
    let handle = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr()) };
    if handle.is_null() {
        return last_error() == ERROR_ACCESS_DENIED;
    }
    // SAFETY: 上で開いたハンドルを 1 回だけ閉じる。
    unsafe { CloseHandle(handle) };
    true
}
