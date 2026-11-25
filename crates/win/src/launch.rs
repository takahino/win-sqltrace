//! 起動モード: コマンドラインと環境ブロックを組み立て、`DEBUG_ONLY_THIS_PROCESS` で起動する。

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use sqltrace_core::LaunchSpec;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, CREATE_NEW_CONSOLE, CREATE_UNICODE_ENVIRONMENT, DEBUG_ONLY_THIS_PROCESS, PROCESS_INFORMATION,
    STARTUPINFOW,
};

use crate::error::{last_error, DebugError};

/// デバッガの下ではデバッグ用のヒープ（遅く、メモリの使い方も変わる）が既定で使われるので切る。
const NO_DEBUG_HEAP: (&str, &str) = ("_NO_DEBUG_HEAP", "1");

/// 1 つの引数を、MSVC の C ランタイムの規則（`CommandLineToArgvW` と同じ）で読めるように引用する。
pub fn quote_argument(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']) {
        return arg.to_owned();
    }
    let mut quoted = String::from("\"");
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // 引用符の前の \ は 2 倍にし、引用符自体も \ で逃がす。
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(c);
                backslashes = 0;
            }
        }
    }
    // 閉じる引用符の前の \ も 2 倍にする。
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

/// 起動するプログラムのコマンドライン。`arguments` は利用者が書いたまま（引用も含めて）渡す。
pub(crate) fn command_line(exe: &Path, arguments: &str) -> String {
    let exe = quote_argument(&exe.to_string_lossy());
    if arguments.is_empty() {
        exe
    } else {
        format!("{exe} {arguments}")
    }
}

/// 環境ブロック（`名前=値\0...\0\0` の UTF-16）。今の環境を引き継ぎ、`extra` で上書きする。
/// 名前の大文字小文字は区別せず、OS の求めるとおり名前の順に並べる。
pub(crate) fn environment_block(
    base: impl IntoIterator<Item = (OsString, OsString)>,
    extra: &[(String, String)],
) -> Vec<u16> {
    let mut entries: Vec<(OsString, OsString)> = base.into_iter().collect();
    let mut set = |name: &str, value: &str| {
        let upper = name.to_uppercase();
        entries.retain(|(n, _)| n.to_string_lossy().to_uppercase() != upper);
        entries.push((name.into(), value.into()));
    };
    set(NO_DEBUG_HEAP.0, NO_DEBUG_HEAP.1);
    for (name, value) in extra {
        set(name, value);
    }
    entries.sort_by_cached_key(|(n, _)| n.to_string_lossy().to_uppercase());
    let mut block = Vec::new();
    for (name, value) in &entries {
        block.extend(name.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    if entries.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

/// 起動する exe のフルパス。フォルダーを含まない名前なら PATH から探す（`CreateProcessW` に渡す
/// アプリケーション名は PATH を探さないため）。
pub(crate) fn resolve_exe(exe: &Path) -> Result<PathBuf, DebugError> {
    let not_found = || DebugError::ExeNotFound { exe: exe.to_owned() };
    if exe.is_file() {
        return std::path::absolute(exe).map_err(|_| not_found());
    }
    let has_directory = exe.parent().is_some_and(|p| !p.as_os_str().is_empty());
    if has_directory {
        return Err(not_found());
    }
    let names: Vec<PathBuf> = if exe.extension().is_some() {
        vec![exe.to_owned()]
    } else {
        vec![exe.to_owned(), exe.with_extension("exe")]
    };
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file())
        .ok_or_else(not_found)
}

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// デバッグ付きで起動して PID を返す。呼んだスレッドがデバッガになる。
pub(crate) fn launch(spec: &LaunchSpec, exe: &Path) -> Result<u32, DebugError> {
    let working_dir = spec.working_dir.clone().or_else(|| exe.parent().map(Path::to_path_buf));
    let application = wide(exe.as_os_str());
    let mut command: Vec<u16> = wide(OsString::from(command_line(exe, &spec.arguments)).as_os_str());
    let environment = environment_block(std::env::vars_os(), &spec.environment);
    let working_dir = working_dir.map(|d| wide(d.as_os_str()));

    let mut flags = DEBUG_ONLY_THIS_PROCESS | CREATE_UNICODE_ENVIRONMENT;
    if spec.new_console {
        flags |= CREATE_NEW_CONSOLE;
    }
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: 文字列はすべて NUL 終端で、呼び出しの間生きている。コマンドラインは書き換えられることがある
    // ので、可変のバッファーを渡す。
    let ok = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            flags,
            environment.as_ptr().cast(),
            working_dir.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
            &startup,
            &mut info,
        )
    };
    if ok == 0 {
        return Err(DebugError::Launch {
            exe: exe.to_owned(),
            code: last_error(),
        });
    }
    // ここで受け取るハンドルは使わない（デバッグイベントで別のハンドルが届く）。
    // SAFETY: CreateProcessW が返した、こちらが持つハンドルを 1 回ずつ閉じる。
    unsafe {
        CloseHandle(info.hThread);
        CloseHandle(info.hProcess);
    }
    Ok(info.dwProcessId)
}

