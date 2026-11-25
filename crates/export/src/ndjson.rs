//! NDJSON（1 行に 1 つの JSON）: トレースの生の記録。保存しておき、後で HTML や Excel に書き出せる。
//!
//! 依存を増やさないよう、この形式を読み書きできるだけの小さな JSON の処理を持つ。64 ビットの値
//! （ハンドル、アドレス）は JSON の数（倍精度）では落ちるので `"0x…"` の文字列にする。

use std::fmt::Write as _;
use std::io::{self, BufRead, Write};

use crate::entry::{Caller, Entry, Kind, SqlEntry};

/// 1 行目に書く見出し。読むときに形式と版を確かめる。
const FORMAT: &str = "sqltrace-ndjson";
const VERSION: i64 = 1;

pub struct NdjsonWriter<W: Write> {
    out: W,
}

impl<W: Write> NdjsonWriter<W> {
    pub fn new(mut out: W) -> io::Result<Self> {
        writeln!(out, r#"{{"type":"header","format":"{FORMAT}","version":{VERSION}}}"#)?;
        Ok(Self { out })
    }

    /// 1 件ずつ書いてすぐ流す（強制終了されても、そこまでの記録は残る）。
    pub fn write(&mut self, entry: &Entry) -> io::Result<()> {
        let mut line = String::new();
        encode(entry, &mut line);
        line.push('\n');
        self.out.write_all(line.as_bytes())?;
        self.out.flush()
    }

    pub fn into_inner(self) -> W {
        self.out
    }
}

fn encode(entry: &Entry, s: &mut String) {
    let pid = |s: &mut String, pid: Option<u32>| match pid {
        Some(p) => write!(s, "{p}").ok(),
        None => {
            s.push_str("null");
            Some(())
        }
    };
    match entry {
        Entry::Sql(e) => {
            write!(
                s,
                r#"{{"type":"sql","time":{},"pid":{},"tid":{},"layer":"#,
                e.time_ms, e.pid, e.tid
            )
            .ok();
            string(s, &e.layer);
            s.push_str(r#","layer_name":"#);
            string(s, &e.layer_name);
            s.push_str(r#","function":"#);
            string(s, &e.function);
            write!(s, r#","kind":"{}","sql":"#, e.kind.id()).ok();
            optional(s, e.sql.as_deref());
            write!(s, r#","truncated":{},"handle":"#, e.truncated).ok();
            optional(s, e.handle.map(hex).as_deref());
            s.push_str(r#","caller":"#);
            match &e.caller {
                None => s.push_str("null"),
                Some(c) => {
                    s.push_str(r#"{"address":"#);
                    string(s, &hex(c.address));
                    s.push_str(r#","module":"#);
                    optional(s, c.module.as_deref());
                    s.push_str(r#","offset":"#);
                    string(s, &hex(c.offset));
                    s.push('}');
                }
            }
            s.push('}');
        }
        Entry::Status {
            time_ms,
            pid: p,
            event,
            message,
        } => {
            write!(s, r#"{{"type":"status","time":{time_ms},"pid":"#).ok();
            pid(s, *p);
            s.push_str(r#","event":"#);
            string(s, event);
            s.push_str(r#","message":"#);
            string(s, message);
            s.push('}');
        }
        Entry::Warning {
            time_ms,
            pid: p,
            message,
        } => {
            write!(s, r#"{{"type":"warning","time":{time_ms},"pid":"#).ok();
            pid(s, *p);
            s.push_str(r#","message":"#);
            string(s, message);
            s.push('}');
        }
    }
}

fn hex(value: u64) -> String {
    format!("0x{value:x}")
}

fn optional(s: &mut String, value: Option<&str>) {
    match value {
        Some(v) => string(s, v),
        None => s.push_str("null"),
    }
}

fn string(s: &mut String, value: &str) {
    s.push('"');
    for c in value.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            '\t' => s.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                write!(s, "\\u{:04x}", c as u32).ok();
            }
            c => s.push(c),
        }
    }
    s.push('"');
}

#[derive(Debug, PartialEq, Eq)]
pub struct ReadError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} 行目を読めません（{}）。sqltrace が書いた NDJSON か確かめてください",
            self.line, self.message
        )
    }
}

impl std::error::Error for ReadError {}

/// sqltrace が書いた NDJSON を読む。最後の行が途中で切れていたら（書いている途中で強制終了された）、そこは捨てる。
pub fn read(input: impl BufRead) -> Result<Vec<Entry>, ReadError> {
    let lines: Vec<String> = input.lines().collect::<Result<_, _>>().map_err(|e| ReadError {
        line: 0,
        message: e.to_string(),
    })?;
    let last = lines.iter().rposition(|l| !l.trim().is_empty());
    let mut entries = Vec::new();
    let mut saw_header = false;
    for (index, text) in lines.iter().enumerate() {
        let line = index + 1;
        if text.trim().is_empty() {
            continue;
        }
        let error = |message: String| ReadError { line, message };
        let value = match Parser::new(text).document() {
            Ok(v) => v,
            Err(_) if Some(index) == last && saw_header => break,
            Err(message) => return Err(error(message)),
        };
        if !saw_header {
            if value.get("type").and_then(Json::as_str) != Some("header")
                || value.get("format").and_then(Json::as_str) != Some(FORMAT)
            {
                return Err(error("sqltrace の NDJSON の見出しがありません".into()));
            }
            let version = value.get("version").and_then(Json::as_i64);
            if version != Some(VERSION) {
                return Err(error(format!("知らない版です（{version:?}）")));
            }
            saw_header = true;
            continue;
        }
        entries.push(decode(&value).map_err(error)?);
    }
    if !saw_header {
        return Err(ReadError {
            line: 1,
            message: "空です".into(),
        });
    }
    Ok(entries)
}

fn decode(v: &Json) -> Result<Entry, String> {
    let str_of = |key: &str| {
        v.get(key)
            .and_then(Json::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("{key} がありません"))
    };
    let opt_str = |value: Option<&Json>| value.and_then(Json::as_str).map(str::to_owned);
    let time_ms = v.get("time").and_then(Json::as_i64).ok_or("time がありません")?;
    let pid = v.get("pid").and_then(Json::as_i64).map(|p| p as u32);
    match v.get("type").and_then(Json::as_str) {
        Some("sql") => {
            let kind = Kind::from_id(&str_of("kind")?).ok_or("kind が違います")?;
            let caller = match v.get("caller") {
                Some(Json::Null) | None => None,
                Some(c) => Some(Caller {
                    address: parse_hex(c.get("address")).ok_or("caller.address が違います")?,
                    module: opt_str(c.get("module")),
                    offset: parse_hex(c.get("offset")).ok_or("caller.offset が違います")?,
                }),
            };
            Ok(Entry::Sql(SqlEntry {
                time_ms,
                pid: pid.ok_or("pid がありません")?,
                tid: v.get("tid").and_then(Json::as_i64).ok_or("tid がありません")? as u32,
                layer: str_of("layer")?,
                layer_name: str_of("layer_name")?,
                function: str_of("function")?,
                kind,
                sql: opt_str(v.get("sql")),
                truncated: matches!(v.get("truncated"), Some(Json::Bool(true))),
                handle: parse_hex(v.get("handle")),
                caller,
            }))
        }
        Some("status") => Ok(Entry::Status {
            time_ms,
            pid,
            event: str_of("event")?,
            message: str_of("message")?,
        }),
        Some("warning") => Ok(Entry::Warning {
            time_ms,
            pid,
            message: str_of("message")?,
        }),
        other => Err(format!("知らない type です（{other:?}）")),
    }
}

fn parse_hex(value: Option<&Json>) -> Option<u64> {
    let text = value?.as_str()?;
    u64::from_str_radix(text.strip_prefix("0x")?, 16).ok()
}

#[derive(Debug, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    /// 数は元の文字列のまま持つ（時刻のミリ秒を倍精度にしないため）。
    Number(String),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Number(n) => n.parse().ok(),
            _ => None,
        }
    }
}

struct Parser<'a> {
    text: &'a str,
    position: usize,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, position: 0 }
    }

    fn document(mut self) -> Result<Json, String> {
        let value = self.value()?;
        self.skip_space();
        if self.position != self.text.len() {
            return Err(format!("{} 文字目の後ろに余計なものがあります", self.position));
        }
        Ok(value)
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.position).copied()
    }

