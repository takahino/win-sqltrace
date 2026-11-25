//! 試験の対象: 試験用の oci.dll（OCI と OCI7）と sqora32.dll（ODBC ドライバのふり）を決まった順に呼ぶ。
//!
//! 使い方: `oci_caller --dll-dir <DIR>`（DIR に oci.dll と sqora32.dll がある）
//!
//! ハンドルは本物の確保をせず、見分けのつく値を使う（スタブの DLL は中身を見ない）。呼ぶ順と SQL は
//! 試験（tests/oci.rs）の期待と対応している。
#![cfg_attr(not(windows), allow(unused))]

#[cfg(windows)]
fn main() {
    windows::main();
}

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    use std::path::PathBuf;

    use windows_sys::core::s;
    use windows_sys::Win32::Foundation::HMODULE;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

    type Handle = *mut c_void;
    type Prepare = unsafe extern "C" fn(Handle, Handle, *const u8, u32, u32, u32) -> i32;
    type Prepare2 = unsafe extern "C" fn(Handle, *mut Handle, Handle, *const u8, u32, *const u8, u32, u32, u32) -> i32;
    type Execute = unsafe extern "C" fn(Handle, Handle, Handle, u32, u32, Handle, Handle, u32) -> i32;
    type Oparse = unsafe extern "C" fn(Handle, *const u8, i32, i32, u32) -> i32;
    type Osql3 = unsafe extern "C" fn(Handle, *const u8, i32) -> i32;
    type Oexec = unsafe extern "C" fn(Handle) -> i32;
    type Oexn = unsafe extern "C" fn(Handle, i32, i32) -> i32;
    type Oexfet = unsafe extern "C" fn(Handle, u32, i32, i32) -> i32;
    type DriverExecute = unsafe extern "C" fn(Handle, *const u8, u32) -> i32;

    /// OCI_DESCRIBE_ONLY: 実行せずに列の情報だけを得る。sqlTrace は実行として出さない。
    const OCI_DESCRIBE_ONLY: u32 = 0x10;
    const OCI_NTV_SYNTAX: u32 = 1;

    fn handle(value: usize) -> Handle {
        value as Handle
    }

    fn load(dir: &std::path::Path, name: &str) -> HMODULE {
        let path: Vec<u16> = dir
            .join(name)
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: NUL 終端のパス。
        let module = unsafe { LoadLibraryW(path.as_ptr()) };
        assert!(!module.is_null(), "{name} を読み込めません");
        module
    }

    use std::os::windows::ffi::OsStrExt;

    /// 関数を宣言どおりの型で取り出す。
    macro_rules! function {
        ($module:expr, $name:literal, $type:ty) => {{
            // SAFETY: スタブの DLL の関数で、型は宣言（oci.h など）に合わせてある。
            unsafe {
                let f = GetProcAddress($module, s!($name)).expect(concat!($name, " がありません"));
                std::mem::transmute::<unsafe extern "system" fn() -> isize, $type>(f)
            }
        }};
    }

    pub fn main() {
        let mut args = std::env::args().skip(1);
        let dir = match (args.next().as_deref(), args.next()) {
            (Some("--dll-dir"), Some(dir)) => PathBuf::from(dir),
            _ => {
                eprintln!("使い方: oci_caller --dll-dir <DIR>");
                std::process::exit(2);
            }
        };
        let oci = load(&dir, "oci.dll");
        let driver = load(&dir, "sqora32.dll");
        let prepare = function!(oci, "OCIStmtPrepare", Prepare);
        let prepare2 = function!(oci, "OCIStmtPrepare2", Prepare2);
        let execute = function!(oci, "OCIStmtExecute", Execute);
        let oparse = function!(oci, "oparse", Oparse);
        let osql3 = function!(oci, "osql3", Osql3);
        let oexec = function!(oci, "oexec", Oexec);
        let oexn = function!(oci, "oexn", Oexn);
        let oexfet = function!(oci, "oexfet", Oexfet);
        let driver_execute = function!(driver, "driver_execute", DriverExecute);

        let (svc, err) = (handle(0x100), handle(0x200));
        let null = std::ptr::null_mut();
        let utf8 = "SELECT '日本' FROM dual";
        let utf16: Vec<u8> = "SELECT N'日本' FROM dual"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        // SAFETY: 文字列は呼び出しの間生きていて、長さはバイト数。スタブはハンドルの中身を見ない。
        unsafe {
            // OCI: 準備と実行。実行には SQL がなく、同じ文のハンドルの準備から付く。
            let sql = b"SELECT 1 FROM dual";
            prepare(handle(0x1000), err, sql.as_ptr(), sql.len() as u32, OCI_NTV_SYNTAX, 0);
            execute(svc, handle(0x1000), err, 1, 0, null, null, 0);
            // 列の情報だけを得る呼び出しは、実行として出さない。
            execute(svc, handle(0x1000), err, 0, 0, null, null, OCI_DESCRIBE_ONLY);
            // 文字コードは推定する: UTF-8 と、UTF-16 の環境（OCI_UTF16ID）。
            prepare(handle(0x1001), err, utf8.as_ptr(), utf8.len() as u32, OCI_NTV_SYNTAX, 0);
            execute(svc, handle(0x1001), err, 1, 0, null, null, 0);
            prepare(
                handle(0x1002),
                err,
                utf16.as_ptr(),
                utf16.len() as u32,
                OCI_NTV_SYNTAX,
                0,
            );
            execute(svc, handle(0x1002), err, 1, 0, null, null, 0);
            // OCIStmtPrepare2 は文のハンドルを返すので、入口では SQL とハンドルを結び付けられない。
            let sql = b"SELECT 2 FROM dual";
            let mut statement: Handle = null;
            prepare2(
                svc,
                &mut statement,
                err,
                sql.as_ptr(),
                sql.len() as u32,
                std::ptr::null(),
                0,
                1,
                0,
            );
            execute(svc, statement, err, 1, 0, null, null, 0);

            // OCI7: cda がハンドル。長さ -1 は NUL 終端。
            let cda = handle(0x2000);
            oparse(cda, c"SELECT 7 FROM dual".as_ptr().cast(), -1, 0, 2);
            oexec(cda);
            let update = b"UPDATE t SET x = 1";
            osql3(handle(0x2001), update.as_ptr(), update.len() as i32);
            oexn(handle(0x2001), 1, 0);
            oexfet(cda, 10, 0, 0);

            // ODBC ドライバ（sqora32.dll）の中から呼ばれた OCI は、内側の呼び出しとして出さない。
            let inner = b"SELECT inner FROM dual";
            driver_execute(handle(0x3000), inner.as_ptr(), inner.len() as u32);
        }
    }
}
