//! 対象のメモリにある SQL の文字列を読んで、文字列にする。
//!
//! 読み取りはページ（4 KiB）単位に区切る。NUL 終端の文字列が確保済みの領域の端で終わっていても、
//! その先の未確保のページを読もうとして全体が失敗しないようにするため。

use std::fmt;

use crate::memory::Memory;
use crate::probe::{Encoding, Unit};

const PAGE_SIZE: u64 = 4096;

/// Windows の `CP_ACP`（対象と同じ機械の ANSI コードページ）。
pub const CP_ACP: u32 = 0;

/// バイト列をコードページで文字列にする。Windows では `MultiByteToWideChar`。OS に依存するので外から渡す。
pub trait CodepageDecoder {
    /// 変換できないバイトは置換文字にする（失敗にはしない）。
    fn decode(&self, bytes: &[u8], codepage: u32) -> String;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextOptions {
    /// 1 つの SQL で読む上限（バイト）。超えた分は切り捨てて `truncated` を立てる。
    pub max_bytes: usize,
    /// [`Encoding::Ansi`] のコードページ。
    pub ansi_codepage: u32,
    /// [`Encoding::Auto`] で UTF-16 でも UTF-8 でもなかったときのコードページ（OCI の NLS_LANG に合わせて上書きする）。
    pub auto_codepage: u32,
}

impl Default for TextOptions {
    fn default() -> Self {
        Self {
            max_bytes: 1 << 20,
            ansi_codepage: CP_ACP,
            auto_codepage: CP_ACP,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text {
    pub value: String,
    /// 上限で切り捨てたか、途中から読めなくなった。
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextError {
    /// 文字列の先頭から読めなかった。
    Unreadable { address: u64 },
}

impl fmt::Display for TextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TextError::Unreadable { address } => {
                write!(f, "SQL の文字列 (0x{address:x}) を対象のメモリから読めませんでした")
            }
        }
    }
}

/// 文字列を読む。`pointer` が 0 なら `Ok(None)`。`length` は（値, 単位）で、負なら NUL 終端。
pub fn read_text(
    memory: &dyn Memory,
    pointer: u64,
    length: Option<(i64, Unit)>,
    encoding: Encoding,
    decoder: &dyn CodepageDecoder,
    options: &TextOptions,
) -> Result<Option<Text>, TextError> {
    if pointer == 0 {
        return Ok(None);
    }
    let read = match length {
        Some((len, unit)) if len >= 0 => {
            let unit_size = if unit == Unit::Chars && encoding == Encoding::Utf16 {
                2
            } else {
                1
            };
            let wanted = usize::try_from(len).unwrap_or(usize::MAX).saturating_mul(unit_size);
            read_exact(memory, pointer, wanted, options.max_bytes)
        }
        _ => {
            let utf16 = match encoding {
                Encoding::Utf16 => true,
                Encoding::Ansi => false,
                Encoding::Auto => looks_like_utf16_start(memory, pointer),
            };
            read_until_nul(memory, pointer, if utf16 { 2 } else { 1 }, options.max_bytes)
        }
    };
    let Some((bytes, truncated)) = read else {
        return Err(TextError::Unreadable { address: pointer });
    };
    Ok(Some(Text {
        value: decode(&bytes, encoding, decoder, options),
        truncated,
    }))
}

fn decode(bytes: &[u8], encoding: Encoding, decoder: &dyn CodepageDecoder, options: &TextOptions) -> String {
    match encoding {
        Encoding::Utf16 => decode_utf16(bytes),
        Encoding::Ansi => decoder.decode(bytes, options.ansi_codepage),
        Encoding::Auto if is_probably_utf16(bytes) => decode_utf16(bytes),
        Encoding::Auto => match std::str::from_utf8(bytes) {
            // ASCII だけなら UTF-8 でもコードページでも同じ。非 ASCII を含んで UTF-8 として正しければ、
            // CP932 などがたまたま UTF-8 の並びになることはまずないので UTF-8 とみなす。
            Ok(s) => s.to_owned(),
            Err(_) => decoder.decode(bytes, options.auto_codepage),
        },
    }
}

fn decode_utf16(bytes: &[u8]) -> String {
    let mut units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    // 上限で切ったときに、サロゲートの組の前半だけが残ることがある。置換文字にせず落とす。
    if units.last().is_some_and(|u| (0xd800..0xdc00).contains(u)) {
        units.pop();
    }
    String::from_utf16_lossy(&units)
}

/// SQL はほぼ必ず ASCII のキーワードを含む。UTF-16 なら奇数番目のバイトの多くが 0 になり、
/// ANSI や UTF-8 の SQL では 0 は現れない。
fn is_probably_utf16(bytes: &[u8]) -> bool {
    if bytes.len() < 2 || !bytes.len().is_multiple_of(2) {
        return false;
    }
    let units = bytes.chunks_exact(2).take(256);
    let total = units.len();
    let ascii_like = units.filter(|c| c[1] == 0 && c[0] != 0).count();
    ascii_like * 4 >= total
}

/// NUL 終端で長さが分からないとき: 先頭の 1 文字が `X\0` なら UTF-16 とみなす。
fn looks_like_utf16_start(memory: &dyn Memory, pointer: u64) -> bool {
    let mut head = [0u8; 2];
    memory.read(pointer, &mut head) && head[0] != 0 && head[1] == 0
}

/// ページ境界で区切って読む。途中で読めなくなったらそこまでを返す（先頭から読めなければ `None`）。
fn read_exact(memory: &dyn Memory, pointer: u64, wanted: usize, max_bytes: usize) -> Option<(Vec<u8>, bool)> {
    let limit = wanted.min(max_bytes);
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut address = pointer;
    while bytes.len() < limit {
        let piece = piece_len(address, limit - bytes.len());
        let start = bytes.len();
        bytes.resize(start + piece, 0);
        if !memory.read(address, &mut bytes[start..]) {
            bytes.truncate(start);
            return if bytes.is_empty() && limit > 0 {
                None
            } else {
                Some((bytes, true))
            };
        }
        address += piece as u64;
    }
    Some((bytes, wanted > max_bytes))
}

/// `unit_size` バイトの 0 が出るまで読む。終端は含めない。
fn read_until_nul(memory: &dyn Memory, pointer: u64, unit_size: usize, max_bytes: usize) -> Option<(Vec<u8>, bool)> {
    let mut bytes = Vec::new();
    let mut address = pointer;
    let mut scanned = 0;
    loop {
        if bytes.len() >= max_bytes {
            bytes.truncate(max_bytes - max_bytes % unit_size);
            return Some((bytes, true));
        }
        let piece = piece_len(address, max_bytes - bytes.len());
        let start = bytes.len();
        bytes.resize(start + piece, 0);
        if !memory.read(address, &mut bytes[start..]) {
            bytes.truncate(start);
            return if bytes.is_empty() { None } else { Some((bytes, true)) };
        }
        address += piece as u64;
        // 先頭から unit_size ごとに区切って探す（ページ境界で文字が割れても、溜めたバイト列の上で探すので取りこぼさない）。
        while scanned + unit_size <= bytes.len() {
            if bytes[scanned..scanned + unit_size].iter().all(|&b| b == 0) {
                bytes.truncate(scanned);
                return Some((bytes, false));
            }
            scanned += unit_size;
        }
    }
}

/// 次のページ境界を越えない長さ。
fn piece_len(address: u64, remaining: usize) -> usize {
    let to_page_end = (PAGE_SIZE - address % PAGE_SIZE) as usize;
    to_page_end.min(remaining)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::BufferMemory;

    /// 試験用: 0x80 以上を Latin-1 とみなし、コードページの番号を先頭に付けて区別できるようにする。
    struct FakeDecoder;

    impl CodepageDecoder for FakeDecoder {
        fn decode(&self, bytes: &[u8], codepage: u32) -> String {
            format!(
                "cp{codepage}:{}",
                bytes.iter().map(|&b| char::from(b)).collect::<String>()
            )
        }
    }

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    fn read(memory: &BufferMemory, ptr: u64, len: Option<(i64, Unit)>, enc: Encoding) -> Option<Text> {
        read_text(memory, ptr, len, enc, &FakeDecoder, &TextOptions::default()).unwrap()
    }

    #[test]
    fn null_pointer_means_no_sql() {
        assert_eq!(read(&BufferMemory::new(), 0, None, Encoding::Ansi), None);
    }

    #[test]
    fn utf16_with_char_count() {
        let memory = BufferMemory::new().with(0x1000, utf16("SELECT 1 FROM DUAL"));
        let text = read(&memory, 0x1000, Some((6, Unit::Chars)), Encoding::Utf16).unwrap();
        assert_eq!(
            text,
            Text {
                value: "SELECT".into(),
                truncated: false
            }
        );
    }

    #[test]
    fn negative_length_is_nul_terminated() {
        let mut bytes = b"SELECT 1".to_vec();
        bytes.extend_from_slice(b"\0garbage");
        let memory = BufferMemory::new().with(0x1000, bytes);
        let text = read(&memory, 0x1000, Some((-3, Unit::Bytes)), Encoding::Ansi).unwrap();
        assert_eq!(text.value, "cp0:SELECT 1");
    }

    #[test]
    fn nul_terminator_split_across_page_boundary() {
        // UTF-16 の終端 (00 00) がページ境界の直前の奇数番地から始まっても見つける。
        let mut bytes = utf16("AB");
        bytes.extend_from_slice(&[0, 0]);
        let start = 0x2000 - 3;
        let memory = BufferMemory::new()
            .with(start, bytes[..3].to_vec())
            .with(0x2000, bytes[3..].to_vec());
        let text = read(&memory, start, None, Encoding::Utf16).unwrap();
        assert_eq!(text.value, "AB");
    }

    #[test]
    fn string_ending_at_unmapped_page_without_terminator_is_truncated() {
        // 終端がないまま、次の（読めない）ページの直前で終わる。
        let memory = BufferMemory::new().with(0x2000 - 6, b"SELECT".to_vec());
        let text = read(&memory, 0x2000 - 6, None, Encoding::Ansi).unwrap();
        assert_eq!(
            text,
            Text {
                value: "cp0:SELECT".into(),
                truncated: true
            }
        );
    }

    #[test]
    fn unreadable_start_is_an_error() {
        let result = read_text(
            &BufferMemory::new(),
            0x1000,
            None,
            Encoding::Ansi,
            &FakeDecoder,
            &TextOptions::default(),
        );
        assert_eq!(result, Err(TextError::Unreadable { address: 0x1000 }));
    }

    #[test]
    fn auto_detects_utf16_utf8_and_codepage() {
        let sjis_like = vec![b'S', b'E', b'L', 0x95, 0x5c]; // CP932 の「表」(0x95 0x5C) を含む。UTF-8 としては不正。
        let memory = BufferMemory::new()
            .with(0x1000, utf16("SELECT"))
            .with(0x2000, "SELECT 'あ'".as_bytes().to_vec())
            .with(0x3000, sjis_like.clone());
        let options = TextOptions {
            auto_codepage: 932,
            ..Default::default()
        };
        let auto = |ptr, len: usize| {
            read_text(
                &memory,
                ptr,
                Some((len as i64, Unit::Bytes)),
                Encoding::Auto,
                &FakeDecoder,
                &options,
            )
            .unwrap()
            .unwrap()
            .value
        };
        assert_eq!(auto(0x1000, 12), "SELECT");
        assert_eq!(auto(0x2000, "SELECT 'あ'".len()), "SELECT 'あ'");
        assert!(auto(0x3000, sjis_like.len()).starts_with("cp932:"));
    }

    #[test]
    fn auto_nul_terminated_utf16() {
        let mut bytes = utf16("SELECT");
        bytes.extend_from_slice(&[0, 0]);
        let memory = BufferMemory::new().with(0x1000, bytes);
        assert_eq!(read(&memory, 0x1000, None, Encoding::Auto).unwrap().value, "SELECT");
    }

    #[test]
    fn max_bytes_truncates_without_splitting_surrogate_pair() {
        let memory = BufferMemory::new().with(0x1000, utf16("A😀"));
        // "A" (2 バイト) + サロゲートの前半 (2 バイト) で切れる。
        let options = TextOptions {
            max_bytes: 4,
            ..Default::default()
        };
        let text = read_text(
            &memory,
            0x1000,
            Some((3, Unit::Chars)),
            Encoding::Utf16,
            &FakeDecoder,
            &options,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            text,
            Text {
                value: "A".into(),
                truncated: true
            }
        );
    }
}
