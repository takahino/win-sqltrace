//! sqlTrace が捕まえる API の入口の定義。
//!
//! 対象を足すときは、ここに [`ProbeDef`] を 1 件足すだけでよい（新しい層なら [`Layer`] も 1 つ足して `LAYERS` に入れる）。
//! 引数の位置は 0 始まり。x86 では stdcall（ODBC、DB-Library）も cdecl（OCI）も入口での引数の位置は同じ。

use sqltrace_core::{Catalog, Encoding, Layer, LengthArg, ProbeDef, Role, ScalarArg, SkipIf, TextArg, Unit, Width};

pub static ODBC: Layer = Layer {
    id: "odbc",
    name: "ODBC",
    // ODBC ドライバ。ここから OCI などを呼んだ分は、odbc32.dll で捕まえた呼び出しの内側。
    related_modules: &["sqora*.dll", "msorcl32.dll", "sqlsrv32.dll", "msodbcsql*.dll"],
};

pub static OCI: Layer = Layer {
    id: "oci",
    name: "Oracle OCI",
    related_modules: &[],
};

pub static OCI7: Layer = Layer {
    id: "oci7",
    name: "Oracle OCI7",
    related_modules: &[],
};

pub static DBLIB: Layer = Layer {
    id: "dblib",
    name: "DB-Library",
    related_modules: &[],
};

pub static LAYERS: [&Layer; 4] = [&ODBC, &OCI, &OCI7, &DBLIB];

const fn arg(index: u8, width: Width) -> ScalarArg {
    ScalarArg { index, width }
}

const fn handle(index: u8) -> Option<ScalarArg> {
    Some(arg(index, Width::Ptr))
}

/// 文字列のポインターと長さ（符号付き 32 ビット。負なら NUL 終端）。
const fn text(pointer: u8, length: u8, unit: Unit, encoding: Encoding) -> Option<TextArg> {
    Some(TextArg {
        pointer,
        length: Some(LengthArg {
            arg: arg(length, Width::I32),
            unit,
        }),
        encoding,
    })
}

const fn probe(
    layer: &'static Layer,
    modules: &'static [&'static str],
    function: &'static str,
    role: Role,
    sql: Option<TextArg>,
    handle: Option<ScalarArg>,
) -> ProbeDef {
    ProbeDef {
        layer,
        modules,
        function,
        role,
        sql,
        handle,
        skip_if: None,
    }
}

const ODBC_MODULES: &[&str] = &["odbc32.dll"];
/// OCI7 の関数は、古いクライアントでは ociw32.dll、8i 以降は oci.dll が持つ。
const OCI7_MODULES: &[&str] = &["ociw32.dll", "oci.dll"];

/// OCI の `mode` で、実行ではない呼び出し（OCI_DESCRIBE_ONLY = 0x10、OCI_PARSE_ONLY = 0x100）。
const OCI_NOT_EXECUTED: u64 = 0x10 | 0x100;

