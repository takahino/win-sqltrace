//! 対象のプロセスのメモリの読み書き。

use std::ffi::c_void;

use sqltrace_core::Memory;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Diagnostics::Debug::{FlushInstructionCache, ReadProcessMemory, WriteProcessMemory};
use windows_sys::Win32::System::Memory::{VirtualProtectEx, PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS};

/// 対象のプロセスのメモリ。ハンドルはデバッグイベントでもらったもので、閉じない。
#[derive(Clone, Copy)]
pub struct ProcessMemory {
    handle: HANDLE,
}

impl ProcessMemory {
    pub(crate) fn new(handle: HANDLE) -> Self {
        Self { handle }
    }

    /// スタックなど、書き込みのできるメモリに書く。
    pub(crate) fn write(&self, address: u64, bytes: &[u8]) -> bool {
        let Ok(address) = usize::try_from(address) else {
            return false;
        };
        let mut written = 0usize;
        // SAFETY: 書き込み先は対象のプロセスのアドレス空間で、こちらのメモリは `bytes` だけを読む。
        let ok = unsafe {
            WriteProcessMemory(
                self.handle,
                address as *const c_void,
                bytes.as_ptr().cast(),
                bytes.len(),
                &mut written,
            )
        };
        ok != 0 && written == bytes.len()
    }

    /// コード（読み取り専用のページ）に書く。保護を一時的に外し、命令キャッシュを捨てる。
    pub(crate) fn write_code(&self, address: u64, bytes: &[u8]) -> bool {
        let Ok(at) = usize::try_from(address) else {
            return false;
        };
        let mut old: PAGE_PROTECTION_FLAGS = 0;
        // SAFETY: 対象のプロセスのページの保護を変えるだけで、こちらのメモリは `old` だけを書く。
        let unprotected = unsafe {
            VirtualProtectEx(
                self.handle,
                at as *const c_void,
                bytes.len(),
                PAGE_EXECUTE_READWRITE,
                &mut old,
            )
        } != 0;
        let ok = self.write(address, bytes);
        if unprotected {
            let mut ignored: PAGE_PROTECTION_FLAGS = 0;
            // SAFETY: 上で変えた保護を元に戻す。
            unsafe { VirtualProtectEx(self.handle, at as *const c_void, bytes.len(), old, &mut ignored) };
        }
        // SAFETY: 対象のプロセスの命令キャッシュを捨てるだけ。
        unsafe { FlushInstructionCache(self.handle, at as *const c_void, bytes.len()) };
        ok
    }
}

impl Memory for ProcessMemory {
    fn read(&self, address: u64, buf: &mut [u8]) -> bool {
        let Ok(address) = usize::try_from(address) else {
            return false;
        };
        let mut read = 0usize;
        // SAFETY: 書き込み先は `buf` で、その長さだけを渡す。
        let ok = unsafe {
            ReadProcessMemory(
                self.handle,
                address as *const c_void,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut read,
            )
        };
        ok != 0 && read == buf.len()
    }
}
