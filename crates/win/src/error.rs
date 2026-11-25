//! デバッグ層の失敗。表示には「何が起きたか」と「どうすればよいか」を入れる。

use std::fmt;
use std::path::PathBuf;

use sqltrace_core::Arch;

const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_PATH_NOT_FOUND: u32 = 3;
const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_NOT_SUPPORTED: u32 = 50;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_BAD_EXE_FORMAT: u32 = 193;
const ERROR_ELEVATION_REQUIRED: u32 = 740;

#[derive(Debug)]
pub enum DebugError {
    Attach {
        pid: u32,
        code: u32,
    },
    Launch {
        exe: PathBuf,
        code: u32,
    },
    ExeNotFound {
        exe: PathBuf,
    },
    /// この版の sqltrace では扱えないアーキテクチャ。`None` は ARM64 など sqlTrace が知らないもの。
    UnsupportedArch {
        pid: Option<u32>,
        arch: Option<Arch>,
    },
    /// ほかの sqltrace がその PID をトレースしている。`tracer` はその sqltrace の PID（分かれば）。
    AlreadyTraced {
        pid: u32,
        tracer: Option<u32>,
    },
    /// sqltrace ではないデバッガ（Visual Studio など）が付いている。
    OtherDebugger {
        pid: u32,
    },
    /// デバッグの最中の OS の呼び出しの失敗（続けられない）。
    Os {
        what: &'static str,
        code: u32,
    },
}

impl DebugError {
    pub(crate) fn last_os(what: &'static str) -> Self {
        DebugError::Os {
            what,
            code: last_error(),
        }
    }
}

pub(crate) fn last_error() -> u32 {
    // SAFETY: 引数を取らず、呼び出したスレッドの最後のエラーを返すだけ。
    unsafe { windows_sys::Win32::Foundation::GetLastError() }
}

/// OS のエラーの説明（日本語の環境では日本語になる）。
fn os_message(code: u32) -> String {
    let message = std::io::Error::from_raw_os_error(code as i32).to_string();
    message.trim_end().to_owned()
}

impl fmt::Display for DebugError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DebugError::Attach { pid, code } => {
                write!(f, "PID {pid} にアタッチできませんでした（{}）。", os_message(*code))?;
                let hint = match *code {
                    ERROR_ACCESS_DENIED => {
                        "対象が別のユーザーや管理者で動いているなら、sqltrace を管理者として実行してください。\
                         別のデバッガ（Visual Studio など）がアタッチしていないかも確かめてください"
                    }
                    ERROR_INVALID_PARAMETER => {
                        "そのプロセスはもう終わっているか、ほかのデバッガが付いているかもしれません。PID を確かめてください"
                    }
                    ERROR_NOT_SUPPORTED => "64 ビットのプロセスには x64 版の sqltrace を使ってください",
                    _ => "PID と権限を確かめてください",
                };
                f.write_str(hint)
            }
            DebugError::Launch { exe, code } => {
                write!(f, "{} を起動できませんでした（{}）。", exe.display(), os_message(*code))?;
                let hint = match *code {
                    ERROR_ELEVATION_REQUIRED => {
                        "このアプリは管理者権限を要求します。sqltrace を管理者として実行してください"
                    }
                    ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => "パスと作業フォルダーを確かめてください",
                    ERROR_BAD_EXE_FORMAT => "実行ファイルではないか、この Windows で動かない形式です",
                    _ => "パスと引数を確かめてください",
                };
                f.write_str(hint)
            }
            DebugError::ExeNotFound { exe } => write!(
                f,
                "{} が見つかりません。フルパスで指定するか、PATH の通ったフォルダーに置いてください",
                exe.display()
            ),
            DebugError::UnsupportedArch { pid, arch } => {
                if let Some(pid) = pid {
                    write!(f, "PID {pid} は")?;
                } else {
                    f.write_str("このプログラムは")?;
                }
                match arch {
                    Some(arch) => write!(
                        f,
                        " {arch} です。この sqltrace（{}）ではトレースできません。\
                         x64 版の sqltrace を使ってください（x64 版は x64 と x86 の両方を扱えます）",
                        crate::host_arch()
                    ),
                    None => f.write_str("sqlTrace が対応していないアーキテクチャ（ARM64 など）です"),
                }
            }
            DebugError::AlreadyTraced { pid, tracer } => write!(
                f,
                "PID {pid} は{}がトレースしています。1 つのプロセスに付けられる sqltrace は 1 つだけです。\
                 そちらを止めてからアタッチしてください",
                crate::journal::tracer_name(*tracer)
            ),
            DebugError::OtherDebugger { pid } => write!(
                f,
                "PID {pid} には別のデバッガ（Visual Studio など）が付いています。1 つのプロセスに付けられる\
                 デバッガは 1 つだけです。そちらを外してからアタッチしてください"
            ),
            DebugError::Os { what, code } => write!(
                f,
                "{what}に失敗しました（{}）。対象のプロセスが終わったか、権限が足りない可能性があります",
                os_message(*code)
            ),
        }
    }
}

impl std::error::Error for DebugError {}
