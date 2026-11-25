//! 試験の補助。
#![cfg(windows)]

pub mod harness;

use std::sync::Once;

use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// 試験のプロセスを「閉じたら中のプロセスをすべて終わらせる」ジョブに入れる。試験が失敗や中断で終わっても、
/// 起動した対象のアプリを残さないため（子プロセスはジョブを引き継ぐ）。
pub fn kill_children_on_exit() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: 名前なしのジョブを作るだけ。ハンドルはプロセスの終わりまで閉じない（閉じると中が終わる）。
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        assert!(!job.is_null(), "ジョブを作れません");
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: 渡す構造体の大きさは種類に合っている。
        let ok = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        assert_ne!(ok, 0, "ジョブを設定できません");
        // SAFETY: 自分のプロセスの擬似ハンドルを渡すだけ。
        let ok = unsafe { AssignProcessToJobObject(job, GetCurrentProcess()) };
        assert_ne!(ok, 0, "ジョブに入れません");
    });
}

/// 試験のアプリとスタブの DLL（oci.dll、sqora32.dll）を `target` 向けにビルドし、置き場所のフォルダーを返す。
/// 試験のプロセスにつき、ターゲットごとに 1 回だけビルドする。
///
/// 外側の `cargo test` とは出力先を分ける（同じ出力先だとロックで待ち合う）。スタブの DLL は試験の crate の
/// 依存にできない（cdylib は試験から参照できない）ので、ここでビルドする。
pub fn artifacts(target: &str) -> std::path::PathBuf {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    static BUILT: Mutex<Option<HashMap<String, PathBuf>>> = Mutex::new(None);
    let mut built = BUILT.lock().unwrap_or_else(|e| e.into_inner());
    let built = built.get_or_insert_with(HashMap::new);
    if let Some(dir) = built.get(target) {
        return dir.clone();
    }
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let target_dir = workspace.join("target").join("itest");
    let status = std::process::Command::new(env!("CARGO"))
        .current_dir(&workspace)
        .args([
            "build",
            "-q",
            "-p",
            "sqltrace-itest",
            "-p",
            "sqltrace-stub-oci",
            "-p",
            "sqltrace-stub-sqora",
            "--lib",
            "--bins",
            "--target",
            target,
        ])
        .env("CARGO_TARGET_DIR", &target_dir)
        .status()
        .expect("cargo を起動できません");
    assert!(
        status.success(),
        "{target} 向けの試験のアプリとスタブをビルドできません"
    );
    let dir = target_dir.join(target).join("debug");
    built.insert(target.to_owned(), dir.clone());
    dir
}

/// この試験と同じターゲット。
pub const NATIVE_TARGET: &str = if cfg!(target_arch = "x86") {
    "i686-pc-windows-msvc"
} else {
    "x86_64-pc-windows-msvc"
};

/// x64 版の sqltrace から WOW64 のプロセスを試すときのターゲット。
#[cfg(target_arch = "x86_64")]
pub const WOW64_TARGET: &str = "i686-pc-windows-msvc";

/// 試験のアプリが `--multiline` で送る SQL。CRLF と LF が混ざり、文字列リテラルの中にも改行があり、
/// 最後も改行で終わる（Windows のアプリがそのまま送ってくる形）。ANSI 版でも同じに読めるよう ASCII だけにする
/// （ANSI 版は対象の機械のコードページで読むので、それ以外の文字は機械によって変わる）。
pub const MULTILINE_SQL: &str = "SELECT a,\r\n       b -- note\r\nFROM t\nWHERE x = 'one\r\ntwo'\r\n";
