//! sqltrace のコマンドライン。

mod args;

use std::process::ExitCode;

use args::{Command, UsageError, USAGE};

#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!("sqltrace はいまは Windows だけで動きます");
    ExitCode::FAILURE
}

#[cfg(windows)]
fn main() -> ExitCode {
    match args::parse(std::env::args().skip(1)) {
        Ok(command) => windows::execute(command),
        Err(UsageError(message)) => {
            eprintln!("{message}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use sqltrace_core::{LaunchSpec, Selection, Status, TargetSpec, TraceEvent};
    use sqltrace_export::{Console, OutputPlan, Recorder, TimeZone};
    use sqltrace_session::{SessionConfig, StopSignal};

    use super::*;
    use crate::args::Outputs;

    pub fn execute(command: Command) -> ExitCode {
        match command {
            Command::Help => {
                eprintln!("{USAGE}");
                ExitCode::SUCCESS
            }
            Command::Layers => {
                for layer in sqltrace_catalog::CATALOG.layers {
                    eprintln!("{:<8} {}", layer.id, layer.name);
                }
                ExitCode::SUCCESS
            }
            Command::Repair { pid } => match sqltrace_win::repair(pid) {
                Ok(report) => {
                    eprintln!(
                        "PID {pid}: {} か所を元に戻しました（{} か所は既に戻っていました）",
                        report.restored, report.untouched
                    );
                    ExitCode::SUCCESS
                }
                Err(error) => fail(error),
            },
            Command::Export { input, outputs } => {
                // export でファイルを指定しなければ、コンソールに出す。
                let console = if outputs.quiet || outputs.html.is_some() || outputs.xlsx.is_some() {
                    Console::Off
                } else {
                    Console::All
                };
                let plan = OutputPlan {
                    console,
                    ..plan(&outputs)
                };
                match sqltrace_export::export_saved(&input, &plan, zone()) {
                    Ok(written) => {
                        report_written(&written);
                        ExitCode::SUCCESS
                    }
                    Err(error) => fail(error),
                }
            }
            Command::Attach { pid, common } => trace(TargetSpec::Attach { pid }, format!("PID {pid}"), &common),
            Command::Run { run, common } => {
                let mut spec = LaunchSpec::new(run.exe);
                spec.arguments = run
                    .arguments
                    .iter()
                    .map(|a| sqltrace_win::quote_argument(a))
                    .collect::<Vec<_>>()
                    .join(" ");
                spec.working_dir = run.working_dir;
                spec.environment = run.environment;
                spec.new_console = !run.same_console;
                let title = format!("{} {}", spec.exe.display(), spec.arguments);
                trace(TargetSpec::Launch(spec), title.trim_end().to_owned(), &common)
            }
        }
    }

    fn fail(error: impl std::fmt::Display) -> ExitCode {
        eprintln!("{error}");
        ExitCode::FAILURE
    }

    fn zone() -> TimeZone {
        TimeZone {
            offset_minutes: sqltrace_win::utc_offset_minutes(),
        }
    }

    fn plan(outputs: &Outputs) -> OutputPlan {
        OutputPlan {
            console: if outputs.quiet {
                Console::StatusOnly
            } else {
                Console::All
            },
            color: use_color(outputs),
            ndjson: outputs.ndjson.clone(),
            html: outputs.html.clone(),
            xlsx: outputs.xlsx.clone(),
        }
    }

    /// 色は、端末に出すときだけ付ける（ファイルにリダイレクトした SQL に色の記号を混ぜない）。
    /// 慣習の環境変数 NO_COLOR があれば付けない。
    fn use_color(outputs: &Outputs) -> bool {
        use std::io::IsTerminal;
        !outputs.no_color
            && std::env::var_os("NO_COLOR").is_none()
            && std::io::stdout().is_terminal()
            && sqltrace_win::enable_console_colors()
    }

    fn report_written(written: &[PathBuf]) {
        for path in written {
            eprintln!("書き出しました: {}", path.display());
        }
    }

    fn trace(target: TargetSpec, title: String, common: &args::Common) -> ExitCode {
        let config = SessionConfig {
            selection: common.layers.as_deref().map(Selection::parse).unwrap_or_default(),
            suppress_nested: !common.keep_nested,
            ..SessionConfig::default()
        };
        // 出力先の誤りは、対象に触る前に知らせる。
        let mut recorder = match Recorder::open(&plan(&common.outputs), zone(), title) {
            Ok(recorder) => recorder,
            Err(error) => return fail(error),
        };
        // 管理者でなければ付かないが、自分のプロセスにはそのままでもアタッチできる。
        sqltrace_win::enable_debug_privilege();
        let stop = StopSignal::new();
        let finished = Arc::new(AtomicBool::new(false));
        sqltrace_win::install_console_handler(stop.clone(), finished.clone());
        let result = sqltrace_session::run(&target, &config, &stop, &mut |event| {
            recorder.record(&event);
            // 止め方の案内は、付いてから出す（断られたときに出さない）。
            if matches!(
                event,
                TraceEvent::Status(Status::Attached { .. } | Status::Launched { .. })
            ) {
                eprintln!("Ctrl+C で止めます（ブレークポイントを外して離れ、対象は動き続けます）");
            }
        });
        // 止めた後でも、ここまでの記録は書き出す。
        let written = recorder.finish();
        finished.store(true, Ordering::SeqCst);
        let mut code = ExitCode::SUCCESS;
        match written {
            Ok(written) => report_written(&written),
            Err(error) => code = fail(error),
        }
        if let Err(error) = result {
            code = fail(error);
        }
        code
    }
}
