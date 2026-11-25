//! ファイルを選ぶ Windows の標準のダイアログ（開く / 名前を付けて保存）。GUI のスレッドから呼ぶ。

use std::path::{Path, PathBuf};

use windows_sys::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, GetSaveFileNameW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST,
    OPENFILENAMEW,
};

/// ファイルの種類の絞り込み。`("Excel", "*.xlsx")` のように並べる。
pub type Filter<'a> = &'a [(&'a str, &'a str)];

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `名前\0パターン\0…\0\0` の形にする。
fn filter_block(filter: Filter<'_>) -> Vec<u16> {
    let mut block = Vec::new();
    for (name, pattern) in filter {
        block.extend(name.encode_utf16());
        block.push(0);
        block.extend(pattern.encode_utf16());
        block.push(0);
    }
    block.push(0);
    block
}

fn run(title: &str, initial: Option<&Path>, filter: Filter<'_>, save: Option<&str>) -> Option<PathBuf> {
    let title = wide(title);
    let filter = filter_block(filter);
    let mut file = vec![0u16; 32_768];
    if let Some(initial) = initial {
        let initial: Vec<u16> = initial.as_os_str().to_string_lossy().encode_utf16().collect();
        let length = initial.len().min(file.len() - 1);
        file[..length].copy_from_slice(&initial[..length]);
    }
    let extension = save.map(wide);
    // SAFETY: OPENFILENAMEW は整数とポインターだけの C の構造体で、すべて 0 は有効な値。
    let mut dialog: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    dialog.lStructSize = size_of::<OPENFILENAMEW>() as u32;
    dialog.lpstrFilter = filter.as_ptr();
    dialog.nFilterIndex = 1;
    dialog.lpstrFile = file.as_mut_ptr();
    dialog.nMaxFile = file.len() as u32;
    dialog.lpstrTitle = title.as_ptr();
    // ダイアログで選んだフォルダーに、このプロセスの作業フォルダーを変えさせない。
    dialog.Flags = OFN_NOCHANGEDIR | OFN_PATHMUSTEXIST;
    let ok = match &extension {
        Some(extension) => {
            dialog.lpstrDefExt = extension.as_ptr();
            dialog.Flags |= OFN_OVERWRITEPROMPT;
            // SAFETY: 渡すバッファーはどれも呼び出しの間生きていて、長さを入れてある。
            unsafe { GetSaveFileNameW(&mut dialog) }
        }
        None => {
            dialog.Flags |= OFN_FILEMUSTEXIST;
            // SAFETY: 同上。
            unsafe { GetOpenFileNameW(&mut dialog) }
        }
    };
    if ok == 0 {
        // 取り消し（または失敗）。どちらも「選ばなかった」として扱う。
        return None;
    }
    let length = file.iter().position(|&c| c == 0).unwrap_or(file.len());
    Some(PathBuf::from(String::from_utf16_lossy(&file[..length])))
}

/// 開くファイルを選ぶ。取り消したら `None`。
pub fn open_file(title: &str, initial: Option<&Path>, filter: Filter<'_>) -> Option<PathBuf> {
    run(title, initial, filter, None)
}

/// 保存先を選ぶ。`extension` は拡張子を書かなかったときに付けるもの（`xlsx` など）。上書きなら確かめる。
pub fn save_file(title: &str, initial: Option<&Path>, filter: Filter<'_>, extension: &str) -> Option<PathBuf> {
    run(title, initial, filter, Some(extension))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_block_is_double_nul_terminated() {
        let block = filter_block(&[("Excel", "*.xlsx")]);
        assert_eq!(String::from_utf16_lossy(&block), "Excel\0*.xlsx\0\0");
    }
}
