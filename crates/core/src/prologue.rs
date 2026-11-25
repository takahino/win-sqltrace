//! 関数の入口の最初の 1 命令を、実行せずにレジスタとスタックの書き換えで済ませる（エミュレーション）。
//!
//! ブレークポイントで止めた後に続行するには、ふつうは元のバイトを戻して 1 命令だけステップ実行し、もう一度
//! int3 を書く。その間ほかのスレッドを止めておく必要があり、遅いうえに取りこぼしの元になる。Windows の DLL の
//! 入口はほとんどが決まった命令（x86 の `mov edi, edi` や `push ebp`、x64 の `mov [rsp+8], rbx` など）で
//! 始まるので、それらは int3 を書いたまま、命令の効果だけを再現して次の命令から続ける。
//!
//! レジスタの番号は x86 の符号化どおり（0 = ax, 1 = cx, 2 = dx, 3 = bx, 4 = sp, 5 = bp, 6 = si, 7 = di、
//! x64 の 8〜15 = r8〜r15）。

use crate::Arch;

/// スタックポインターの番号。
const SP: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Emulation {
    /// 何もしない命令（`mov edi, edi`、`nop`）。
    Skip { length: u8 },
    /// `push reg`。
    Push { register: u8, length: u8 },
    /// `mov [sp + displacement], reg`（x64 の 64 ビットの保存）。
    StoreToStack { register: u8, displacement: i8, length: u8 },
    /// `mov reg, sp`。
    CopyStackPointer { register: u8, length: u8 },
}

/// エミュレーションが読み書きするスレッドの状態。デバッグ層がスレッドのコンテキストで実装する。
pub trait Cpu {
    fn register(&self, index: u8) -> u64;
    fn set_register(&mut self, index: u8, value: u64);
    fn ip(&self) -> u64;
    fn set_ip(&mut self, value: u64);
}

impl Emulation {
    pub fn length(self) -> u8 {
        match self {
            Emulation::Skip { length }
            | Emulation::Push { length, .. }
            | Emulation::StoreToStack { length, .. }
            | Emulation::CopyStackPointer { length, .. } => length,
        }
    }

    /// 命令の効果を `cpu` に施す。スタックへの書き込みは `write` で行い、失敗したら `cpu` を変えずに `false`
    /// を返す（呼び出し側はステップ実行に切り替える）。
    pub fn apply(self, arch: Arch, cpu: &mut dyn Cpu, write: &mut dyn FnMut(u64, &[u8]) -> bool) -> bool {
        let width = arch.pointer_size();
        let sp = cpu.register(SP);
        match self {
            Emulation::Skip { .. } => {}
            Emulation::Push { register, .. } => {
                let new_sp = sp.wrapping_sub(width as u64);
                if !write(new_sp, &cpu.register(register).to_le_bytes()[..width]) {
                    return false;
                }
                cpu.set_register(SP, new_sp);
            }
            Emulation::StoreToStack {
                register, displacement, ..
            } => {
                let address = sp.wrapping_add_signed(i64::from(displacement));
                if !write(address, &cpu.register(register).to_le_bytes()[..width]) {
                    return false;
                }
            }
            Emulation::CopyStackPointer { register, .. } => cpu.set_register(register, sp),
        }
        cpu.set_ip(cpu.ip() + u64::from(self.length()));
        true
    }
}

/// 入口のバイト列（先頭から数バイト）を読み、エミュレーションできる命令なら返す。
pub fn decode(arch: Arch, bytes: &[u8]) -> Option<Emulation> {
    match arch {
        Arch::X86 => decode_x86(bytes),
        Arch::X64 => decode_x64(bytes),
    }
}

fn modrm(byte: u8) -> (u8, u8, u8) {
    (byte >> 6, (byte >> 3) & 7, byte & 7)
}

/// x86 と x64 で共通の、REX を持たない形。
fn decode_common(bytes: &[u8]) -> Option<Emulation> {
    match *bytes {
        [0x90, ..] => Some(Emulation::Skip { length: 1 }),
        [0x66, 0x90, ..] => Some(Emulation::Skip { length: 2 }),
        [op @ 0x50..=0x57, ..] if op - 0x50 != SP => Some(Emulation::Push {
            register: op - 0x50,
            length: 1,
        }),
        _ => None,
    }
}

