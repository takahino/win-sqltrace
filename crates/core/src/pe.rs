//! 対象のメモリに読み込まれた PE（DLL）のヘッダーとエクスポート表を読む。
//!
//! ファイルではなくメモリ上のイメージを読むので、RVA はそのままベースからのオフセットとして使える。

use std::fmt;

use crate::memory::{read_u16, read_u32, Memory};
use crate::Arch;

/// エクスポートの数と表の大きさの上限。壊れた（または読み違えた）ヘッダーで巨大な確保をしないため。
const MAX_EXPORTS: u32 = 65_536;
const MAX_DIRECTORY_BYTES: u32 = 16 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportTarget {
    /// モジュール内のコード（RVA）。
    Code(u32),
    /// 他の DLL への転送（`NTDLL.RtlAllocateHeap` のような文字列）。ここにはブレークポイントを置けない。
    Forwarder(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Export {
    pub name: String,
    pub target: ExportTarget,
}

/// 読み込まれたモジュールの、sqlTrace が使う情報。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleImage {
    /// `None` は対応していないアーキテクチャ（ARM64 など）。
    pub arch: Option<Arch>,
    pub machine: u16,
    pub size_of_image: u32,
    /// エクスポート表にある自分の名前（`ODBC32.dll` など）。ファイル名が取れないときの代わり。
    pub export_name: Option<String>,
    pub exports: Vec<Export>,
}

impl ModuleImage {
    /// 名前で探す（大文字小文字を区別する。エクスポート名は区別されるため）。
    pub fn export(&self, name: &str) -> Option<&Export> {
        self.exports.iter().find(|e| e.name == name)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeError {
    Unreadable { address: u64 },
    NotPe { base: u64 },
    Malformed { base: u64, what: &'static str },
}

impl fmt::Display for PeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PeError::Unreadable { address } => write!(f, "PE ヘッダー (0x{address:x}) を読めませんでした"),
            PeError::NotPe { base } => write!(f, "0x{base:x} は PE イメージではありません"),
            PeError::Malformed { base, what } => write!(f, "0x{base:x} の PE の{what}が壊れています"),
        }
    }
}

/// PE のヘッダーのうち、モジュールの範囲とアーキテクチャを決めるもの。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageHeaders {
    /// `None` は対応していないアーキテクチャ（ARM64 など）。
    pub arch: Option<Arch>,
    pub machine: u16,
    pub size_of_image: u32,
    /// エクスポート表の RVA と大きさ。
    export_directory: Option<(u32, u32)>,
}

/// `base` に読み込まれたモジュールのヘッダーだけを読む。デバッグ層がすべての DLL の範囲を知るのに使う
/// （エクスポート表は読まないので軽い）。
pub fn read_headers(memory: &dyn Memory, base: u64) -> Result<ImageHeaders, PeError> {
    let u16_at = |offset: u64| read_u16(memory, base + offset).ok_or(PeError::Unreadable { address: base + offset });
    let u32_at = |offset: u64| read_u32(memory, base + offset).ok_or(PeError::Unreadable { address: base + offset });

    if u16_at(0)? != 0x5a4d {
        return Err(PeError::NotPe { base });
    }
    let nt = u64::from(u32_at(0x3c)?);
    if u32_at(nt)? != 0x0000_4550 {
        return Err(PeError::NotPe { base });
    }
    let machine = u16_at(nt + 4)?;
    let optional = nt + 24;
    // 32 ビット (PE32) と 64 ビット (PE32+) でデータディレクトリの位置が違う。
    let (count_offset, directories) = match u16_at(optional)? {
        0x10b => (92, 96),
        0x20b => (108, 112),
        _ => {
            return Err(PeError::Malformed {
                base,
                what: "オプションヘッダー",
            })
        }
    };
    let size_of_image = u32_at(optional + 56)?;
    let directory_count = u32_at(optional + count_offset)?;
    let export_directory = if directory_count == 0 {
        None
    } else {
        let rva = u32_at(optional + directories)?;
        let size = u32_at(optional + directories + 4)?;
        (rva != 0 && size != 0).then_some((rva, size))
    };
    Ok(ImageHeaders {
        arch: Arch::from_pe_machine(machine),
        machine,
        size_of_image,
        export_directory,
    })
}

/// `base` に読み込まれたモジュールを読む。エクスポート表がなければ `exports` は空。
pub fn read_module(memory: &dyn Memory, base: u64) -> Result<ModuleImage, PeError> {
    let headers = read_headers(memory, base)?;
    let mut image = ModuleImage {
        arch: headers.arch,
        machine: headers.machine,
        size_of_image: headers.size_of_image,
        export_name: None,
        exports: Vec::new(),
    };
    let Some((export_rva, export_size)) = headers.export_directory else {
        return Ok(image);
    };
    let table = ExportDirectory::read(memory, base, export_rva, export_size)?;
    image.export_name = table.name;
    image.exports = table.exports;
    Ok(image)
}

