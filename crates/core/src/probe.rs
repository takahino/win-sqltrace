//! 何を捕まえるかの定義（データ）。中身は `catalog` crate が持つ。対象を足すときはそちらに 1 件足すだけでよい。

use crate::Arch;

/// API の層（ODBC、OCI など）。表示と絞り込み（`--probe odbc`）、重複の除去に使う。
#[derive(Debug, PartialEq, Eq)]
pub struct Layer {
    /// 絞り込みで指定する名前（小文字）。
    pub id: &'static str,
    pub name: &'static str,
    /// この層に属するがフックはしないモジュール（ODBC ドライバなど）。ここから他の層の API を呼んだ分は、
    /// この層で既に捕まえた呼び出しの内側なので捨てる（例: sqora32.dll → oci.dll）。
    pub related_modules: &'static [&'static str],
}

/// その入口が SQL に対して何をするか。相関器（ハンドル → SQL）がこれで振る舞いを変える。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// SQL を渡してその場で実行する（`SQLExecDirect`）。
    ExecDirect,
    /// SQL をハンドルに準備する（`SQLPrepare`、`OCIStmtPrepare`）。
    Prepare,
    /// ハンドルの SQL バッファーに足す（DB-Library の `dbcmd`）。実行の後に足すと新しい文になる。
    Append,
    /// ハンドルに準備済みの SQL を実行する（`SQLExecute`、`OCIStmtExecute`）。
    Execute,
}

/// 引数の幅と符号。x64 では 32 ビット以下の引数のレジスタの上位が不定なので、必ずこれで切り揃える。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    I16,
    I32,
    U32,
    Ptr,
}

impl Width {
    pub fn normalize(self, raw: u64, arch: Arch) -> i64 {
        match self {
            Width::I16 => i64::from(raw as u16 as i16),
            Width::I32 => i64::from(raw as u32 as i32),
            Width::U32 => i64::from(raw as u32),
            Width::Ptr => match arch {
                Arch::X86 => i64::from(raw as u32),
                // ユーザー空間のアドレスは 2^63 未満なので、ビットのまま i64 にしてよい。
                Arch::X64 => raw as i64,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalarArg {
    /// 0 始まりの引数の位置。
    pub index: u8,
    pub width: Width,
}

/// 長さ引数の単位。ODBC の W 系は文字数、A 系と OCI はバイト数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Chars,
    Bytes,
}

/// SQL の長さを渡す引数。符号付きの幅で負の値なら NUL 終端とみなす（ODBC の SQL_NTS = -3、OCI7 の -1）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LengthArg {
    pub arg: ScalarArg,
    pub unit: Unit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// 対象のコードページ（既定は ACP）。ODBC の A 系。
    Ansi,
    Utf16,
    /// UTF-16 か、UTF-8 か、コードページかを中身から推定する。OCI はクライアントの文字集合（NLS_LANG）に従うため。
    Auto,
}

/// SQL の文字列を渡す引数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextArg {
    /// 文字列のポインターの引数の位置。NULL なら SQL なしとみなす（`OCIStmtPrepare2` のキー検索など）。
    pub pointer: u8,
    /// `None` なら常に NUL 終端。
    pub length: Option<LengthArg>,
    pub encoding: Encoding,
}

/// 引数のビットが立っていたら記録しない（`OCIStmtExecute` の `OCI_DESCRIBE_ONLY` など、実行ではない呼び出し）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SkipIf {
    pub arg: ScalarArg,
    pub mask: u64,
}

/// 捕まえる入口 1 つ。
#[derive(Debug)]
pub struct ProbeDef {
    pub layer: &'static Layer,
    /// 関数を探すモジュール名のパターン（大文字小文字を区別しない。`*` が使える）。
    pub modules: &'static [&'static str],
    pub function: &'static str,
    pub role: Role,
    pub sql: Option<TextArg>,
    /// 文のハンドル（Prepare と Execute を対応付ける鍵）。
    pub handle: Option<ScalarArg>,
    pub skip_if: Option<SkipIf>,
}

/// 定義の一覧。`catalog` crate が static で持つ。
#[derive(Debug)]
pub struct Catalog {
    pub layers: &'static [&'static Layer],
    pub probes: &'static [ProbeDef],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_truncates_and_sign_extends() {
        assert_eq!(Width::I16.normalize(0xdead_ffff, Arch::X64), -1);
        assert_eq!(Width::I32.normalize(0x1_ffff_fffd, Arch::X64), -3);
        assert_eq!(Width::U32.normalize(0x1_ffff_fffd, Arch::X64), 0xffff_fffd);
        assert_eq!(Width::Ptr.normalize(0x1234_5678_9abc, Arch::X86), 0x5678_9abc);
        assert_eq!(Width::Ptr.normalize(0x1234_5678_9abc, Arch::X64), 0x1234_5678_9abc);
    }
}