fn decode_x86(bytes: &[u8]) -> Option<Emulation> {
    if let Some(e) = decode_common(bytes) {
        return Some(e);
    }
    match *bytes {
        // mov edi, edi（ホットパッチ用の 2 バイトの nop）。
        [0x8B, 0xFF, ..] => Some(Emulation::Skip { length: 2 }),
        [opcode @ (0x8B | 0x89), m, ..] => copy_stack_pointer(opcode, m, 0, 2),
        _ => None,
    }
}

fn decode_x64(bytes: &[u8]) -> Option<Emulation> {
    if let Some(e) = decode_common(bytes) {
        return Some(e);
    }
    let [rex @ 0x40..=0x4F, rest @ ..] = bytes else {
        return None;
    };
    let (w, r, x, b) = (rex & 8 != 0, (rex >> 2) & 1, (rex >> 1) & 1, rex & 1);
    match *rest {
        // push r64（REX.B で r8〜r15。REX.W は意味を持たない）。
        [op @ 0x50..=0x57, ..] if !(b == 0 && op - 0x50 == SP) => Some(Emulation::Push {
            register: (op - 0x50) + 8 * b,
            length: 2,
        }),
        // ここから先は 64 ビットの mov で、sp を土台にする（REX.X と、sp 側の REX.B は 0）。
        _ if !w || x != 0 => None,
        // mov [rsp + disp8], reg: 89 /r（mod = 01、rm = 100 で SIB = 0x24）。
        [0x89, m, 0x24, displacement, ..] if b == 0 => match modrm(m) {
            (1, reg, 4) if !(r == 0 && reg == SP) => Some(Emulation::StoreToStack {
                register: reg + 8 * r,
                displacement: displacement as i8,
                length: 5,
            }),
            _ => None,
        },
        [opcode @ (0x8B | 0x89), m, ..] => {
            // 8B は reg ← rm（rm 側が sp なので REX.B = 0）、89 は rm ← reg（reg 側が sp なので REX.R = 0）。
            let (dest_extension, source_extension) = if opcode == 0x8B { (r, b) } else { (b, r) };
            if source_extension != 0 {
                return None;
            }
            copy_stack_pointer(opcode, m, dest_extension, 3)
        }
        _ => None,
    }
}