    fn skip_space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.position += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), String> {
        self.skip_space();
        if self.peek() == Some(byte) {
            self.position += 1;
            Ok(())
        } else {
            Err(format!("{} 文字目に '{}' がありません", self.position, byte as char))
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.skip_space();
        let rest = &self.text[self.position..];
        for (word, value) in [
            ("null", Json::Null),
            ("true", Json::Bool(true)),
            ("false", Json::Bool(false)),
        ] {
            if rest.starts_with(word) {
                self.position += word.len();
                return Ok(value);
            }
        }
        match self.peek() {
            Some(b'"') => self.string().map(Json::String),
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'-' | b'0'..=b'9') => {
                let start = self.position;
                while matches!(self.peek(), Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')) {
                    self.position += 1;
                }
                Ok(Json::Number(self.text[start..self.position].to_owned()))
            }
            _ => Err(format!("{} 文字目に値がありません", self.position)),
        }
    }

    fn object(&mut self) -> Result<Json, String> {
        self.expect(b'{')?;
        let mut fields = Vec::new();
        self.skip_space();
        if self.peek() == Some(b'}') {
            self.position += 1;
            return Ok(Json::Object(fields));
        }
        loop {
            self.skip_space();
            let key = self.string()?;
            self.expect(b':')?;
            fields.push((key, self.value()?));
            self.skip_space();
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(b'}') => {
                    self.position += 1;
                    return Ok(Json::Object(fields));
                }
                _ => return Err(format!("{} 文字目に ',' か '}}' がありません", self.position)),
            }
        }
    }

    fn array(&mut self) -> Result<Json, String> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_space();
        if self.peek() == Some(b']') {
            self.position += 1;
            return Ok(Json::Array(items));
        }
        loop {
            items.push(self.value()?);
            self.skip_space();
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(b']') => {
                    self.position += 1;
                    return Ok(Json::Array(items));
                }
                _ => return Err(format!("{} 文字目に ',' か ']' がありません", self.position)),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self
            .text
            .get(self.position..self.position + 4)
            .ok_or("\\u の後ろが短い")?;
        self.position += 4;
        u32::from_str_radix(digits, 16).map_err(|_| format!("\\u{digits} は 16 進ではありません"))
    }

    fn string(&mut self) -> Result<String, String> {
        if self.peek() != Some(b'"') {
            return Err(format!("{} 文字目に文字列がありません", self.position));
        }
        self.position += 1;
        let mut out = String::new();
        loop {
            let rest = &self.text[self.position..];
            let Some(c) = rest.chars().next() else {
                return Err("文字列が閉じていません".into());
            };
            self.position += c.len_utf8();
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let escape = self.peek().ok_or("\\ の後ろがありません")?;
                    self.position += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'u' => {
                            let mut code = self.hex4()?;
                            // サロゲートの組（BMP の外の文字）。
                            if (0xD800..0xDC00).contains(&code) && self.text[self.position..].starts_with("\\u") {
                                self.position += 2;
                                let low = self.hex4()?;
                                code = 0x10000 + ((code - 0xD800) << 10) + (low.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                        }
                        other => return Err(format!("知らないエスケープ \\{}", other as char)),
                    }
                }
                c => out.push(c),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Entry> {
        vec![
            Entry::Status {
                time_ms: 1,
                pid: Some(10),
                event: "attached".into(),
                message: "PID 10 にアタッチしました".into(),
            },
            Entry::Sql(SqlEntry {
                time_ms: 1_795_669_385_123,
                pid: 10,
                tid: 20,
                layer: "odbc".into(),
                layer_name: "ODBC".into(),
                function: "SQLExecDirectW".into(),
                kind: Kind::Execute,
                sql: Some("SELECT '日本\"\\\n\t\u{1}' -- 😀".into()),
                truncated: true,
                handle: Some(u64::MAX),
                caller: Some(Caller {
                    address: 0x7ff6_1234_5678,
                    module: Some("app.exe".into()),
                    offset: 0x5678,
                }),
            }),
            Entry::Sql(SqlEntry {
                time_ms: 2,
                pid: 10,
                tid: 21,
                layer: "oci".into(),
                layer_name: "Oracle OCI".into(),
                function: "OCIStmtExecute".into(),
                kind: Kind::Execute,
                sql: None,
                truncated: false,
                handle: None,
                caller: None,
            }),
            Entry::Warning {
                time_ms: 3,
                pid: None,
                message: "警告".into(),
            },
        ]
    }

    fn write_all(entries: &[Entry]) -> Vec<u8> {
        let mut writer = NdjsonWriter::new(Vec::new()).unwrap();
        for e in entries {
            writer.write(e).unwrap();
        }
        writer.into_inner()
    }

    #[test]
    fn round_trips_every_field() {
        let entries = sample();
        let bytes = write_all(&entries);
        assert_eq!(read(bytes.as_slice()).unwrap(), entries);
    }

    #[test]
    fn one_line_per_entry() {
        let bytes = write_all(&sample());
        // 改行は SQL の中でもエスケープされるので、行の数は見出し + 件数。
        assert_eq!(String::from_utf8(bytes).unwrap().lines().count(), 5);
    }

    #[test]
    fn torn_last_line_is_dropped() {
        let mut bytes = write_all(&sample());
        bytes.extend_from_slice(br#"{"type":"sql","time":5,"pi"#);
        assert_eq!(read(bytes.as_slice()).unwrap(), sample());
    }

    #[test]
    fn broken_middle_line_is_an_error() {
        let text = "{\"type\":\"header\",\"format\":\"sqltrace-ndjson\",\"version\":1}\n{oops}\n{\"type\":\"warning\",\"time\":1,\"pid\":null,\"message\":\"x\"}\n";
        assert_eq!(read(text.as_bytes()).unwrap_err().line, 2);
    }

    #[test]
    fn rejects_other_files() {
        assert!(read(&b"{\"a\":1}\n"[..]).is_err());
        assert!(read(&b""[..]).is_err());
    }

    #[test]
    fn parses_surrogate_pairs() {
        let v = Parser::new(r#""😀""#).document().unwrap();
        assert_eq!(v, Json::String("😀".into()));
    }
}