/// `GetFinalPathNameByHandleW` などが返す `\\?\` 付きのパスを、ふつうの形にする。
pub(crate) fn strip_verbatim(path: Vec<u16>) -> String {
    let path = String::from(OsString::from_wide(&path).to_string_lossy());
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_follows_msvc_rules() {
        assert_eq!(quote_argument("plain"), "plain");
        assert_eq!(quote_argument(""), "\"\"");
        assert_eq!(quote_argument("a b"), "\"a b\"");
        assert_eq!(quote_argument(r#"say "hi""#), r#""say \"hi\"""#);
        // 末尾の \ は閉じる引用符を逃がさないよう 2 倍にする。
        assert_eq!(quote_argument(r"C:\My Dir\"), r#""C:\My Dir\\""#);
        // 引用符のない \ はそのまま。
        assert_eq!(quote_argument(r"C:\dir\x"), r"C:\dir\x");
    }

    #[test]
    fn command_line_quotes_exe_and_keeps_arguments() {
        assert_eq!(
            command_line(Path::new(r"C:\Program Files\app.exe"), r#"-x "a b""#),
            r#""C:\Program Files\app.exe" -x "a b""#
        );
        assert_eq!(command_line(Path::new(r"C:\app.exe"), ""), r"C:\app.exe");
    }

    fn decode(block: &[u16]) -> Vec<String> {
        String::from_utf16_lossy(block)
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn environment_adds_no_debug_heap_and_overrides_case_insensitively() {
        let base = [("Path".into(), r"C:\bin".into()), ("zeta".into(), "1".into())];
        let block = environment_block(base, &[("PATH".into(), r"D:\x".into()), ("Alpha".into(), "a".into())]);
        assert_eq!(block[block.len() - 2..], [0, 0]);
        assert_eq!(
            decode(&block),
            // '_' (0x5F) は英大文字より後ろに並ぶ。
            ["Alpha=a", r"PATH=D:\x", "zeta=1", "_NO_DEBUG_HEAP=1"]
        );
    }

    #[test]
    fn user_can_override_no_debug_heap() {
        let block = environment_block(Vec::new(), &[("_no_debug_heap".into(), "0".into())]);
        assert_eq!(decode(&block), ["_no_debug_heap=0"]);
    }

    #[test]
    fn verbatim_prefix_is_removed() {
        let w = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        assert_eq!(
            strip_verbatim(w(r"\\?\C:\Windows\odbc32.dll")),
            r"C:\Windows\odbc32.dll"
        );
        assert_eq!(strip_verbatim(w(r"\\?\UNC\srv\share\a.dll")), r"\\srv\share\a.dll");
    }
}
