//! HTML: 1 枚で完結する報告（外部のファイルを読まない）。SQL を含む値はすべてエスケープする
//! （SQL の文字列にはどんな文字も入りうる。`</script>` などで報告を壊さないため）。

use std::io::{self, Write};

use crate::entry::Entry;
use crate::highlight::{pieces, TokenKind};
use crate::time::TimeZone;

const STYLE: &str = r#"
body { font-family: "Segoe UI", "Yu Gothic UI", "Meiryo", sans-serif; margin: 1.5rem; color: #1f2328; }
h1 { font-size: 1.3rem; margin: 0 0 .3rem; }
.meta { color: #59636e; margin-bottom: 1rem; }
.tools { position: sticky; top: 0; background: #fff; padding: .5rem 0; display: flex; gap: 1rem; align-items: center; }
.tools input[type=search] { width: 28rem; padding: .3rem .5rem; }
table { border-collapse: collapse; width: 100%; font-size: .85rem; }
th, td { border-bottom: 1px solid #d1d9e0; padding: .3rem .5rem; text-align: left; vertical-align: top; }
th { background: #f6f8fa; position: sticky; top: 2.6rem; }
td.num { text-align: right; font-variant-numeric: tabular-nums; }
pre { margin: 0; white-space: pre-wrap; word-break: break-all; font-family: Consolas, "BIZ UDGothic", monospace; }
.unknown { color: #8c959f; font-style: italic; }
.badge { display: inline-block; font-size: .75rem; padding: 0 .4rem; border-radius: .6rem; background: #fff8c5; }
.prepare td { color: #59636e; }
.log li.warning { color: #9a6700; }
"#;

/// 絞り込み（大文字小文字を区別しない部分一致）と、実行だけを表示する切り替え。
const SCRIPT: &str = r#"
const q = document.getElementById('q');
const onlyExec = document.getElementById('exec');
const rows = Array.from(document.querySelectorAll('#sql tbody tr'));
const count = document.getElementById('count');
function apply() {
  const needle = q.value.toLowerCase();
  let shown = 0;
  for (const r of rows) {
    const ok = (!onlyExec.checked || r.dataset.kind === 'execute') &&
      (needle === '' || r.textContent.toLowerCase().includes(needle));
    r.hidden = !ok;
    if (ok) shown++;
  }
  count.textContent = shown + ' / ' + rows.length + ' 件';
}
q.addEventListener('input', apply);
onlyExec.addEventListener('change', apply);
apply();
"#;

/// 色分けの CSS（色は [`TokenKind::light`] で決める）。
fn token_style() -> String {
    let mut css = String::new();
    for kind in KINDS {
        if let Some(color) = kind.light() {
            let weight = if kind.bold() { "font-weight: 600;" } else { "" };
            let style = if kind.italic() { "font-style: italic;" } else { "" };
            css.push_str(&format!(
                ".{} {{ color: {}; {weight}{style} }}
",
                class(kind),
                color.hex()
            ));
        }
    }
    css
}

const KINDS: [TokenKind; 5] = [
    TokenKind::Keyword,
    TokenKind::String,
    TokenKind::Number,
    TokenKind::Comment,
    TokenKind::Bind,
];

fn class(kind: TokenKind) -> &'static str {
    match kind {
        TokenKind::Keyword => "sk",
        TokenKind::String => "ss",
        TokenKind::Number => "sn",
        TokenKind::Comment => "sc",
        TokenKind::Bind => "sb",
        TokenKind::Identifier | TokenKind::Other => "",
    }
}

/// SQL を色分けした HTML（断片ごとにエスケープする）。
pub fn highlighted(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len() * 2);
    for (kind, text) in pieces(sql) {
        match kind.light() {
            Some(_) => {
                out.push_str(&format!("<span class=\"{}\">{}</span>", class(kind), escape(text)));
            }
            None => out.push_str(&escape(text)),
        }
    }
    out
}

/// 文字を HTML の本文と属性の値に置ける形にする。
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// 報告の見出し（何をトレースしたか）。
pub struct HtmlReport<'a> {
    pub title: &'a str,
    pub zone: TimeZone,
}

impl HtmlReport<'_> {
    pub fn write(&self, mut out: impl Write, entries: &[Entry]) -> io::Result<()> {
        let e = escape;
        let sql_count = entries.iter().filter(|x| matches!(x, Entry::Sql(_))).count();
        let span = match (entries.first(), entries.last()) {
            (Some(first), Some(last)) => format!(
                "{} 〜 {}",
                self.zone.date_time(first.time_ms()),
                self.zone.date_time(last.time_ms())
            ),
            _ => "記録なし".into(),
        };
        write!(
            out,
            "<!DOCTYPE html>\n<html lang=\"ja\">\n<head>\n<meta charset=\"utf-8\">\n<title>{title} - sqlTrace</title>\n\
             <style>{STYLE}{colors}</style>\n</head>\n<body>\n<h1>{title}</h1>\n<div class=\"meta\">{span}・SQL {sql_count} 件</div>\n\
             <div class=\"tools\"><input type=\"search\" id=\"q\" placeholder=\"絞り込み（SQL、関数、呼び出し元など）\">\
             <label><input type=\"checkbox\" id=\"exec\"> 実行だけ</label><span id=\"count\"></span></div>\n\
             <table id=\"sql\">\n<thead><tr><th>#</th><th>時刻</th><th>PID</th><th>TID</th><th>層</th><th>関数</th>\
             <th>種別</th><th>SQL</th><th>呼び出し元</th><th>ハンドル</th></tr></thead>\n<tbody>\n",
            title = e(self.title),
            colors = token_style(),
        )?;
        let mut number = 0;
        for entry in entries {
            let Entry::Sql(s) = entry else {
                continue;
            };
            number += 1;
            let sql = match &s.sql {
                Some(sql) => {
                    let badge = if s.truncated {
                        " <span class=\"badge\">切り詰め</span>"
                    } else {
                        ""
                    };
                    format!("<pre>{}</pre>{badge}", highlighted(sql))
                }
                None => "<span class=\"unknown\">不明（このハンドルの準備を捕まえていない）</span>".into(),
            };
            writeln!(
                out,
                "<tr class=\"{kind}\" data-kind=\"{kind}\"><td class=\"num\">{number}</td><td>{time}</td>\
                 <td class=\"num\">{pid}</td><td class=\"num\">{tid}</td><td>{layer}</td><td>{function}</td>\
                 <td>{label}</td><td>{sql}</td><td>{caller}</td><td>{handle}</td></tr>",
                kind = s.kind.id(),
                time = self.zone.date_time(s.time_ms),
                pid = s.pid,
                tid = s.tid,
                layer = e(&s.layer_name),
                function = e(&s.function),
                label = s.kind.label(),
                caller = e(&s.caller.as_ref().map(|c| c.display()).unwrap_or_default()),
                handle = s.handle.map(|h| format!("0x{h:x}")).unwrap_or_default(),
            )?;
        }
        writeln!(out, "</tbody>\n</table>\n<h2>ログ</h2>\n<ul class=\"log\">")?;
        for entry in entries {
            let (class, time_ms, message) = match entry {
                Entry::Sql(_) => continue,
                Entry::Status { time_ms, message, .. } => ("status", *time_ms, message),
                Entry::Warning { time_ms, message, .. } => ("warning", *time_ms, message),
            };
            writeln!(
                out,
                "<li class=\"{class}\">{}  {}</li>",
                self.zone.date_time(time_ms),
                e(message)
            )?;
        }
        writeln!(out, "</ul>\n<script>{SCRIPT}</script>\n</body>\n</html>")?;
        out.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{Kind, SqlEntry};

    #[test]
    fn everything_from_the_target_is_escaped() {
        let hostile = "SELECT '</pre></script><script>alert(1)</script>' & \"x\"";
        let entries = vec![Entry::Sql(SqlEntry {
            time_ms: 0,
            pid: 1,
            tid: 2,
            layer: "odbc".into(),
            layer_name: "ODBC".into(),
            function: "SQLExecDirectW".into(),
            kind: Kind::Execute,
            sql: Some(hostile.into()),
            truncated: false,
            handle: None,
            caller: None,
        })];
        let mut out = Vec::new();
        HtmlReport {
            title: "<app>",
            zone: TimeZone::default(),
        }
        .write(&mut out, &entries)
        .unwrap();
        let html = String::from_utf8(out).unwrap();
        assert!(!html.contains("<script>alert"));
        assert!(!html.contains("<app>"));
        assert!(html.contains("&lt;/script&gt;&lt;script&gt;alert(1)"));
        // 本物の script は最後の 1 つだけ。
        assert_eq!(html.matches("<script>").count(), 1);
    }
}
