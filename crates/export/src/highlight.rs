//! SQL の色分けのための字句の切り分け。GUI、HTML、Excel、コンソールが同じ切り分けを使い、色だけを変える。
//!
//! 構文を解析するのではなく、見やすさのための軽い切り分け。どの方言（Oracle、SQL Server、ODBC）でも
//! 大きく外れないように、キーワード、文字列、数、コメント、バインド変数だけを見分ける。切り分けた断片を
//! つなぐと元の文字列に戻る（何も落とさない）。

use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Keyword,
    /// `'…'`（`N'…'` も）。
    String,
    Number,
    /// `-- …` と `/* … */`。
    Comment,
    /// `?`、`:name`、`:1`（Oracle）、`@name`（SQL Server）。
    Bind,
    /// 名前（表、列、関数）と、`"…"` / `[…]` で囲んだ名前。
    Identifier,
    /// 空白、記号など。
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub range: Range<usize>,
}

/// キーワード（大文字で並べる。比べるときは大文字小文字を区別しない）。よく使う関数も入れる。
const KEYWORDS: &[&str] = &[
    "ALL",
    "ALTER",
    "AND",
    "ANY",
    "AS",
    "ASC",
    "AVG",
    "BEGIN",
    "BETWEEN",
    "BODY",
    "BY",
    "CALL",
    "CASE",
    "CAST",
    "CHECK",
    "COALESCE",
    "COMMIT",
    "CONNECT",
    "CONSTRAINT",
    "COUNT",
    "CREATE",
    "CROSS",
    "DECLARE",
    "DECODE",
    "DEFAULT",
    "DELETE",
    "DESC",
    "DISTINCT",
    "DROP",
    "DUAL",
    "ELSE",
    "END",
    "EXCEPT",
    "EXEC",
    "EXECUTE",
    "EXISTS",
    "FALSE",
    "FETCH",
    "FIRST",
    "FOR",
    "FOREIGN",
    "FROM",
    "FULL",
    "FUNCTION",
    "GRANT",
    "GROUP",
    "HAVING",
    "IF",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INTERSECT",
    "INTO",
    "IS",
    "JOIN",
    "KEY",
    "LEFT",
    "LEVEL",
    "LIKE",
    "LIMIT",
    "LOOP",
    "MATCHED",
    "MAX",
    "MERGE",
    "MIN",
    "MINUS",
    "NEXT",
    "NOT",
    "NOWAIT",
    "NULL",
    "NVL",
    "OFFSET",
    "ON",
    "ONLY",
    "OR",
    "ORDER",
    "OUTER",
    "OVER",
    "PACKAGE",
    "PARTITION",
    "PRIMARY",
    "PRIOR",
    "PROCEDURE",
    "REFERENCES",
    "RETURN",
    "RETURNING",
    "REVOKE",
    "RIGHT",
    "ROLLBACK",
    "ROW",
    "ROWNUM",
    "ROWS",
    "SAVEPOINT",
    "SELECT",
    "SEQUENCE",
    "SET",
    "START",
    "SUM",
    "SYSDATE",
    "TABLE",
    "THEN",
    "TOP",
    "TRIGGER",
    "TRUE",
    "TRUNCATE",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USING",
    "VALUES",
    "VIEW",
    "WHEN",
    "WHERE",
    "WHILE",
    "WITH",
];

fn is_keyword(word: &str) -> bool {
    KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(word))
}

/// 名前に使える文字（日本語の名前も名前として扱う）。
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$' || c == '#'
}

