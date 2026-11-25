//! exe のビルド（build.rs）で使う補助。
//!
//! アイコン（`.ico`）を Windows のリソース（`.res`）にして、MSVC のリンカーに渡す。`.res` はここで書くので、
//! rc.exe もリソースのクレートも要らない（リンカーが `.res` を受け取って exe に埋め込む）。

use std::path::Path;

/// `ico` を exe のアイコンにする（MSVC のときだけ。ほかのターゲットでは何もしない）。build.rs から呼ぶ。
///
/// # Panics
/// `.ico` を読めないとき、または `OUT_DIR` に書けないとき（ビルドを止めて知らせる）。
#[allow(clippy::print_stdout)] // cargo への指示（cargo:…）は標準出力に書く決まり。
pub fn embed_icon(ico: &Path) {
    println!("cargo:rerun-if-changed={}", ico.display());
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    let bytes = std::fs::read(ico).unwrap_or_else(|e| panic!("{} を読めません: {e}", ico.display()));
    let out = std::env::var("OUT_DIR").unwrap_or_else(|e| panic!("OUT_DIR がありません: {e}"));
    let res = Path::new(&out).join("icon.res");
    std::fs::write(&res, icon_resources(&bytes)).unwrap_or_else(|e| panic!("{} に書けません: {e}", res.display()));
    println!("cargo:rustc-link-arg-bins={}", res.display());
}

/// `.ico` の中身を `.res` にする（RT_ICON を絵ごとに 1 つと、それをまとめる RT_GROUP_ICON の 1 番）。
pub fn icon_resources(ico: &[u8]) -> Vec<u8> {
    const RT_ICON: u16 = 3;
    const RT_GROUP_ICON: u16 = 14;
    let word = |at: usize| u16::from_le_bytes([ico[at], ico[at + 1]]);
    let dword = |at: usize| u32::from_le_bytes([ico[at], ico[at + 1], ico[at + 2], ico[at + 3]]);
    let count = usize::from(word(4));
    let mut res = Vec::new();
    // .res の始まりの、中身の無い 1 つ（32 バイトの頭だけ）。
    resource(&mut res, 0, 0, 0, &[]);
    let mut group = vec![0, 0, 1, 0];
    group.extend_from_slice(&(count as u16).to_le_bytes());
    for index in 0..count {
        let entry = 6 + index * 16;
        let (size, offset) = (dword(entry + 8) as usize, dword(entry + 12) as usize);
        let id = index as u16 + 1;
        // 0x1010: MOVEABLE | DISCARDABLE（rc.exe が付けるのと同じ）。
        resource(&mut res, RT_ICON, id, 0x1010, &ico[offset..offset + size]);
        // GRPICONDIRENTRY: .ico の ICONDIRENTRY の始めの 12 バイトと、リソースの番号。
        group.extend_from_slice(&ico[entry..entry + 12]);
        group.extend_from_slice(&id.to_le_bytes());
    }
    // 0x1030: MOVEABLE | PURE | DISCARDABLE。
    resource(&mut res, RT_GROUP_ICON, 1, 0x1030, &group);
    res
}

/// `.res` に 1 つ書く（頭は 32 バイト。種類と名前は番号。中身は 4 バイト境界まで埋める）。
fn resource(res: &mut Vec<u8>, kind: u16, id: u16, flags: u16, data: &[u8]) {
    res.extend_from_slice(&(data.len() as u32).to_le_bytes());
    res.extend_from_slice(&32u32.to_le_bytes());
    for value in [0xFFFF, kind, 0xFFFF, id] {
        res.extend_from_slice(&value.to_le_bytes());
    }
    res.extend_from_slice(&0u32.to_le_bytes());
    res.extend_from_slice(&flags.to_le_bytes());
    res.extend_from_slice(&0u16.to_le_bytes());
    res.extend_from_slice(&[0; 8]);
    res.extend_from_slice(data);
    res.resize(res.len().div_ceil(4) * 4, 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1 枚の絵（4 バイト）だけの .ico。
    fn tiny_ico() -> Vec<u8> {
        let mut ico = vec![0, 0, 1, 0, 1, 0];
        ico.extend_from_slice(&[16, 16, 0, 0, 1, 0, 32, 0]);
        ico.extend_from_slice(&4u32.to_le_bytes());
        ico.extend_from_slice(&22u32.to_le_bytes());
        ico.extend_from_slice(&[1, 2, 3, 4]);
        ico
    }

    #[test]
    fn writes_empty_icon_and_group_entries_aligned() {
        let res = icon_resources(&tiny_ico());
        // 空の頭（32）+ RT_ICON（32 + 4）+ RT_GROUP_ICON（32 + 20）。
        assert_eq!(res.len(), 32 + 36 + 52);
        assert_eq!(res.len() % 4, 0);
        // RT_ICON の中身が、そのまま入る。
        assert_eq!(&res[32 + 32..32 + 36], &[1, 2, 3, 4]);
        // RT_GROUP_ICON の種類の番号。
        let group = 32 + 36;
        assert_eq!(u16::from_le_bytes([res[group + 10], res[group + 11]]), 14);
    }
}
