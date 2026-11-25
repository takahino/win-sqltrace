//! 動いているプロセスの一覧（GUI でアタッチの相手を選ぶため）。

use sqltrace_core::Arch;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::SystemInformation::{IMAGE_FILE_MACHINE, IMAGE_FILE_MACHINE_UNKNOWN};
use windows_sys::Win32::System::Threading::{
    IsWow64Process2, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::launch::strip_verbatim;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub parent_pid: u32,
    /// exe のファイル名。
    pub name: String,
    /// exe のフルパス。開けないプロセス（別のユーザー、保護されたもの）では `None`。
    pub path: Option<String>,
    /// 開けないプロセスでは `None`。
    pub arch: Option<Arch>,
}

impl ProcessInfo {
    /// この sqltrace でトレースできるアーキテクチャか（開けなければ分からないので `false`）。
    pub fn traceable(&self) -> bool {
        self.arch.is_some_and(crate::debugger::can_trace)
    }
}

/// 開いたプロセスのアーキテクチャ（WOW64 なら x86）。
pub(crate) fn arch_of(process: HANDLE) -> Option<Arch> {
    let mut machine: IMAGE_FILE_MACHINE = 0;
    let mut native: IMAGE_FILE_MACHINE = 0;
    // SAFETY: 出力先はどちらも有効な u16。
    if unsafe { IsWow64Process2(process, &mut machine, &mut native) } == 0 {
        return None;
    }
    // WOW64 でなければ machine は UNKNOWN で、OS と同じアーキテクチャ。
    let machine = if machine == IMAGE_FILE_MACHINE_UNKNOWN {
        native
    } else {
        machine
    };
    Arch::from_pe_machine(machine)
}

fn image_path(process: HANDLE) -> Option<String> {
    let mut buffer = vec![0u16; 1024];
    let mut length = buffer.len() as u32;
    // SAFETY: 出力先は `buffer` で、その長さを渡す。
    let ok = unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut length) };
    if ok == 0 {
        return None;
    }
    buffer.truncate(length as usize);
    Some(strip_verbatim(buffer))
}

/// 動いているプロセスの一覧。自分自身は含めない。
pub fn list_processes() -> Vec<ProcessInfo> {
    // SAFETY: 引数は値だけ。失敗すれば INVALID_HANDLE_VALUE。
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Vec::new();
    }
    let mut list = Vec::new();
    // SAFETY: PROCESSENTRY32W は整数と配列だけの構造体で、すべて 0 は有効な値。
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let me = std::process::id();
    // SAFETY: スナップショットのハンドルと、大きさを入れた構造体を渡す。
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while more {
        let pid = entry.th32ProcessID;
        // PID 0 は System Idle Process で、アタッチできない。
        if pid != 0 && pid != me {
            let length = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..length]);
            // SAFETY: 引数は値だけ。失敗すれば NULL。
            let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
            let (path, arch) = if handle.is_null() {
                (None, None)
            } else {
                let info = (image_path(handle), arch_of(handle));
                // SAFETY: 上で開いたハンドルを 1 回だけ閉じる。
                unsafe { CloseHandle(handle) };
                info
            };
            list.push(ProcessInfo {
                pid,
                parent_pid: entry.th32ParentProcessID,
                name,
                path,
                arch,
            });
        }
        // SAFETY: 同上。
        more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    // SAFETY: 上で作ったスナップショットを 1 回だけ閉じる。
    unsafe { CloseHandle(snapshot) };
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_a_child_process_with_its_arch() {
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/c", "ping -n 5 127.0.0.1 >nul"])
            .spawn()
            .unwrap();
        let list = list_processes();
        let found = list.iter().find(|p| p.pid == child.id()).cloned();
        let _ = child.kill();
        let _ = child.wait();
        let found = found.expect("起動したプロセスが一覧にない");
        assert!(found.name.eq_ignore_ascii_case("cmd.exe"));
        assert_eq!(found.parent_pid, std::process::id());
        assert!(found.arch.is_some());
        assert!(found.path.unwrap().to_ascii_lowercase().ends_with("cmd.exe"));
        assert!(
            !list.iter().any(|p| p.pid == std::process::id()),
            "自分自身が入っている"
        );
    }
}