pub static PROBES: &[ProbeDef] = &[
    // ODBC: SQLExecDirect(hstmt, text, len) / SQLPrepare(hstmt, text, len)。SQL_NTS = -3。
    // 無印は ANSI（UNICODE を定義しないアプリが呼ぶ）。W の長さは文字数、A と無印はバイト数。
    probe(
        &ODBC,
        ODBC_MODULES,
        "SQLExecDirect",
        Role::ExecDirect,
        text(1, 2, Unit::Bytes, Encoding::Ansi),
        handle(0),
    ),
    probe(
        &ODBC,
        ODBC_MODULES,
        "SQLExecDirectA",
        Role::ExecDirect,
        text(1, 2, Unit::Bytes, Encoding::Ansi),
        handle(0),
    ),
    probe(
        &ODBC,
        ODBC_MODULES,
        "SQLExecDirectW",
        Role::ExecDirect,
        text(1, 2, Unit::Chars, Encoding::Utf16),
        handle(0),
    ),
    probe(
        &ODBC,
        ODBC_MODULES,
        "SQLPrepare",
        Role::Prepare,
        text(1, 2, Unit::Bytes, Encoding::Ansi),
        handle(0),
    ),
    probe(
        &ODBC,
        ODBC_MODULES,
        "SQLPrepareA",
        Role::Prepare,
        text(1, 2, Unit::Bytes, Encoding::Ansi),
        handle(0),
    ),
    probe(
        &ODBC,
        ODBC_MODULES,
        "SQLPrepareW",
        Role::Prepare,
        text(1, 2, Unit::Chars, Encoding::Utf16),
        handle(0),
    ),
    probe(&ODBC, ODBC_MODULES, "SQLExecute", Role::Execute, None, handle(0)),
    // OCI: OCIStmtPrepare(stmtp, errhp, stmt, stmt_len, language, mode)。stmt_len は ub4 のバイト数。
    // 文字コードはクライアントの文字集合（NLS_LANG、UTF-16 モードもある）なので推定する。
    probe(
        &OCI,
        &["oci.dll"],
        "OCIStmtPrepare",
        Role::Prepare,
        Some(TextArg {
            pointer: 2,
            length: Some(LengthArg {
                arg: arg(3, Width::U32),
                unit: Unit::Bytes,
            }),
            encoding: Encoding::Auto,
        }),
        handle(0),
    ),
    // OCIStmtPrepare2(svchp, &stmtp, errhp, stmt, stmt_len, key, key_len, language, mode)。
    // stmtp は出力なので入口では分からない（戻りでの取得は後の段階）。キー検索では stmt が NULL のことがある。
    probe(
        &OCI,
        &["oci.dll"],
        "OCIStmtPrepare2",
        Role::Prepare,
        Some(TextArg {
            pointer: 3,
            length: Some(LengthArg {
                arg: arg(4, Width::U32),
                unit: Unit::Bytes,
            }),
            encoding: Encoding::Auto,
        }),
        None,
    ),
    // OCIStmtExecute(svchp, stmtp, errhp, iters, rowoff, snap_in, snap_out, mode)。
    ProbeDef {
        layer: &OCI,
        modules: &["oci.dll"],
        function: "OCIStmtExecute",
        role: Role::Execute,
        sql: None,
        handle: handle(1),
        skip_if: Some(SkipIf {
            arg: arg(7, Width::U32),
            mask: OCI_NOT_EXECUTED,
        }),
    },
    // OCI7: oparse(cda, sqlstm, sqllen, defflg, lngflg) / osql3(cda, sqlstm, sqllen)。長さ -1 は NUL 終端。
    probe(
        &OCI7,
        OCI7_MODULES,
        "oparse",
        Role::Prepare,
        text(1, 2, Unit::Bytes, Encoding::Auto),
        handle(0),
    ),
    probe(
        &OCI7,
        OCI7_MODULES,
        "osql3",
        Role::Prepare,
        text(1, 2, Unit::Bytes, Encoding::Auto),
        handle(0),
    ),
    // oexec(cda) / oexn(cda, iters, rowoff) / oexfet(cda, nrows, cancel, exact)。
    probe(&OCI7, OCI7_MODULES, "oexec", Role::Execute, None, handle(0)),
    probe(&OCI7, OCI7_MODULES, "oexn", Role::Execute, None, handle(0)),
    probe(&OCI7, OCI7_MODULES, "oexfet", Role::Execute, None, handle(0)),
    // DB-Library（VC6 時代の SQL Server）: dbcmd(dbproc, cmd) で足し、dbsqlexec(dbproc) で実行する。
    probe(
        &DBLIB,
        &["ntwdblib.dll"],
        "dbcmd",
        Role::Append,
        Some(TextArg {
            pointer: 1,
            length: None,
            encoding: Encoding::Ansi,
        }),
        handle(0),
    ),
    // dbsqlexec は送信と結果待ちを、dbsqlsend は送信だけをする。どちらも溜めた SQL を実行する。
    probe(&DBLIB, &["ntwdblib.dll"], "dbsqlexec", Role::Execute, None, handle(0)),
    probe(&DBLIB, &["ntwdblib.dll"], "dbsqlsend", Role::Execute, None, handle(0)),
];

pub static CATALOG: Catalog = Catalog {
    layers: &LAYERS,
    probes: PROBES,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_probe_layer_is_listed() {
        for p in CATALOG.probes {
            assert!(
                CATALOG.layers.contains(&p.layer),
                "{} の層 {} が LAYERS にない",
                p.function,
                p.layer.id
            );
        }
    }

    #[test]
    fn functions_are_unique_per_layer() {
        let mut seen = std::collections::HashSet::new();
        for p in CATALOG.probes {
            assert!(seen.insert((p.layer.id, p.function)), "{} が重複している", p.function);
        }
    }

    #[test]
    fn layer_ids_are_lowercase_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for l in CATALOG.layers {
            assert_eq!(l.id, l.id.to_ascii_lowercase());
            assert!(seen.insert(l.id));
        }
    }

    #[test]
    fn execute_without_sql_has_a_handle() {
        for p in CATALOG
            .probes
            .iter()
            .filter(|p| p.role == Role::Execute || p.role == Role::Append)
        {
            assert!(
                p.handle.is_some(),
                "{} はハンドルなしでは SQL を対応付けられない",
                p.function
            );
        }
    }
}
