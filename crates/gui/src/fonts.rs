//! 日本語のフォント。egui の既定のフォントには日本語がないので、Windows に入っているものを読み込む。

use std::sync::Arc;

use eframe::egui::{Context, FontData, FontDefinitions, FontFamily, FontTweak};

/// 探す順（Windows 10 以降は游ゴシック、古い環境はメイリオか MS ゴシック）。
const CANDIDATES: &[&str] = &["YuGothM.ttc", "meiryo.ttc", "msgothic.ttc"];

/// 日本語のフォントは、egui の既定のフォントより文字の基準の位置が高く、そのままでは上に寄って上の端が欠ける。
/// 描く位置だけを下へずらしてそろえる（行の高さは変えない）。値は画面で見て合わせた。
fn tweak(name: &str) -> FontTweak {
    let y_offset_factor = match name {
        "YuGothM.ttc" => 0.22,
        _ => 0.15,
    };
    FontTweak {
        y_offset_factor,
        ..FontTweak::default()
    }
}

/// 見つかったフォントの名前を返す。どれもなければ `None`（日本語は豆腐になるが、動作は続ける）。
pub fn install_japanese(ctx: &Context) -> Option<&'static str> {
    let fonts_dir = std::env::var_os("WINDIR").map_or_else(
        || std::path::PathBuf::from(r"C:\Windows\Fonts"),
        |w| std::path::PathBuf::from(w).join("Fonts"),
    );
    let (name, bytes) = CANDIDATES
        .iter()
        .find_map(|name| std::fs::read(fonts_dir.join(name)).ok().map(|bytes| (*name, bytes)))?;
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "japanese".to_owned(),
        Arc::new(FontData::from_owned(bytes).tweak(tweak(name))),
    );
    // 英数字は既定のフォントのまま、足りない文字（日本語）だけをこちらで描く。
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push("japanese".to_owned());
    }
    ctx.set_fonts(fonts);
    Some(name)
}
