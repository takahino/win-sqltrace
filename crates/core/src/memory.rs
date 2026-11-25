//! 対象プロセスのメモリを読む口。実装はデバッグ層（`ReadProcessMemory`）と試験用の [`BufferMemory`]。

use crate::Arch;

pub trait Memory {
    /// `address` から `buf.len()` バイトを読む。一部でも読めなければ `false`（`buf` の中身は不定）。
    fn read(&self, address: u64, buf: &mut [u8]) -> bool;
}

pub fn read_u16(memory: &dyn Memory, address: u64) -> Option<u16> {
    let mut b = [0u8; 2];
    memory.read(address, &mut b).then(|| u16::from_le_bytes(b))
}

pub fn read_u32(memory: &dyn Memory, address: u64) -> Option<u32> {
    let mut b = [0u8; 4];
    memory.read(address, &mut b).then(|| u32::from_le_bytes(b))
}

pub fn read_u64(memory: &dyn Memory, address: u64) -> Option<u64> {
    let mut b = [0u8; 8];
    memory.read(address, &mut b).then(|| u64::from_le_bytes(b))
}

pub fn read_pointer(memory: &dyn Memory, address: u64, arch: Arch) -> Option<u64> {
    match arch {
        Arch::X86 => read_u32(memory, address).map(u64::from),
        Arch::X64 => read_u64(memory, address),
    }
}

/// 試験で対象のメモリの代わりに使う。実際のメモリと同じく、読めるかどうかはページ（4 KiB）単位で決まる:
/// 置いた塊を含むページ全体が読め、塊の外のバイトは 0。どの塊も含まないページは読めない。
#[derive(Clone, Debug, Default)]
pub struct BufferMemory {
    regions: Vec<(u64, Vec<u8>)>,
}

const PAGE_SIZE: u64 = 4096;

impl BufferMemory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, base: u64, bytes: impl Into<Vec<u8>>) -> Self {
        self.regions.push((base, bytes.into()));
        self
    }

    fn page_is_mapped(&self, page: u64) -> bool {
        self.regions.iter().any(|(base, bytes)| {
            let first = base / PAGE_SIZE;
            let last = (base + bytes.len().max(1) as u64 - 1) / PAGE_SIZE;
            (first..=last).contains(&page)
        })
    }

    fn byte_at(&self, address: u64) -> u8 {
        self.regions
            .iter()
            .find_map(|(base, bytes)| address.checked_sub(*base).and_then(|i| bytes.get(i as usize)).copied())
            .unwrap_or(0)
    }
}

impl Memory for BufferMemory {
    fn read(&self, address: u64, buf: &mut [u8]) -> bool {
        let Some(end) = address.checked_add(buf.len() as u64) else {
            return false;
        };
        if buf.is_empty() {
            return true;
        }
        if !(address / PAGE_SIZE..=(end - 1) / PAGE_SIZE).all(|page| self.page_is_mapped(page)) {
            return false;
        }
        for (offset, byte) in buf.iter_mut().enumerate() {
            *byte = self.byte_at(address + offset as u64);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readable_by_page_like_real_memory() {
        let memory = BufferMemory::new().with(0x1ffc, vec![1, 2, 3, 4]);
        assert_eq!(read_u32(&memory, 0x1ffc), Some(0x0403_0201));
        // 同じページの塊の外は 0 として読める。
        assert_eq!(read_u32(&memory, 0x1000), Some(0));
        // 塊を含まない次のページにかかる読み取りは失敗する。
        assert_eq!(read_u32(&memory, 0x1ffe), None);
    }
}