pub fn tokenize(sql: &str) -> Vec<Token> {
    let mut tokens: Vec<Token> = Vec::new();
    let chars: Vec<(usize, char)> = sql.char_indices().collect();
    let end_of = |i: usize| chars.get(i).map_or(sql.len(), |&(at, _)| at);
    let mut push = |kind: TokenKind, start: usize, end: usize| {
        // 同じ種類が続けば 1 つにまとめる（記号と空白が細切れにならないように）。
        if let Some(last) = tokens.last_mut() {
            if last.kind == kind && last.range.end == start && kind == TokenKind::Other {
                last.range.end = end;
                return;
            }
        }
        tokens.push(Token {
            kind,
            range: start..end,
        });
    };
    let mut i = 0;
    while i < chars.len() {
        let (start, c) = chars[i];
        let next = chars.get(i + 1).map(|&(_, c)| c);
        let (kind, length) = match c {
            '-' if next == Some('-') => {
                let n = chars[i..]
                    .iter()
                    .position(|&(_, c)| c == '\n')
                    .unwrap_or(chars.len() - i);
                (TokenKind::Comment, n)
            }
            '/' if next == Some('*') => {
                let n = (i + 2..chars.len().saturating_sub(1))
                    .find(|&j| chars[j].1 == '*' && chars[j + 1].1 == '/')
                    .map_or(chars.len() - i, |j| j + 2 - i);
                (TokenKind::Comment, n)
            }
            '\'' => (TokenKind::String, quoted(&chars[i..], '\'')),
            'N' | 'n' if next == Some('\'') => (TokenKind::String, 1 + quoted(&chars[i + 1..], '\'')),
            '"' => (TokenKind::Identifier, quoted(&chars[i..], '"')),
            '[' => {
                let n = chars[i..]
                    .iter()
                    .position(|&(_, c)| c == ']')
                    .map_or(chars.len() - i, |p| p + 1);
                (TokenKind::Identifier, n)
            }
            '?' => (TokenKind::Bind, 1),
            ':' | '@' if next.is_some_and(is_word_char) => {
                let n = 1 + chars[i + 1..].iter().take_while(|&&(_, c)| is_word_char(c)).count();
                (TokenKind::Bind, n)
            }
            c if c.is_ascii_digit() || (c == '.' && next.is_some_and(|n| n.is_ascii_digit())) => {
                let mut n = 0;
                let mut seen_exponent = false;
                while let Some(&(_, d)) = chars.get(i + n) {
                    let exponent_sign =
                        seen_exponent && (d == '+' || d == '-') && matches!(chars[i + n - 1].1, 'e' | 'E');
                    if d.is_ascii_digit() || d == '.' || exponent_sign {
                        n += 1;
                    } else if (d == 'e' || d == 'E') && !seen_exponent {
                        seen_exponent = true;
                        n += 1;
                    } else {
                        break;
                    }
                }
                (TokenKind::Number, n)
            }
            c if is_word_char(c) => {
                let n = chars[i..].iter().take_while(|&&(_, c)| is_word_char(c)).count();
                let word = &sql[start..end_of(i + n)];
                let kind = if is_keyword(word) {
                    TokenKind::Keyword
                } else {
                    TokenKind::Identifier
                };
                (kind, n)
            }
            _ => (TokenKind::Other, 1),
        };
        push(kind, start, end_of(i + length));
        i += length;
    }
    tokens
}

/// `quote` で始まる囲みの長さ（文字数）。囲みの中の `quote` 2 つは 1 つの文字。閉じていなければ最後まで。
fn quoted(chars: &[(usize, char)], quote: char) -> usize {
    let mut j = 1;
    while j < chars.len() {
        if chars[j].1 == quote {
            if chars.get(j + 1).is_some_and(|&(_, c)| c == quote) {
                j += 2;
                continue;
            }
            return j + 1;
        }
        j += 1;
    }
    chars.len()
}

/// 色（RGB）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// `#0033b3` の形（HTML と Excel）。
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

impl TokenKind {
    /// 白い背景（HTML、Excel）での色。`None` は文字の既定の色のまま。
    pub fn light(self) -> Option<Rgb> {
        match self {
            TokenKind::Keyword => Some(Rgb(0x00, 0x33, 0xb3)),
            TokenKind::String => Some(Rgb(0xa3, 0x15, 0x15)),
            TokenKind::Number => Some(Rgb(0x09, 0x86, 0x58)),
            TokenKind::Comment => Some(Rgb(0x6a, 0x73, 0x7d)),
            TokenKind::Bind => Some(Rgb(0x82, 0x50, 0xdf)),
            TokenKind::Identifier | TokenKind::Other => None,
        }
    }