/// `mov reg, sp`。8B では modrm の rm が sp で reg が行き先、89 ではその逆。
fn copy_stack_pointer(opcode: u8, m: u8, dest_extension: u8, length: u8) -> Option<Emulation> {
    let (mode, reg, rm) = modrm(m);
    let (dest, source) = if opcode == 0x8B { (reg, rm) } else { (rm, reg) };
    let register = dest + 8 * dest_extension;
    (mode == 3 && source == SP && register != SP).then_some(Emulation::CopyStackPointer { register, length })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeCpu {
        registers: [u64; 16],
        ip: u64,
    }

    impl Cpu for FakeCpu {
        fn register(&self, index: u8) -> u64 {
            self.registers[usize::from(index)]
        }
        fn set_register(&mut self, index: u8, value: u64) {
            self.registers[usize::from(index)] = value;
        }
        fn ip(&self) -> u64 {
            self.ip
        }
        fn set_ip(&mut self, value: u64) {
            self.ip = value;
        }
    }

    #[test]
    fn decodes_x86_entries() {
        assert_eq!(
            decode(Arch::X86, &[0x8B, 0xFF, 0x55]),
            Some(Emulation::Skip { length: 2 })
        );
        assert_eq!(
            decode(Arch::X86, &[0x55, 0x8B, 0xEC]),
            Some(Emulation::Push { register: 5, length: 1 })
        );
        // mov ebp, esp
        assert_eq!(
            decode(Arch::X86, &[0x8B, 0xEC]),
            Some(Emulation::CopyStackPointer { register: 5, length: 2 })
        );
        // push esp は値が特殊なので扱わない。x86 の 0x40〜0x4F は inc/dec で REX ではない。
        assert_eq!(decode(Arch::X86, &[0x54]), None);
        assert_eq!(decode(Arch::X86, &[0x40, 0x53]), None);
        // sub esp, imm8 はフラグを変えるので扱わない。
        assert_eq!(decode(Arch::X86, &[0x83, 0xEC, 0x10]), None);
    }

    #[test]
    fn decodes_x64_entries() {
        // mov [rsp+8], rbx
        assert_eq!(
            decode(Arch::X64, &[0x48, 0x89, 0x5C, 0x24, 0x08]),
            Some(Emulation::StoreToStack {
                register: 3,
                displacement: 8,
                length: 5
            })
        );
        // mov [rsp+0x18], r8
        assert_eq!(
            decode(Arch::X64, &[0x4C, 0x89, 0x44, 0x24, 0x18]),
            Some(Emulation::StoreToStack {
                register: 8,
                displacement: 0x18,
                length: 5
            })
        );
        // push rbx（REX 付き）、push r12
        assert_eq!(
            decode(Arch::X64, &[0x40, 0x53]),
            Some(Emulation::Push { register: 3, length: 2 })
        );
        assert_eq!(
            decode(Arch::X64, &[0x41, 0x54]),
            Some(Emulation::Push {
                register: 12,
                length: 2
            })
        );
        // mov r11, rsp / mov rax, rsp / mov rbp, rsp（89 形）
        assert_eq!(
            decode(Arch::X64, &[0x4C, 0x8B, 0xDC]),
            Some(Emulation::CopyStackPointer {
                register: 11,
                length: 3
            })
        );
        assert_eq!(
            decode(Arch::X64, &[0x48, 0x8B, 0xC4]),
            Some(Emulation::CopyStackPointer { register: 0, length: 3 })
        );
        assert_eq!(
            decode(Arch::X64, &[0x48, 0x89, 0xE5]),
            Some(Emulation::CopyStackPointer { register: 5, length: 3 })
        );
        // 32 ビットの mov（REX.W なし）と sub rsp は扱わない。
        assert_eq!(decode(Arch::X64, &[0x40, 0x89, 0x5C, 0x24, 0x08]), None);
        assert_eq!(decode(Arch::X64, &[0x48, 0x83, 0xEC, 0x28]), None);
        // mov [r12+8], rbx（土台が rsp ではない）
        assert_eq!(decode(Arch::X64, &[0x49, 0x89, 0x5C, 0x24, 0x08]), None);
    }

    #[test]
    fn push_writes_below_stack_and_moves_sp() {
        let mut cpu = FakeCpu {
            ip: 0x1000,
            ..Default::default()
        };
        cpu.registers[4] = 0x8000;
        cpu.registers[5] = 0x1122_3344_5566_7788;
        let mut written = Vec::new();
        let ok = Emulation::Push { register: 5, length: 1 }.apply(Arch::X86, &mut cpu, &mut |a, b| {
            written.push((a, b.to_vec()));
            true
        });
        assert!(ok);
        assert_eq!(written, [(0x7ffc, vec![0x88, 0x77, 0x66, 0x55])]);
        assert_eq!(cpu.registers[4], 0x7ffc);
        assert_eq!(cpu.ip, 0x1001);
    }

    #[test]
    fn failed_write_leaves_cpu_untouched() {
        let mut cpu = FakeCpu {
            ip: 0x1000,
            ..Default::default()
        };
        cpu.registers[4] = 0x8000;
        let ok = Emulation::StoreToStack {
            register: 3,
            displacement: 8,
            length: 5,
        }
        .apply(Arch::X64, &mut cpu, &mut |_, _| false);
        assert!(!ok);
        assert_eq!((cpu.ip, cpu.registers[4]), (0x1000, 0x8000));
    }

    #[test]
    fn store_and_copy_use_stack_pointer() {
        let mut cpu = FakeCpu {
            ip: 0x1000,
            ..Default::default()
        };
        cpu.registers[4] = 0x8000;
        cpu.registers[3] = 0xAB;
        let mut at = 0;
        Emulation::StoreToStack {
            register: 3,
            displacement: 8,
            length: 5,
        }
        .apply(Arch::X64, &mut cpu, &mut |a, b| {
            at = a;
            b.len() == 8
        });
        assert_eq!((at, cpu.ip), (0x8008, 0x1005));
        Emulation::CopyStackPointer {
            register: 11,
            length: 3,
        }
        .apply(Arch::X64, &mut cpu, &mut |_, _| true);
        assert_eq!(cpu.registers[11], 0x8000);
    }
}
