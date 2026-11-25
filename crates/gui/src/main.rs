//! sqlTrace の GUI。
// リリース版ではコンソールの窓を開かない（試すときに panic の表示が見えるよう、開発版では開く）。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod fonts;
#[cfg(windows)]
mod tracing;

#[cfg(not(windows))]
fn main() {
    eprintln!("sqltrace-gui はいまは Windows だけで動きます");
}

#[cfg(windows)]
/// `--attach <PID>` か `--run <EXE> [引数...]`。読めなければ、何も始めずに理由を返す。
fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Option<app::StartRequest>, String> {
    let usage = "起動時の引数は --attach <PID> か --run <EXE> [引数...] です";
    match args.next().as_deref() {
        None => Ok(None),
        Some("--attach") => {
            let pid = args.next().and_then(|p| p.parse().ok()).ok_or(usage)?;
            Ok(Some(app::StartRequest::Attach(pid)))
        }
        Some("--run") => {
            let exe = args.next().ok_or(usage)?;
            let arguments = args
                .map(|a| sqltrace_win::quote_argument(&a))
                .collect::<Vec<_>>()
                .join(" ");
            Ok(Some(app::StartRequest::Run { exe, arguments }))
        }
        Some(_) => Err(usage.to_owned()),
    }
}

#[cfg(windows)]
fn main() -> eframe::Result {
    let (request, usage_error) = match parse_args(std::env::args().skip(1)) {
        Ok(request) => (request, None),
        Err(message) => (None, Some(message)),
    };
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("sqlTrace")
            .with_icon(eframe::egui::IconData {
                // 64x64 の RGBA（tools/icon/make-icon.ps1 が SVG から作る。画像の読み込みの依存を足さないため）。
                rgba: include_bytes!("../../../assets/sqltrace-64.rgba").to_vec(),
                width: 64,
                height: 64,
            })
            .with_inner_size([1100.0, 700.0])
            .with_min_inner_size([760.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "sqlTrace",
        options,
        Box::new(move |cc| {
            let font = fonts::install_japanese(&cc.egui_ctx);
            let mut app = app::App::new(font, request);
            if let Some(message) = usage_error {
                app.show_error(message);
            }
            Ok(Box::new(app))
        }),
    )
}
