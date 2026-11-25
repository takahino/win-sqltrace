//! ジャーナル: 置いたブレークポイントと元のバイトをファイルに書いておき、sqltrace が強制終了されても
//! `sqltrace repair <pid>` で元に戻せるようにする。
//!
//! 完全には防げない: デバッガが死ぬと対象はデタッチされて動き続けるので、repair の前に int3 を踏むと
//! 対象は落ちる。ステップ実行のために止めていたスレッドも止まったまま残る（ここには書いていない）。

use std::fmt;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

use sqltrace_core::Memory;
use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ,
    PROCESS_VM_WRITE,
};

use crate::error::last_error;
use crate::memory::ProcessMemory;

const HEADER: &str = "sqltrace-journal 1";
const INT3: u8 = 0xCC;

/// ジャーナルを置くフォルダー（`%LOCALAPPDATA%\sqlTrace\journal`）。
pub fn journal_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map_or_else(std::env::temp_dir, PathBuf::from);
    base.join("sqlTrace").join("journal")
}

fn journal_path(pid: u32) -> PathBuf {
    journal_dir().join(format!("{pid}.journal"))
}

/// プロセスの作成時刻。PID は使い回されるので、repair で同じプロセスかを確かめるのに使う。
pub(crate) fn creation_time(process: HANDLE) -> Option<u64> {
    let mut times = [FILETIME::default(); 4];
    let [created, exited, kernel, user] = &mut times;
    // SAFETY: 4 つの出力先はどれも有効な FILETIME。
    let ok = unsafe { GetProcessTimes(process, created, exited, kernel, user) } != 0;
    ok.then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

pub(crate) struct Journal {
    pid: u32,
    created: u64,
    file: File,
}

impl Journal {
    pub(crate) fn create(pid: u32, created: u64) -> std::io::Result<Self> {
        fs::create_dir_all(journal_dir())?;
        let mut journal = Self {
            pid,
            created,
            file: File::create(journal_path(pid))?,
        };
        journal.write_header()?;
        Ok(journal)
    }

    fn write_header(&mut self) -> std::io::Result<()> {
        // tracer はトレースしている sqltrace の PID（2 つ目の sqltrace に、誰が使っているかを伝えるため）。
        writeln!(
            self.file,
            "{HEADER}\npid {}\ncreated {}\ntracer {}",
            self.pid,
            self.created,
            std::process::id()
        )?;
        self.file.flush()
    }

    /// 置いた分を書き足す。int3 を書く前に呼ぶ（書いた後で落ちても戻せるように）。
    pub(crate) fn record(&mut self, entries: &[(u64, u8)]) -> std::io::Result<()> {
        for (address, original) in entries {
            writeln!(self.file, "bp {address:x} {original:02x}")?;
        }
        self.file.flush()
    }

    /// 書き直す（アンロードで外したアドレスを残さないため。後で別のモジュールがそこに来ることがある）。
    pub(crate) fn rewrite(&mut self, entries: &[(u64, u8)]) -> std::io::Result<()> {
        self.file = File::create(journal_path(self.pid))?;
        self.write_header()?;
        self.record(entries)
    }

    pub(crate) fn remove(self) {
        drop(self.file);
        let _ = fs::remove_file(journal_path(self.pid));
    }
}

/// その PID をトレースしている sqltrace の PID（ジャーナルの記録から）。分からなければ `None`。
pub(crate) fn tracer_of(pid: u32) -> Option<u32> {
    let text = fs::read_to_string(journal_path(pid)).ok()?;
    text.lines().find_map(|l| l.strip_prefix("tracer ")?.parse().ok())
}

#[derive(Debug, PartialEq, Eq)]
struct Parsed {
    pid: u32,
    created: u64,
    breakpoints: Vec<(u64, u8)>,
}

fn parse(text: &str) -> Option<Parsed> {
    let mut lines = text.lines();
    if lines.next()? != HEADER {
        return None;
    }
    let pid = lines.next()?.strip_prefix("pid ")?.parse().ok()?;
    let created = lines.next()?.strip_prefix("created ")?.parse().ok()?;
    let mut breakpoints = Vec::new();
    for line in lines {
        let Some(rest) = line.strip_prefix("bp ") else {
            continue;
        };
        let mut parts = rest.split(' ');
        let address = u64::from_str_radix(parts.next()?, 16).ok()?;
        let original = u8::from_str_radix(parts.next()?, 16).ok()?;
        breakpoints.push((address, original));
    }
    Some(Parsed {
        pid,
        created,
        breakpoints,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub struct RepairReport {
    /// int3 を元に戻した数。
    pub restored: usize,
    /// もう int3 ではなかった（元に戻っていた）数。
    pub untouched: usize,
}

#[derive(Debug)]
pub enum RepairError {
    NoJournal {
        pid: u32,
    },
    Malformed {
        path: PathBuf,
    },
    /// プロセスがもうない。ジャーナルは消した。
    Gone {
        pid: u32,
    },
    /// 同じ PID の別のプロセス（元のプロセスは終わっている）。ジャーナルは消した。
    Reused {
        pid: u32,
    },
    Open {
        pid: u32,
        code: u32,
    },
    /// ほかの sqltrace がトレースしている最中（戻すとその sqltrace が壊れる）。
    InUse {
        pid: u32,
        tracer: Option<u32>,
    },
}

impl fmt::Display for RepairError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RepairError::NoJournal { pid } => write!(
                f,
                "PID {pid} のジャーナルがありません（{}）。正常にデタッチしていれば修復は要りません",
                journal_dir().display()
            ),
            RepairError::Malformed { path } => {
                write!(
                    f,
                    "ジャーナル {} を読めません。壊れているので消してください",
                    path.display()
                )
            }
            RepairError::Gone { pid } => {
                write!(
                    f,
                    "PID {pid} はもう終わっています。修復は要りません（ジャーナルは消しました）"
                )
            }
            RepairError::Reused { pid } => write!(
                f,
                "PID {pid} は別のプロセスに使い回されています。元のプロセスは終わっているので修復は要りません\
                 （ジャーナルは消しました）"
            ),
            RepairError::Open { pid, code } => write!(
                f,
                "PID {pid} を開けませんでした（{}）。管理者として実行してください",
                std::io::Error::from_raw_os_error(*code as i32)
            ),
            RepairError::InUse { pid, tracer } => write!(
                f,
                "PID {pid} は{}がトレースしている最中なので、修復しません。修復は要りません\
                 （止めれば、その sqltrace がブレークポイントを戻します）",
                tracer_name(*tracer)
            ),
        }
    }
}

/// 「別の sqltrace（PID 5678）」。PID が分からなければ「別の sqltrace」。
pub(crate) fn tracer_name(tracer: Option<u32>) -> String {
    match tracer {
        Some(pid) => format!("別の sqltrace（PID {pid}）"),
        None => "別の sqltrace".into(),
    }
}

impl std::error::Error for RepairError {}

/// ジャーナルに従って、残った int3 を元のバイトに戻す。今の値が int3 のところだけ書く。
pub fn repair(pid: u32) -> Result<RepairReport, RepairError> {
    if crate::lock::is_held(pid) {
        return Err(RepairError::InUse {
            pid,
            tracer: tracer_of(pid),
        });
    }
    let path = journal_path(pid);
    let text = fs::read_to_string(&path).map_err(|_| RepairError::NoJournal { pid })?;
    let parsed = parse(&text).ok_or_else(|| RepairError::Malformed { path: path.clone() })?;
    let remove = || {
        let _ = fs::remove_file(&path);
    };
    // SAFETY: 引数は値だけ。失敗すれば NULL が返る。
    let handle = unsafe {
        OpenProcess(
            PROCESS_VM_READ | PROCESS_VM_WRITE | PROCESS_VM_OPERATION | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            parsed.pid,
        )
    };
    if handle.is_null() {
        let code = last_error();
        // ERROR_INVALID_PARAMETER: その PID のプロセスがない。
        if code == 87 {
            remove();
            return Err(RepairError::Gone { pid });
        }
        return Err(RepairError::Open { pid, code });
    }
    let result = (|| {
        if creation_time(handle) != Some(parsed.created) {
            return Err(RepairError::Reused { pid });
        }
        let memory = ProcessMemory::new(handle);
        let mut report = RepairReport {
            restored: 0,
            untouched: 0,
        };
        for &(address, original) in &parsed.breakpoints {
            let mut current = [0u8];
            if memory.read(address, &mut current) && current[0] == INT3 && memory.write_code(address, &[original]) {
                report.restored += 1;
            } else {
                report.untouched += 1;
            }
        }
        Ok(report)
    })();
    // SAFETY: 上で開いたハンドルを 1 回だけ閉じる。
    unsafe { CloseHandle(handle) };
    if !matches!(result, Err(RepairError::Open { .. })) {
        remove();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_what_it_writes() {
        let text = format!("{HEADER}\npid 42\ncreated 1234\nbp 7ffe0010 8b\nbp 10 55\n");
        assert_eq!(
            parse(&text),
            Some(Parsed {
                pid: 42,
                created: 1234,
                breakpoints: vec![(0x7ffe_0010, 0x8b), (0x10, 0x55)],
            })
        );
        assert_eq!(parse("other\n"), None);
    }
}
