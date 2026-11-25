//! Excel（xlsx）: SQL の表のシートと、状態と警告のシート。
//!
//! Excel の制限に合わせる: 1 つのセルは 32,767 文字まで（超えた分は切り詰めて印を付ける）、タブと改行以外の
//! 制御文字は入れられない（取り除く）、1 枚のシートは 1,048,576 行まで（超えた分は書かず、ログのシートに書く）。

use rust_xlsxwriter::{Format, FormatAlign, Workbook, Worksheet, XlsxError};

use crate::entry::Entry;
use crate::highlight::{pieces, TokenKind};
use crate::time::TimeZone;

const MAX_CELL_CHARS: usize = 32_767;
const MAX_ROWS: u32 = 1_048_576;
const CUT_MARK: &str = "…（Excel のセルの上限で切り詰め）";

/// Excel のセルに入れられる文字列にする。
pub fn cell_text(text: &str) -> String {
    let cleaned: String = text
        .replace("\r\n", "\n")
        .chars()
        .filter(|&c| c == '\t' || c == '\n' || !c.is_control())
        .collect();
    if cleaned.chars().count() <= MAX_CELL_CHARS {
        return cleaned;
    }
    let keep = MAX_CELL_CHARS - CUT_MARK.chars().count();
    let mut cut: String = cleaned.chars().take(keep).collect();
    cut.push_str(CUT_MARK);
    cut
}

/// 色分けの書式（種類ごとに 1 つ。色は [`TokenKind::light`] で決める）。
struct Palette {
    plain: Format,
    kinds: Vec<(TokenKind, Format)>,
}

impl Palette {
    fn new() -> Self {
        let kinds = [
            TokenKind::Keyword,
            TokenKind::String,
            TokenKind::Number,
            TokenKind::Comment,
            TokenKind::Bind,
        ]
        .into_iter()
        .filter_map(|kind| {
            let color = kind.light()?;
            let mut format = Format::new().set_font_color(color.hex().as_str());
            if kind.bold() {
                format = format.set_bold();
            }
            if kind.italic() {
                format = format.set_italic();
            }
            Some((kind, format))
        })
        .collect();
        Self {
            plain: Format::new(),
            kinds,
        }
    }

    fn format(&self, kind: TokenKind) -> &Format {
        self.kinds
            .iter()
            .find(|(k, _)| *k == kind)
            .map_or(&self.plain, |(_, f)| f)
    }
}

/// SQL を色分けして 1 つのセルに書く（Excel のリッチテキスト）。色の付く断片がなければふつうの文字列で書く。
fn write_highlighted(
    sheet: &mut Worksheet,
    row: u32,
    col: u16,
    sql: &str,
    palette: &Palette,
    cell: &Format,
) -> Result<(), XlsxError> {
    // リッチテキストは空の断片を受け付けない。
    let runs: Vec<(&Format, &str)> = pieces(sql)
        .filter(|(_, text)| !text.is_empty())
        .map(|(kind, text)| (palette.format(kind), text))
        .collect();
    if runs.len() < 2 || runs.iter().all(|(f, _)| std::ptr::eq(*f, &palette.plain)) {
        sheet.write_string_with_format(row, col, sql, cell)?;
    } else {
        sheet.write_rich_string_with_format(row, col, &runs, cell)?;
    }
    Ok(())
}

pub struct XlsxReport<'a> {
    pub title: &'a str,
    pub zone: TimeZone,
}

