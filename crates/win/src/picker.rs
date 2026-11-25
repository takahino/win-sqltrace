//! 照準（Spy++ と同じ操作）: マウスのボタンを押したままほかのウィンドウの上へ動かし、離したところの
//! ウィンドウのプロセスを選ぶ。
//!
//! GUI のウィンドウの外ではマウスのイベントが届かないので、押している間は毎回カーソルの位置とボタンの状態を
//! OS に聞く（[`window_under_cursor`]、[`left_button_down`]）。

use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetClassNameW, GetCursorPos, GetWindowTextW, GetWindowThreadProcessId, WindowFromPoint, GA_ROOT,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickedWindow {
    pub pid: u32,
    /// 一番外側のウィンドウのタイトルとクラス名（どのアプリかを見分ける手がかり）。
    pub title: String,
    pub class: String,
}

fn text(hwnd: HWND, read: unsafe extern "system" fn(HWND, *mut u16, i32) -> i32) -> String {
    let mut buffer = [0u16; 256];
    // SAFETY: 出力先は `buffer` で、その長さを渡す。
    let length = unsafe { read(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..length.max(0) as usize])
}

/// 今のカーソルの下にある一番外側のウィンドウ。
pub fn window_under_cursor() -> Option<PickedWindow> {
    let mut point = POINT::default();
    // SAFETY: 出力先は有効な POINT。
    if unsafe { GetCursorPos(&mut point) } == 0 {
        return None;
    }
    // SAFETY: 引数は値だけ。
    let hwnd = unsafe { WindowFromPoint(point) };
    if hwnd.is_null() {
        return None;
    }
    // ボタンや入力欄など子のウィンドウを指していても、アプリの本体のウィンドウにする。
    // SAFETY: 引数は値だけ。
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    let root = if root.is_null() { hwnd } else { root };
    let mut pid = 0u32;
    // SAFETY: 出力先は有効な u32。
    unsafe { GetWindowThreadProcessId(root, &mut pid) };
    if pid == 0 {
        return None;
    }
    Some(PickedWindow {
        pid,
        title: text(root, GetWindowTextW),
        class: text(root, GetClassNameW),
    })
}

/// マウスの左ボタンが今押されているか（ほかのアプリのウィンドウの上でも分かる）。
pub fn left_button_down() -> bool {
    // SAFETY: 引数は値だけ。最上位のビットが「押されている」。
    unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON)) as u16 & 0x8000 != 0 }
}
