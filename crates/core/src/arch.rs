//! 対象プロセスのアーキテクチャ。

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Arch {
    X86,
    X64,
}

impl Arch {
    pub fn pointer_size(self) -> usize {
        match self {
            Arch::X86 => 4,
            Arch::X64 => 8,
        }
    }

    /// PE ヘッダーの `Machine` から。対応していない値は `None`。
    pub fn from_pe_machine(machine: u16) -> Option<Arch> {
        match machine {
            0x014c => Some(Arch::X86),
            0x8664 => Some(Arch::X64),
            _ => None,
        }
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Arch::X86 => "x86",
            Arch::X64 => "x64",
        })
    }
}