impl XlsxReport<'_> {
    /// xlsx のバイト列を作る（ファイルへの書き込みは呼び出し側。書き出し先の誤りを表示に含めるため）。
    pub fn to_bytes(&self, entries: &[Entry]) -> Result<Vec<u8>, XlsxError> {
        let mut workbook = Workbook::new();
        let header = Format::new().set_bold().set_background_color("#F6F8FA");
        let wrap = Format::new().set_text_wrap().set_align(FormatAlign::Top);
        let top = Format::new().set_align(FormatAlign::Top);
        let palette = Palette::new();

        let mut skipped = 0usize;
        {
            let sheet = workbook.add_worksheet();
            sheet.set_name("SQL")?;
            let columns: [(&str, f64); 11] = [
                ("#", 6.0),
                ("時刻", 24.0),
                ("PID", 8.0),
                ("TID", 8.0),
                ("層", 12.0),
                ("関数", 18.0),
                ("種別", 6.0),
                ("SQL", 80.0),
                ("切り詰め", 8.0),
                ("呼び出し元", 28.0),
                ("ハンドル", 16.0),
            ];
            for (col, (name, width)) in columns.iter().enumerate() {
                let col = col as u16;
                sheet.write_string_with_format(0, col, *name, &header)?;
                sheet.set_column_width(col, *width)?;
            }
            let mut row = 0u32;
            for entry in entries {
                let Entry::Sql(s) = entry else {
                    continue;
                };
                if row + 1 >= MAX_ROWS {
                    skipped += 1;
                    continue;
                }
                row += 1;
                sheet.write_number_with_format(row, 0, f64::from(row), &top)?;
                sheet.write_string_with_format(row, 1, self.zone.date_time(s.time_ms), &top)?;
                sheet.write_number_with_format(row, 2, f64::from(s.pid), &top)?;
                sheet.write_number_with_format(row, 3, f64::from(s.tid), &top)?;
                sheet.write_string_with_format(row, 4, cell_text(&s.layer_name), &top)?;
                sheet.write_string_with_format(row, 5, cell_text(&s.function), &top)?;
                sheet.write_string_with_format(row, 6, s.kind.label(), &top)?;
                match s.sql.as_deref() {
                    Some(sql) => write_highlighted(sheet, row, 7, &cell_text(sql), &palette, &wrap)?,
                    None => {
                        sheet.write_string_with_format(
                            row,
                            7,
                            "（不明: このハンドルの準備を捕まえていない）",
                            &wrap,
                        )?;
                    }
                }
                if s.truncated {
                    sheet.write_string_with_format(row, 8, "切り詰め", &top)?;
                }
                if let Some(c) = &s.caller {
                    sheet.write_string_with_format(row, 9, cell_text(&c.display()), &top)?;
                }
                if let Some(h) = s.handle {
                    sheet.write_string_with_format(row, 10, format!("0x{h:x}"), &top)?;
                }
            }
            sheet.set_freeze_panes(1, 0)?;
            sheet.autofilter(0, 0, row.max(1), (columns.len() - 1) as u16)?;
        }
        {
            let sheet = workbook.add_worksheet();
            sheet.set_name("ログ")?;
            for (col, (name, width)) in [("時刻", 24.0), ("種類", 8.0), ("PID", 8.0), ("内容", 100.0)]
                .iter()
                .enumerate()
            {
                sheet.write_string_with_format(0, col as u16, *name, &header)?;
                sheet.set_column_width(col as u16, *width)?;
            }
            let mut row = 0u32;
            sheet.write_string(1, 3, cell_text(self.title))?;
            sheet.write_string(1, 1, "対象")?;
            row += 1;
            for entry in entries {
                let (kind, time_ms, pid, message) = match entry {
                    Entry::Sql(_) => continue,
                    Entry::Status {
                        time_ms, pid, message, ..
                    } => ("状態", *time_ms, *pid, message),
                    Entry::Warning { time_ms, pid, message } => ("警告", *time_ms, *pid, message),
                };
                if row + 2 >= MAX_ROWS {
                    break;
                }
                row += 1;
                sheet.write_string(row, 0, self.zone.date_time(time_ms))?;
                sheet.write_string(row, 1, kind)?;
                if let Some(pid) = pid {
                    sheet.write_number(row, 2, f64::from(pid))?;
                }
                sheet.write_string(row, 3, cell_text(message))?;
            }
            if skipped > 0 {
                sheet.write_string(row + 1, 1, "警告")?;
                sheet.write_string(
                    row + 1,
                    3,
                    format!("Excel の行の上限を超えたので、SQL {skipped} 件を書いていません。全件は NDJSON か HTML で見てください"),
                )?;
            }
            sheet.set_freeze_panes(1, 0)?;
        }
        workbook.save_to_buffer()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{Kind, SqlEntry};

    #[test]
    fn control_characters_are_removed_but_tab_and_newline_kept() {
        assert_eq!(cell_text("a\u{1}b\tc\r\nd\u{7f}"), "ab\tc\nd");
    }

    #[test]
    fn long_text_is_cut_to_the_cell_limit_with_a_mark() {
        let long = "あ".repeat(40_000);
        let cut = cell_text(&long);
        assert_eq!(cut.chars().count(), MAX_CELL_CHARS);
        assert!(cut.ends_with(CUT_MARK));
    }

    #[test]
    fn writes_a_workbook() {
        let entries = vec![Entry::Sql(SqlEntry {
            time_ms: 0,
            pid: 1,
            tid: 2,
            layer: "odbc".into(),
            layer_name: "ODBC".into(),
            function: "SQLExecDirectW".into(),
            kind: Kind::Execute,
            sql: Some("x".repeat(40_000)),
            truncated: true,
            handle: Some(0x10),
            caller: None,
        })];
        let bytes = XlsxReport {
            title: "試験",
            zone: TimeZone::default(),
        }
        .to_bytes(&entries)
        .unwrap();
        // xlsx は zip（先頭が PK）。
        assert_eq!(&bytes[..2], b"PK");
    }
}
