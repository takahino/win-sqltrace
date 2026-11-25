//! 地方時と UTC の差。出力の時刻の表示に使う（export は OS に依存しないので、ここで求めて渡す）。

use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows_sys::Win32::System::SystemInformation::GetSystemTime;
use windows_sys::Win32::System::Time::{SystemTimeToFileTime, SystemTimeToTzSpecificLocalTime};

/// 今の地方時が UTC より何分進んでいるか（日本は 540）。求められなければ 0（UTC で表示する）。
pub fn utc_offset_minutes() -> i32 {
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    let (mut utc_file, mut local_file) = (FILETIME::default(), FILETIME::default());
    // SAFETY: 出力先はどれも有効な構造体。時間帯は NULL（今の設定）を使う。
    let ok = unsafe {
        GetSystemTime(&mut utc);
        SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) != 0
            && SystemTimeToFileTime(&utc, &mut utc_file) != 0
            && SystemTimeToFileTime(&local, &mut local_file) != 0
    };
    if !ok {
        return 0;
    }
    let ticks = |f: FILETIME| (i64::from(f.dwHighDateTime) << 32) | i64::from(f.dwLowDateTime);
    // FILETIME は 100ns 単位。秒の端数で 1 分ずれないよう丸める。
    let difference = ticks(local_file) - ticks(utc_file);
    ((difference as f64) / 600_000_000.0).round() as i32
}