struct ExportDirectory {
    name: Option<String>,
    exports: Vec<Export>,
}

impl ExportDirectory {
    fn read(memory: &dyn Memory, base: u64, rva: u32, size: u32) -> Result<Self, PeError> {
        if !(40..=MAX_DIRECTORY_BYTES).contains(&size) {
            return Err(PeError::Malformed {
                base,
                what: "エクスポート表",
            });
        }
        // 表と名前の文字列はふつう同じ範囲にあるので、まとめて 1 回で読む（名前ごとに対象を読むと遅い）。
        let mut blob = vec![0u8; size as usize];
        if !memory.read(base + u64::from(rva), &mut blob) {
            return Err(PeError::Unreadable {
                address: base + u64::from(rva),
            });
        }
        let view = Blob {
            memory,
            base,
            rva,
            bytes: &blob,
        };
        let malformed = PeError::Malformed {
            base,
            what: "エクスポート表",
        };

        let name_rva = view.u32(rva + 12).ok_or(malformed.clone())?;
        let function_count = view.u32(rva + 20).ok_or(malformed.clone())?;
        let name_count = view.u32(rva + 24).ok_or(malformed.clone())?;
        let functions = view.u32(rva + 28).ok_or(malformed.clone())?;
        let names = view.u32(rva + 32).ok_or(malformed.clone())?;
        let ordinals = view.u32(rva + 36).ok_or(malformed.clone())?;
        if function_count > MAX_EXPORTS || name_count > MAX_EXPORTS {
            return Err(malformed);
        }

        let mut exports = Vec::with_capacity(name_count as usize);
        for i in 0..name_count {
            let (Some(name_ptr), Some(ordinal)) = (view.u32(names + 4 * i), view.u16(ordinals + 2 * i)) else {
                continue;
            };
            if u32::from(ordinal) >= function_count {
                continue;
            }
            let (Some(name), Some(target_rva)) = (view.c_str(name_ptr), view.u32(functions + 4 * u32::from(ordinal)))
            else {
                continue;
            };
            // RVA がエクスポート表の範囲を指していれば、コードではなく転送先の文字列。
            let target = if (rva..rva + size).contains(&target_rva) {
                ExportTarget::Forwarder(view.c_str(target_rva).unwrap_or_default())
            } else {
                ExportTarget::Code(target_rva)
            };
            exports.push(Export { name, target });
        }
        Ok(Self {
            name: view.c_str(name_rva),
            exports,
        })
    }
}

/// まとめて読んだエクスポート表。範囲外を指す RVA だけ対象のメモリから読む。
struct Blob<'a> {
    memory: &'a dyn Memory,
    base: u64,
    rva: u32,
    bytes: &'a [u8],
}

