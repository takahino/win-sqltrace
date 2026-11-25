//! ANSI（コードページ）のバイト列を `MultiByteToWideChar` で文字列にする。

use sqltrace_core::CodepageDecoder;
use windows_sys::Win32::Globalization::MultiByteToWideChar;

/// コードページは対象と同じ機械のもの（`CP_ACP` はこの機械の ANSI コードページ）。
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsDecoder;

impl CodepageDecoder for WindowsDecoder {
    fn decode(&self, bytes: &[u8], codepage: u32) -> String {
        if bytes.is_empty() {
            return String::new();
        }
        let Ok(length) = i32::try_from(bytes.len()) else {
            return String::from_utf8_lossy(bytes).into_owned();
        };
        // 1 バイトは高々 1 つの UTF-16 の単位になる。
        let mut wide = vec![0u16; bytes.len()];
        // SAFETY: 入力と出力のバッファーは、渡した長さのとおりに確保してある。
        let written = unsafe { MultiByteToWideChar(codepage, 0, bytes.as_ptr(), length, wide.as_mut_ptr(), length) };
        if written <= 0 {
            // 知らないコードページなど。読めないより、崩れても見えるほうが調べやすい。
            return String::from_utf8_lossy(bytes).into_owned();
        }
        String::from_utf16_lossy(&wide[..written as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_shift_jis_with_backslash_like_trail_byte() {
        // 「表」は CP932 で 0x95 0x5C（2 バイト目が '\' と同じ値）。
        assert_eq!(WindowsDecoder.decode(&[0x95, 0x5C, b'1'], 932), "表1");
    }

    #[test]
    fn unknown_codepage_falls_back_to_lossy_utf8() {
        assert_eq!(WindowsDecoder.decode(b"SELECT", 12345), "SELECT");
    }
}
