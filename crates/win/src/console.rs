//! コンソールの Ctrl+C と、ウィンドウを閉じる操作で、対象を壊さずにデタッチする。
//!
//! ハンドラは別のスレッドで呼ばれ、デバッグ API はアタッチしたスレッドからしか呼べない。だからハンドラは
//! 旗を立てるだけにして、後始末はデバッグのスレッドに任せる。ウィンドウを閉じる・ログオフ・シャットダウンでは
//! ハンドラから戻るとプロセスが終わるので、後始末が終わるまで（上限つきで）待つ。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use windows_sys::core::BOOL;
use windows_sys::Win32::System::Console::{SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_C_EVENT};

use crate::inspector::StopSignal;

/// OS は閉じる操作のハンドラを 5 秒ほどで打ち切るので、その少し前まで待つ。
const CLOSE_WAIT: Duration = Duration::from_millis(4500);

struct Registration {
    stop: StopSignal,
    finished: Arc<AtomicBool>,
}

static REGISTRATION: OnceLock<Registration> = OnceLock::new();

/// Ctrl+C などで `stop` にデタッチを求める。`finished` は後始末が終わったら呼び出し側が立てる。
/// プロセスにつき 1 回だけ登録できる（2 回目は `false`）。
pub fn install_console_handler(stop: StopSignal, finished: Arc<AtomicBool>) -> bool {
    if REGISTRATION.set(Registration { stop, finished }).is_err() {
        return false;
    }
    // SAFETY: ハンドラはプロセスの終わりまで有効な関数。
    unsafe { SetConsoleCtrlHandler(Some(handler), 1) != 0 }
}

unsafe extern "system" fn handler(kind: u32) -> BOOL {
    let Some(registration) = REGISTRATION.get() else {
        return 0;
    };
    registration.stop.detach();
    if kind != CTRL_C_EVENT && kind != CTRL_BREAK_EVENT {
        let deadline = Instant::now() + CLOSE_WAIT;
        while !registration.finished.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    1
}

/// 標準出力のコンソールで ANSI の色（エスケープシーケンス）を使えるようにする。コンソールでない
/// （ファイルやパイプにリダイレクトした）ときと、古いコンソールで使えないときは `false`。
pub fn enable_console_colors() -> bool {
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, CONSOLE_MODE, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
        STD_OUTPUT_HANDLE,
    };
    // SAFETY: 引数は値だけ。
    let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    let mut mode: CONSOLE_MODE = 0;
    // SAFETY: 出力先は有効な CONSOLE_MODE。コンソールでなければ失敗する。
    if handle.is_null() || unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
        return false;
    }
    // SAFETY: 今の設定に 1 つ足すだけ。
    mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0
        || unsafe { SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) } != 0
}
