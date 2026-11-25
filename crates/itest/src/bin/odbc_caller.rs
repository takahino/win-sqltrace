//! 試験の対象: odbc32.dll の SQL の入口を、無効な（NULL の）ハンドルで呼ぶ。DB も DSN も要らない。
//! odbc32 は SQL_INVALID_HANDLE を返すだけだが、入口には必ず来るので、トレースの試験には十分。
//!
//! 使い方: `odbc_caller [--repeat N] [--seconds S] [--interval-ms M] [--threads T] [--churn] [--raise]
//!                      [--direct-only]`
//! - `--repeat 0` は止められるまで繰り返す。`--seconds` はその秒数だけ繰り返して正常に終わる（デタッチの後も
//!   壊れずに動き続けたかを、終了コードで確かめる）。
//! - `--until-stdin-closes` は標準入力が閉じられるまで繰り返して正常に終わる（試験が終わりの時を決める）。
//! - `--churn` は 1 回ごとにスレッドを作って終わらせる（トレース中のスレッドの生成と終了を試す）。
//! - `--raise` は 1 回ごとに自分で例外と int3 を起こし、自分のハンドラで受け取る。デバッガが横取りすると
//!   数が合わず、終了コード 3 で終わる。
//! - SQL は `SELECT <スレッド>-<回> <W|A|P>` で、トレースの側で取りこぼしと重複を数えられる。
//! - `--multiline` は複数行の SQL（[`sqltrace_itest::MULTILINE_SQL`]）を W 版と ANSI 版で 1 回ずつ送って終わる。
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
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use windows_sys::core::{s, w};
    use windows_sys::Win32::Foundation::EXCEPTION_BREAKPOINT;
    use windows_sys::Win32::System::Diagnostics::Debug::{
        AddVectoredExceptionHandler, RaiseException, EXCEPTION_CONTINUE_EXECUTION, EXCEPTION_CONTINUE_SEARCH,
        EXCEPTION_POINTERS,
    };
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

    const SQL_NTS: i32 = -3;
    /// 自分で起こして自分で受け取る例外（ソフトウェアの例外は 0xE で始める慣習）。
    const OWN_EXCEPTION: u32 = 0xE000_5154;

    type ExecDirectW = unsafe extern "system" fn(*mut c_void, *const u16, i32) -> i16;
    type ExecDirectA = unsafe extern "system" fn(*mut c_void, *const u8, i32) -> i16;
    type Execute = unsafe extern "system" fn(*mut c_void) -> i16;

    static OWN_SEEN: AtomicU64 = AtomicU64::new(0);
    static INT3_SEEN: AtomicU64 = AtomicU64::new(0);
    /// `--until-stdin-closes` で、標準入力が閉じられた。
    static STDIN_CLOSED: AtomicBool = AtomicBool::new(false);

    #[derive(Clone, Copy)]
    struct Odbc {
        exec_direct_w: ExecDirectW,
        exec_direct: ExecDirectA,
        prepare_w: ExecDirectW,
        execute: Execute,
    }

    struct Options {
        repeat: u64,
        seconds: Option<Duration>,
        interval: Duration,
        threads: u32,
        churn: bool,
        raise: bool,
        until_stdin_closes: bool,
        direct_only: bool,
        multiline: bool,
    }

    fn parse() -> Options {
        let mut options = Options {
            repeat: 1,
            seconds: None,
            interval: Duration::ZERO,
            threads: 1,
            churn: false,
            raise: false,
            until_stdin_closes: false,
            direct_only: false,
            multiline: false,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            let mut value = || args.next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            match arg.as_str() {
                "--repeat" => options.repeat = value(),
                "--seconds" => options.seconds = Some(Duration::from_secs(value())),
                "--interval-ms" => options.interval = Duration::from_millis(value()),
                "--threads" => options.threads = value().max(1) as u32,
                "--churn" => options.churn = true,
                "--raise" => options.raise = true,
                "--until-stdin-closes" => options.until_stdin_closes = true,
                "--direct-only" => options.direct_only = true,
                "--multiline" => options.multiline = true,
                other => {
                    eprintln!("知らない引数: {other}");
                    std::process::exit(2);
                }
            }
        }
        options
    }

    fn load() -> Odbc {
        // SAFETY: odbc32.dll は Windows に必ずある。関数の型は ODBC の宣言（SQLRETURN = i16、stdcall）に合わせる。
        unsafe {
            let module = LoadLibraryW(w!("odbc32.dll"));
            assert!(!module.is_null(), "odbc32.dll を読み込めません");
            let get = |name| GetProcAddress(module, name).expect("odbc32.dll の関数がありません");
            Odbc {
                exec_direct_w: std::mem::transmute::<unsafe extern "system" fn() -> isize, ExecDirectW>(get(s!(
                    "SQLExecDirectW"
                ))),
                exec_direct: std::mem::transmute::<unsafe extern "system" fn() -> isize, ExecDirectA>(get(s!(
                    "SQLExecDirect"
                ))),
                prepare_w: std::mem::transmute::<unsafe extern "system" fn() -> isize, ExecDirectW>(get(s!(
                    "SQLPrepareW"
                ))),
                execute: std::mem::transmute::<unsafe extern "system" fn() -> isize, Execute>(get(s!("SQLExecute"))),
            }
        }
    }

    /// 自分の例外と int3 を受け取って続ける。int3 は命令の次から続ける（そのままだと同じ int3 を繰り返す）。
    unsafe extern "system" fn handler(info: *mut EXCEPTION_POINTERS) -> i32 {
        // SAFETY: OS が渡す、例外の記録とコンテキストへの有効なポインター。
        let (record, context) = unsafe { (&*(*info).ExceptionRecord, &mut *(*info).ContextRecord) };
        match record.ExceptionCode as u32 {
            OWN_EXCEPTION => {
                OWN_SEEN.fetch_add(1, Ordering::SeqCst);
                EXCEPTION_CONTINUE_EXECUTION
            }
            code if code == EXCEPTION_BREAKPOINT as u32 => {
                INT3_SEEN.fetch_add(1, Ordering::SeqCst);
                let next = record.ExceptionAddress as usize + 1;
                #[cfg(target_arch = "x86_64")]
                {
                    context.Rip = next as u64;
                }
                #[cfg(target_arch = "x86")]
                {
                    context.Eip = next as u32;
                }
                EXCEPTION_CONTINUE_EXECUTION
            }
            _ => EXCEPTION_CONTINUE_SEARCH,
        }
    }

    fn raise_own() {
        // SAFETY: 自分のハンドラが受け取って続ける例外と int3。
        unsafe {
            RaiseException(OWN_EXCEPTION, 0, 0, std::ptr::null());
            std::arch::asm!("int3");
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn call_once(odbc: Odbc, thread: u32, i: u64, options: &Options) {
        let null = std::ptr::null_mut();
        // SAFETY: NULL のハンドルを渡すと odbc32 は SQL_INVALID_HANDLE を返す。文字列は呼び出しの間生きている。
        unsafe {
            (odbc.exec_direct_w)(null, wide(&format!("SELECT {thread}-{i} W")).as_ptr(), SQL_NTS);
            if !options.direct_only {
                let ansi = format!("SELECT {thread}-{i} A");
                (odbc.exec_direct)(null, ansi.as_ptr(), ansi.len() as i32);
                (odbc.prepare_w)(null, wide(&format!("SELECT {thread}-{i} P")).as_ptr(), SQL_NTS);
                (odbc.execute)(null);
            }
        }
        if options.raise {
            raise_own();
        }
    }

    fn keep_going(options: &Options, started: Instant, i: u64) -> bool {
        if options.until_stdin_closes {
            return !STDIN_CLOSED.load(Ordering::SeqCst);
        }
        match options.seconds {
            Some(limit) => started.elapsed() < limit,
            None => options.repeat == 0 || i < options.repeat,
        }
    }

    pub fn main() {
        let options = parse();
        let odbc = load();
        if options.raise {
            // SAFETY: プロセスの終わりまで有効な関数を、最初に呼ばれるハンドラとして登録する。
            unsafe { AddVectoredExceptionHandler(1, Some(handler)) };
        }
        // 初期化を終えたことを知らせる（アタッチの試験はこれを待つ）。
        #[allow(clippy::print_stdout)] // 標準出力は試験との取り決め（この 1 行だけ）。
        {
            println!("ready");
        }
        let _ = std::io::Write::flush(&mut std::io::stdout());

        if options.until_stdin_closes {
            std::thread::spawn(|| {
                let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
                STDIN_CLOSED.store(true, Ordering::SeqCst);
            });
        }
        if options.multiline {
            let sql = sqltrace_itest::MULTILINE_SQL;
            // SAFETY: NULL のハンドルを渡すと odbc32 は SQL_INVALID_HANDLE を返す。長さは W 版が文字数、ANSI 版がバイト数。
            unsafe {
                (odbc.exec_direct_w)(std::ptr::null_mut(), wide(sql).as_ptr(), SQL_NTS);
                (odbc.exec_direct)(std::ptr::null_mut(), sql.as_ptr(), sql.len() as i32);
            }
            return;
        }
        let started = Instant::now();
        let calls = AtomicU64::new(0);
        let pause = || {
            if !options.interval.is_zero() {
                std::thread::sleep(options.interval);
            }
        };
        if options.churn {
            let mut i = 0;
            while keep_going(&options, started, i) {
                std::thread::scope(|scope| {
                    for thread in 0..options.threads {
                        let (options, calls) = (&options, &calls);
                        scope.spawn(move || {
                            call_once(odbc, thread, i, options);
                            calls.fetch_add(1, Ordering::SeqCst);
                        });
                    }
                });
                i += 1;
                pause();
            }
        } else {
            std::thread::scope(|scope| {
                for thread in 0..options.threads {
                    let (options, calls, pause) = (&options, &calls, &pause);
                    scope.spawn(move || {
                        let mut i = 0;
                        while keep_going(options, started, i) {
                            call_once(odbc, thread, i, options);
                            calls.fetch_add(1, Ordering::SeqCst);
                            i += 1;
                            pause();
                        }
                    });
                }
            });
        }
        if options.raise {
            let calls = calls.load(Ordering::SeqCst);
            let (own, int3) = (OWN_SEEN.load(Ordering::SeqCst), INT3_SEEN.load(Ordering::SeqCst));
            if own != calls || int3 != calls {
                eprintln!("自分の例外の数が合いません: 呼び出し {calls}、例外 {own}、int3 {int3}");
                std::process::exit(3);
            }
        }
    }
}