    /// 暗い背景（GUI）での色。
    pub fn dark(self) -> Option<Rgb> {
        match self {
            TokenKind::Keyword => Some(Rgb(0x56, 0x9c, 0xd6)),
            TokenKind::String => Some(Rgb(0xce, 0x91, 0x78)),
            TokenKind::Number => Some(Rgb(0xb5, 0xce, 0xa8)),
            TokenKind::Comment => Some(Rgb(0x6a, 0x99, 0x55)),
            TokenKind::Bind => Some(Rgb(0xc5, 0x86, 0xc0)),
            TokenKind::Identifier | TokenKind::Other => None,
        }
    }

    /// コンソールの ANSI の色（16 色。どの配色のコンソールでも読める組み合わせ）。
    pub fn ansi(self) -> Option<&'static str> {
        match self {
            TokenKind::Keyword => Some("\x1b[94m"),
            TokenKind::String => Some("\x1b[33m"),
            TokenKind::Number => Some("\x1b[36m"),
            TokenKind::Comment => Some("\x1b[90m"),
            TokenKind::Bind => Some("\x1b[95m"),
            TokenKind::Identifier | TokenKind::Other => None,
        }
    }

    /// 太字にするか（キーワードだけ）と、斜体にするか（コメントだけ）。
    pub fn bold(self) -> bool {
        self == TokenKind::Keyword
    }

    pub fn italic(self) -> bool {
        self == TokenKind::Comment
    }
}

pub const ANSI_RESET: &str = "\x1b[0m";

/// 断片ごとの文字列と種類。
pub fn pieces(sql: &str) -> impl Iterator<Item = (TokenKind, &str)> {
    tokenize(sql).into_iter().map(move |t| (t.kind, &sql[t.range]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use TokenKind::*;

    fn kinds(sql: &str) -> Vec<(TokenKind, &str)> {
        pieces(sql).filter(|(k, _)| *k != Other).collect()
    }

    #[test]
    fn splits_keywords_strings_numbers_binds_and_comments() {
        assert_eq!(
            kinds("select a, 'it''s' from t where x = :id and y > 1.5e-3 -- note\n/* c */ ?"),
            [
                (Keyword, "select"),
                (Identifier, "a"),
                (String, "'it''s'"),
                (Keyword, "from"),
                (Identifier, "t"),
                (Keyword, "where"),
                (Identifier, "x"),
                (Bind, ":id"),
                (Keyword, "and"),
                (Identifier, "y"),
                (Number, "1.5e-3"),
                (Comment, "-- note"),
                (Comment, "/* c */"),
                (Bind, "?"),
            ]
        );
    }

    #[test]
    fn dialect_specific_forms() {
        assert_eq!(
            kinds("SELECT N'日本', [Order], \"列\", @p1 FROM 表1"),
            [
                (Keyword, "SELECT"),
                (String, "N'日本'"),
                (Identifier, "[Order]"),
                (Identifier, "\"列\""),
                (Bind, "@p1"),
                (Keyword, "FROM"),
                (Identifier, "表1"),
            ]
        );
        // 名前の中の数字や、キーワードを含む名前は分けない。
        assert_eq!(kinds("col1 selected"), [(Identifier, "col1"), (Identifier, "selected")]);
    }

    #[test]
    fn unterminated_parts_run_to_the_end() {
        assert_eq!(kinds("x = 'abc"), [(Identifier, "x"), (String, "'abc")]);
        assert_eq!(kinds("/* open"), [(Comment, "/* open")]);
    }

    #[test]
    fn pieces_join_back_to_the_input() {
        for sql in [
            "SELECT a,\r\n  b -- x\r\nFROM t\nWHERE s = 'a\r\nb'\r\n",
            "",
            "'",
            "::",
            "a:=1",
            "1e",
            "日本語 の 名前",
        ] {
            let joined: std::string::String = pieces(sql).map(|(_, s)| s).collect();
            assert_eq!(joined, sql);
        }
    }
}
