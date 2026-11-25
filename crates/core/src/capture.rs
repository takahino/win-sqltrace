//! API の入口で止めた 1 回の呼び出しを、定義（[`ProbeDef`]）に従って読む。
//!
//! デバッグ層は SQL を知らない。止めたスレッドを [`HitContext`] にして渡すと、ここが引数を解釈する。

use std::time::SystemTime;

use crate::abi::CallSite;
use crate::probe::ProbeDef;
use crate::text::{read_text, CodepageDecoder, Text, TextError, TextOptions};

/// 呼び出し元（戻り先）の場所。シンボルのないアプリで、SQL を出した箇所を探す手がかりにもなる。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallerInfo {
    pub address: u64,
    /// 戻り先を含むモジュールのファイル名。分からなければ `None`。
    pub module: Option<String>,
    /// モジュールの先頭からのオフセット（モジュールが分からなければ `address` と同じ）。
    pub offset: u64,
}

/// 止めた 1 回の呼び出し。デバッグ層が作る。
pub struct HitContext<'a> {
    pub time: SystemTime,
    pub pid: u32,
    pub tid: u32,
    pub site: CallSite<'a>,
    pub caller: Option<CallerInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SqlText {
    /// その入口は SQL を持たない（`SQLExecute` など）か、ポインターが NULL だった。
    Absent,
    Text(Text),
    Unreadable(TextError),
}

/// 読み取った 1 回の呼び出し。相関器に渡す。
#[derive(Clone, Debug)]
pub struct ProbeHit {
    pub time: SystemTime,
    pub pid: u32,
    pub tid: u32,
    pub probe: &'static ProbeDef,
    pub sql: SqlText,
    pub handle: Option<u64>,
    pub caller: Option<CallerInfo>,
}

/// 定義に従って引数を読む。`skip_if` に当たった（実行ではない呼び出しの）ときは `None`。
pub fn capture(
    probe: &'static ProbeDef,
    context: HitContext<'_>,
    decoder: &dyn CodepageDecoder,
    options: &TextOptions,
) -> Option<ProbeHit> {
    let site = &context.site;
    if let Some(skip) = probe.skip_if {
        // 読めないときは記録する側に倒す（取りこぼすより、余分に出るほうが調べやすい）。
        if site.scalar(skip.arg).is_some_and(|v| (v as u64) & skip.mask != 0) {
            return None;
        }
    }
    let sql = match probe.sql {
        None => SqlText::Absent,
        Some(spec) => {
            let pointer = site.scalar(crate::ScalarArg {
                index: spec.pointer,
                width: crate::Width::Ptr,
            });
            let length = spec.length.and_then(|len| site.scalar(len.arg).map(|v| (v, len.unit)));
            match pointer {
                None => SqlText::Unreadable(TextError::Unreadable { address: 0 }),
                Some(pointer) => {
                    match read_text(site.memory, pointer as u64, length, spec.encoding, decoder, options) {
                        Ok(Some(text)) => SqlText::Text(text),
                        Ok(None) => SqlText::Absent,
                        Err(error) => SqlText::Unreadable(error),
                    }
                }
            }
        }
    };
    let handle = probe.handle.and_then(|h| site.scalar(h)).map(|v| v as u64);
    Some(ProbeHit {
        time: context.time,
        pid: context.pid,
        tid: context.tid,
        probe,
        sql,
        handle,
        caller: context.caller,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::memory::BufferMemory;
    use crate::probe::{Encoding, Layer, LengthArg, Role, ScalarArg, SkipIf, TextArg, Unit, Width};
    use crate::{Arch, Registers};

    pub(crate) static LAYER: Layer = Layer {
        id: "test",
        name: "Test",
        related_modules: &["driver*.dll"],
    };

    pub(crate) static EXEC_DIRECT_W: ProbeDef = ProbeDef {
        layer: &LAYER,
        modules: &["odbc32.dll"],
        function: "SQLExecDirectW",
        role: Role::ExecDirect,
        sql: Some(TextArg {
            pointer: 1,
            length: Some(LengthArg {
                arg: ScalarArg {
                    index: 2,
                    width: Width::I32,
                },
                unit: Unit::Chars,
            }),
            encoding: Encoding::Utf16,
        }),
        handle: Some(ScalarArg {
            index: 0,
            width: Width::Ptr,
        }),
        skip_if: None,
    };

    static EXECUTE_WITH_MODE: ProbeDef = ProbeDef {
        layer: &LAYER,
        modules: &["oci.dll"],
        function: "OCIStmtExecute",
        role: Role::Execute,
        sql: None,
        handle: Some(ScalarArg {
            index: 1,
            width: Width::Ptr,
        }),
        skip_if: Some(SkipIf {
            arg: ScalarArg {
                index: 7,
                width: Width::U32,
            },
            mask: 0x10,
        }),
    };

    pub(crate) struct NoDecoder;

    impl CodepageDecoder for NoDecoder {
        fn decode(&self, bytes: &[u8], _codepage: u32) -> String {
            String::from_utf8_lossy(bytes).into_owned()
        }
    }

    fn context(memory: &BufferMemory, regs: Registers) -> HitContext<'_> {
        HitContext {
            time: SystemTime::UNIX_EPOCH,
            pid: 1,
            tid: 2,
            site: CallSite {
                arch: Arch::X64,
                regs,
                memory,
            },
            caller: None,
        }
    }

    #[test]
    fn reads_sql_and_handle_from_win64_registers() {
        let sql: Vec<u8> = "SELECT 1".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let memory = BufferMemory::new().with(0x5000, sql);
        // rcx = hstmt、rdx = 文字列、r8 = SQL_NTS（上位にゴミ）。文字列は終端がなく領域の端で切れる。
        let regs = Registers {
            args: [0xabc, 0x5000, 0xdead_0000_ffff_fffd, 0],
            ..Default::default()
        };
        let hit = capture(
            &EXEC_DIRECT_W,
            context(&memory, regs),
            &NoDecoder,
            &TextOptions::default(),
        )
        .unwrap();
        assert_eq!(hit.handle, Some(0xabc));
        let SqlText::Text(text) = hit.sql else {
            panic!("SQL が読めていない: {:?}", hit.sql)
        };
        assert_eq!(text.value, "SELECT 1");
    }

    #[test]
    fn skip_if_mask_drops_describe_only_execute() {
        // 8 番目の引数 (index 7) はスタック: [rsp+8+8*7]。
        let mut stack = vec![0u8; 8 * 8];
        stack.extend_from_slice(&0x10u64.to_le_bytes());
        let memory = BufferMemory::new().with(0x9000, stack);
        let regs = Registers {
            sp: 0x9000,
            ..Default::default()
        };
        assert!(capture(
            &EXECUTE_WITH_MODE,
            context(&memory, regs),
            &NoDecoder,
            &TextOptions::default()
        )
        .is_none());
    }
}
