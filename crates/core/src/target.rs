//! トレースする対象: 動いているプロセスにアタッチするか、デバッガとして起動するか。

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetSpec {
    Attach { pid: u32 },
    Launch(LaunchSpec),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchSpec {
    pub exe: PathBuf,
    /// exe の後ろに付けるコマンドラインをそのまま（分割も引用し直しもしない。Windows の引数の解釈は exe ごとに違うため）。
    pub arguments: String,
    /// `None` は exe のあるフォルダ（VC6 のアプリは INI や DLL を作業フォルダから探すことが多い）。
    pub working_dir: Option<PathBuf>,
    /// 制御側の環境に足す・上書きする変数（`ORACLE_HOME`、`NLS_LANG` など）。
    pub environment: Vec<(String, String)>,
    /// 新しいコンソールで起動する。同じコンソールだと出力が混ざり、Ctrl+C が対象にも届く。
    pub new_console: bool,
}

impl LaunchSpec {
    pub fn new(exe: impl Into<PathBuf>) -> Self {
        Self {
            exe: exe.into(),
            arguments: String::new(),
            working_dir: None,
            environment: Vec::new(),
            new_console: true,
        }
    }
}
