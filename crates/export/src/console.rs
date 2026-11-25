//! コンソール: 人が読む形で 1 件ずつ出す。SQL は `sql` に、状態と警告は `log` に書く（cli は標準出力と
//! 標準エラーを渡す。SQL だけをリダイレクトでファイルに取れる）。

use std::io::{self, Write};

use crate::entry::Entry;
use crate::highlight::{pieces, ANSI_RESET};
use crate::time::TimeZone;

pub struct ConsoleWriter<S: Write, L: Write> {
    sql: S,
    log: L,
    zone: TimeZone,
    /// `false` なら SQL を出さず、状態と警告だけを出す。
    show_sql: bool,
    color: bool,
}

/// SQL を 1 行ずつ 4 文字の字下げで並べる。複数行にまたがる文字列やコメントでも、色は行ごとに付け直す
/// （行の終わりで色を戻さないと、字下げや次の出力まで色が付く）。CR は落とし、最後の改行は 1 つにする。
fn indented(sql: &str, color: bool) -> String {
    let mut out = String::from("    ");
    for (kind, text) in pieces(sql) {
        let code = if color { kind.ansi() } else { None };
        for (i, segment) in text.split('\n').enumerate() {
            if i > 0 {
                out.push_str("\n    ");
            }
            let segment = segment.strip_suffix('\r').unwrap_or(segment);
            match code {
                Some(code) if !segment.is_empty() => {
                    out.push_str(code);
                    out.push_str(segment);
                    out.push_str(ANSI_RESET);
                }
                _ => out.push_str(segment),
            }
        }
    }
    // SQL が改行で終わるときの、最後の空の行を出さない。
    let trimmed = out.trim_end_matches(['\n', ' ']).len();
    out.truncate(trimmed);
    out.push('\n');
    out
}

impl<S: Write, L: Write> ConsoleWriter<S, L> {
    pub fn new(sql: S, log: L, zone: TimeZone) -> Self {
        Self {
            sql,
            log,
            zone,
            show_sql: true,
            color: false,
        }
    }

    pub fn show_sql(mut self, show: bool) -> Self {
        self.show_sql = show;
        self
    }

    /// SQL を ANSI の色で色分けする（端末に出すときだけ。ファイルに書くときは付けない）。
    pub fn color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    pub fn write(&mut self, entry: &Entry) -> io::Result<()> {
        match entry {
            Entry::Sql(_) if !self.show_sql => Ok(()),
            Entry::Sql(e) => {
                let caller = e.caller.as_ref().map(|c| c.display()).unwrap_or_default();
                writeln!(
                    self.sql,
                    "{}  PID {}  TID {}  {}  {}  [{}]  {caller}",
                    self.zone.clock(e.time_ms),
                    e.pid,
                    e.tid,
                    e.layer_name,
                    e.function,
                    e.kind.label()
                )?;
                match &e.sql {
                    Some(sql) => {
                        self.sql.write_all(indented(sql, self.color).as_bytes())?;
                        if e.truncated {
                            writeln!(self.sql, "    …（長いので切り詰めました）")?;
                        }
                    }
                    None => writeln!(
                        self.sql,
                        "    （SQL 不明: このハンドルの準備を捕まえていません。トレースの開始前に準備した、など）"
                    )?,
                }
                self.sql.flush()
            }
            Entry::Status { message, .. } => writeln!(self.log, "{message}"),
            Entry::Warning { message, .. } => writeln!(self.log, "警告: {message}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{Caller, Kind, SqlEntry};

    #[test]
    fn sql_goes_to_sql_and_status_to_log() {
        let mut sql = Vec::new();
        let mut log = Vec::new();
        let mut console = ConsoleWriter::new(&mut sql, &mut log, TimeZone { offset_minutes: 540 });
        console
            .write(&Entry::Sql(SqlEntry {
                time_ms: 1_795_669_385_123,
                pid: 1,
                tid: 2,
                layer: "odbc".into(),
                layer_name: "ODBC".into(),
                function: "SQLExecute".into(),
                kind: Kind::Execute,
                sql: Some("SELECT 1\nFROM dual".into()),
                truncated: false,
                handle: Some(7),
                caller: Some(Caller {
                    address: 0x1_2363,
                    module: Some("app.exe".into()),
                    offset: 0x2363,
                }),
            }))
            .unwrap();
        console
            .write(&Entry::Warning {
                time_ms: 0,
                pid: None,
                message: "x".into(),
            })
            .unwrap();
        assert_eq!(
            String::from_utf8(sql).unwrap(),
            "14:03:05.123  PID 1  TID 2  ODBC  SQLExecute  [実行]  app.exe+0x2363\n    SELECT 1\n    FROM dual\n"
        );
        assert_eq!(String::from_utf8(log).unwrap(), "警告: x\n");
    }
}

#[cfg(test)]
mod color_tests {
    use super::*;

    #[test]
    fn colors_are_reset_at_every_line_end() {
        let out = indented("SELECT 'a\r\nb' -- c\nFROM t\n", true);
        assert_eq!(
            out,
            "    \x1b[94mSELECT\x1b[0m \x1b[33m'a\x1b[0m\n    \x1b[33mb'\x1b[0m \x1b[90m-- c\x1b[0m\n    \x1b[94mFROM\x1b[0m t\n"
        );
    }

    #[test]
    fn without_color_it_is_plain_text() {
        assert_eq!(indented("SELECT 1\r\nFROM t\r\n", false), "    SELECT 1\n    FROM t\n");
    }
}
