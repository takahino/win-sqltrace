//! 呼び出し規約: 関数の入口で止めたときのレジスタとスタックから、n 番目の引数と戻り先を求める。
//!
//! アーキテクチャの違いはここに閉じる。デバッグ層はスレッドのコンテキストを [`Registers`] に写すだけでよい。

use crate::memory::{read_u32, read_u64, Memory};
use crate::probe::ScalarArg;
use crate::Arch;

/// 止めたスレッドのレジスタのうち、引数を読むのに要るもの。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Registers {
    pub ip: u64,
    pub sp: u64,
    /// Win64 で引数を渡す rcx, rdx, r8, r9。x86 では使わない。
    pub args: [u64; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Abi {
    /// x86 の stdcall と cdecl。入口では [esp] が戻り先、[esp+4+4n] が n 番目の引数で、どちらも同じ。
    X86Stack,
    /// x64 の Windows 規約。最初の 4 つはレジスタ、5 つ目からは戻り先とシャドウ領域（32 バイト）の後ろ。
    Win64,
}

impl Abi {
    pub fn for_arch(arch: Arch) -> Abi {
        match arch {
            Arch::X86 => Abi::X86Stack,
            Arch::X64 => Abi::Win64,
        }
    }

    /// n 番目（0 始まり）の引数の生の値。関数の最初の命令を実行する前に止めたときだけ正しい。
    /// x64 で 32 ビット以下の引数は上位が不定なので、幅は [`crate::Width::normalize`] で揃える。
    pub fn arg(self, regs: &Registers, memory: &dyn Memory, index: usize) -> Option<u64> {
        let slot = index as u64;
        match self {
            Abi::X86Stack => read_u32(memory, regs.sp.checked_add(4 + 4 * slot)?).map(u64::from),
            Abi::Win64 if index < 4 => Some(regs.args[index]),
            Abi::Win64 => read_u64(memory, regs.sp.checked_add(8 + 8 * slot)?),
        }
    }

    /// 入口で止めたときの戻り先（呼び出し元の次の命令）。
    pub fn return_address(self, regs: &Registers, memory: &dyn Memory) -> Option<u64> {
        match self {
            Abi::X86Stack => read_u32(memory, regs.sp).map(u64::from),
            Abi::Win64 => read_u64(memory, regs.sp),
        }
    }
}

/// API の入口で止めた 1 回の呼び出し。引数の解釈に要るものをまとめる。
#[derive(Clone, Copy)]
pub struct CallSite<'a> {
    pub arch: Arch,
    pub regs: Registers,
    pub memory: &'a dyn Memory,
}

impl CallSite<'_> {
    pub fn arg(&self, index: usize) -> Option<u64> {
        Abi::for_arch(self.arch).arg(&self.regs, self.memory, index)
    }

    /// 幅と符号を揃えた引数の値。
    pub fn scalar(&self, arg: ScalarArg) -> Option<i64> {
        self.arg(usize::from(arg.index))
            .map(|raw| arg.width.normalize(raw, self.arch))
    }

    pub fn return_address(&self) -> Option<u64> {
        Abi::for_arch(self.arch).return_address(&self.regs, self.memory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::BufferMemory;
    use crate::Width;

    fn stack(base: u64, slots: &[u64], slot_size: usize) -> BufferMemory {
        let bytes: Vec<u8> = slots
            .iter()
            .flat_map(|s| s.to_le_bytes()[..slot_size].to_vec())
            .collect();
        BufferMemory::new().with(base, bytes)
    }

    #[test]
    fn x86_reads_arguments_after_return_address() {
        let memory = stack(0x1000, &[0xdead, 11, 22, 33], 4);
        let regs = Registers {
            sp: 0x1000,
            ..Default::default()
        };
        assert_eq!(Abi::X86Stack.return_address(&regs, &memory), Some(0xdead));
        assert_eq!(Abi::X86Stack.arg(&regs, &memory, 0), Some(11));
        assert_eq!(Abi::X86Stack.arg(&regs, &memory, 2), Some(33));
    }

    #[test]
    fn win64_reads_registers_then_stack_past_shadow_space() {
        // [rsp] 戻り先、[rsp+8..40] シャドウ領域、[rsp+40] 5 つ目の引数。
        let memory = stack(0x2000, &[0xbeef, 0, 0, 0, 0, 55], 8);
        let regs = Registers {
            sp: 0x2000,
            args: [1, 2, 3, 4],
            ..Default::default()
        };
        assert_eq!(Abi::Win64.return_address(&regs, &memory), Some(0xbeef));
        assert_eq!(Abi::Win64.arg(&regs, &memory, 3), Some(4));
        assert_eq!(Abi::Win64.arg(&regs, &memory, 4), Some(55));
    }

    #[test]
    fn x64_32bit_argument_ignores_garbage_upper_half() {
        let memory = BufferMemory::new();
        // r8 の上位 32 ビットにゴミ、下位は SQL_NTS (-3)。
        let regs = Registers {
            args: [0, 0, 0xffff_1234_ffff_fffd, 0],
            ..Default::default()
        };
        let site = CallSite {
            arch: Arch::X64,
            regs,
            memory: &memory,
        };
        assert_eq!(
            site.scalar(ScalarArg {
                index: 2,
                width: Width::I32
            }),
            Some(-3)
        );
        assert_eq!(
            site.scalar(ScalarArg {
                index: 2,
                width: Width::U32
            }),
            Some(0xffff_fffd)
        );
    }
}
