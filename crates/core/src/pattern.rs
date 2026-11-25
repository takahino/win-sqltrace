//! モジュール名のパターン照合（大文字小文字を区別しない。`*` は 0 文字以上）。

/// `name` はファイル名だけ（ディレクトリを含めない）を渡す。
pub fn matches(pattern: &str, name: &str) -> bool {
    let pattern = pattern.as_bytes();
    let name = name.as_bytes();
    let (mut p, mut n) = (0, 0);
    // 直前の `*` の位置と、そのときの name の位置。合わなくなったら `*` に 1 文字多く食わせてやり直す。
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pattern.len() && pattern[p] == b'*' {
            star = Some((p, n));
            p += 1;
        } else if p < pattern.len() && pattern[p].eq_ignore_ascii_case(&name[n]) {
            p += 1;
            n += 1;
        } else if let Some((sp, sn)) = star {
            p = sp + 1;
            n = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == b'*')
}

/// パスからファイル名だけを取り出す（`\` と `/` の両方を区切りとみなす）。
pub fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_case_insensitively_with_wildcards() {
        assert!(matches("odbc32.dll", "ODBC32.DLL"));
        assert!(matches("sqora*.dll", "SQORA32.dll"));
        assert!(matches("*", "anything"));
        assert!(matches("ora*sql*.dll", "orasql11.dll"));
        assert!(!matches("oci.dll", "ociw32.dll"));
        assert!(!matches("sqora*.dll", "sqora32.exe"));
    }

    #[test]
    fn file_name_strips_directories() {
        assert_eq!(file_name(r"\\?\C:\Windows\SysWOW64\odbc32.dll"), "odbc32.dll");
        assert_eq!(file_name("oci.dll"), "oci.dll");
    }
}