impl Blob<'_> {
    fn slice(&self, rva: u32, len: usize) -> Option<&[u8]> {
        let start = rva.checked_sub(self.rva)? as usize;
        self.bytes.get(start..start.checked_add(len)?)
    }

    fn u16(&self, rva: u32) -> Option<u16> {
        match self.slice(rva, 2) {
            Some(b) => Some(u16::from_le_bytes([b[0], b[1]])),
            None => read_u16(self.memory, self.base + u64::from(rva)),
        }
    }

    fn u32(&self, rva: u32) -> Option<u32> {
        match self.slice(rva, 4) {
            Some(b) => Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            None => read_u32(self.memory, self.base + u64::from(rva)),
        }
    }

    /// NUL 終端の ASCII 文字列（エクスポート名は 256 バイトまでとする）。
    fn c_str(&self, rva: u32) -> Option<String> {
        const MAX: usize = 256;
        let start = rva.checked_sub(self.rva).map(|s| s as usize);
        let bytes: Vec<u8> = match start.and_then(|s| self.bytes.get(s..)) {
            Some(rest) => rest.iter().take(MAX).copied().take_while(|&b| b != 0).collect(),
            None => {
                let mut buf = [0u8; MAX];
                let address = self.base + u64::from(rva);
                // 末尾がページの端にかかると全体は読めないので、短くして読み直す。
                let len = [MAX, 64, 16]
                    .into_iter()
                    .find(|&len| self.memory.read(address, &mut buf[..len]))?;
                buf[..len].iter().copied().take_while(|&b| b != 0).collect()
            }
        };
        (!bytes.is_empty()).then(|| String::from_utf8_lossy(&bytes).into_owned())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::memory::BufferMemory;

    /// 試験用の最小の PE イメージを組み立てる。`exports` は（名前, 関数の RVA、0 なら転送）。
    pub(crate) fn build_image(machine: u16, dll_name: &str, exports: &[(&str, u32)]) -> Vec<u8> {
        let pe32_plus = machine == 0x8664;
        let mut image = vec![0u8; 0x2000];
        let put16 = |img: &mut Vec<u8>, at: usize, v: u16| img[at..at + 2].copy_from_slice(&v.to_le_bytes());
        let put32 = |img: &mut Vec<u8>, at: usize, v: u32| img[at..at + 4].copy_from_slice(&v.to_le_bytes());

        put16(&mut image, 0, 0x5a4d);
        put32(&mut image, 0x3c, 0x80);
        put32(&mut image, 0x80, 0x4550);
        put16(&mut image, 0x84, machine);
        let optional = 0x80 + 24;
        put16(&mut image, optional, if pe32_plus { 0x20b } else { 0x10b });
        put32(&mut image, optional + 56, 0x2000);
        let (count_offset, directories) = if pe32_plus { (108, 112) } else { (92, 96) };
        put32(&mut image, optional + count_offset, 16);

        // エクスポート表を 0x1000 から置く: ディレクトリ(40) → 関数表 → 名前表 → 序数表 → 文字列。
        let dir = 0x1000usize;
        let n = exports.len();
        let functions = dir + 40;
        let names = functions + 4 * n;
        let ordinals = names + 4 * n;
        let mut strings = ordinals + 2 * n;
        let mut place = |img: &mut Vec<u8>, s: &str| {
            let at = strings;
            img[at..at + s.len()].copy_from_slice(s.as_bytes());
            strings += s.len() + 1;
            at as u32
        };
        let name_rva = place(&mut image, dll_name);
        put32(&mut image, dir + 12, name_rva);
        put32(&mut image, dir + 20, n as u32);
        put32(&mut image, dir + 24, n as u32);
        put32(&mut image, dir + 28, functions as u32);
        put32(&mut image, dir + 32, names as u32);
        put32(&mut image, dir + 36, ordinals as u32);
        for (i, (name, target)) in exports.iter().enumerate() {
            let target = if *target == 0 {
                place(&mut image, "OTHER.Function")
            } else {
                *target
            };
            put32(&mut image, functions + 4 * i, target);
            let at = place(&mut image, name);
            put32(&mut image, names + 4 * i, at);
            put16(&mut image, ordinals + 2 * i, i as u16);
        }
        put32(&mut image, optional + directories, dir as u32);
        put32(&mut image, optional + directories + 4, (strings - dir) as u32);
        image
    }

    #[test]
    fn reads_pe32_exports_with_aliases_and_forwarders() {
        let image = build_image(
            0x014c,
            "ODBC32.dll",
            &[("SQLExecDirect", 0x500), ("SQLExecDirectA", 0x500), ("SQLForwarded", 0)],
        );
        let memory = BufferMemory::new().with(0x1000_0000, image);
        let module = read_module(&memory, 0x1000_0000).unwrap();
        assert_eq!(module.arch, Some(Arch::X86));
        assert_eq!(module.size_of_image, 0x2000);
        assert_eq!(module.export_name.as_deref(), Some("ODBC32.dll"));
        // 別名は同じ RVA を指す（ブレークポイントはアドレスで 1 つにまとめる）。
        assert_eq!(
            module.export("SQLExecDirect").unwrap().target,
            ExportTarget::Code(0x500)
        );
        assert_eq!(
            module.export("SQLExecDirectA").unwrap().target,
            ExportTarget::Code(0x500)
        );
        assert_eq!(
            module.export("SQLForwarded").unwrap().target,
            ExportTarget::Forwarder("OTHER.Function".into())
        );
        assert!(module.export("sqlexecdirect").is_none());
    }

    #[test]
    fn reads_pe32_plus_machine() {
        let image = build_image(0x8664, "oci.dll", &[("OCIStmtPrepare", 0x600)]);
        let memory = BufferMemory::new().with(0x7ff0_0000_0000, image);
        let module = read_module(&memory, 0x7ff0_0000_0000).unwrap();
        assert_eq!(module.arch, Some(Arch::X64));
        assert_eq!(
            module.export("OCIStmtPrepare").unwrap().target,
            ExportTarget::Code(0x600)
        );
    }

    #[test]
    fn headers_alone_give_range_and_arch() {
        let image = build_image(0x8664, "oci.dll", &[("OCIStmtPrepare", 0x600)]);
        let memory = BufferMemory::new().with(0x10_0000, image);
        let headers = read_headers(&memory, 0x10_0000).unwrap();
        assert_eq!(headers.arch, Some(Arch::X64));
        assert_eq!(headers.size_of_image, 0x2000);
    }

    #[test]
    fn rejects_non_pe() {
        let memory = BufferMemory::new().with(0x1000, vec![0u8; 0x100]);
        assert_eq!(read_module(&memory, 0x1000), Err(PeError::NotPe { base: 0x1000 }));
    }
}
