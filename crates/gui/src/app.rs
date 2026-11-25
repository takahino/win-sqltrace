//! 画面: 対象を選ぶ（アタッチ / 起動）、捕まえる層と出力先を決める、捕まえた SQL を見る。

use std::path::{Path, PathBuf};
use std::time::Duration;

use eframe::egui::text::LayoutJob;
use eframe::egui::{self, Color32, CursorIcon, RichText, Sense, TextFormat, ViewportCommand};
use egui_extras::{Column, TableBuilder};
use sqltrace_core::{LaunchSpec, Layer, Selection, TargetSpec};
use sqltrace_export::highlight::pieces;
use sqltrace_export::{Console, Entry, Kind, OutputPlan, TimeZone};
use sqltrace_session::SessionConfig;
use sqltrace_win::{PickedWindow, ProcessInfo};

use crate::tracing::{Finished, Tracing};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Attach,
    Launch,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputKind {
    Html,
    Xlsx,
    Ndjson,
}

impl OutputKind {
    fn label(self) -> &'static str {
        match self {
            OutputKind::Html => "HTML",
            OutputKind::Xlsx => "Excel",
            OutputKind::Ndjson => "NDJSON（生の記録）",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            OutputKind::Html => "html",
            OutputKind::Xlsx => "xlsx",
            OutputKind::Ndjson => "ndjson",
        }
    }
}

struct OutputSlot {
    kind: OutputKind,
    enabled: bool,
    path: String,
}

enum Message {
    Info(String),
    Error(String),
}

pub struct App {
    zone: TimeZone,
    mode: Mode,

    processes: Vec<ProcessInfo>,
    process_filter: String,
    selected_pid: Option<u32>,
    /// 照準でドラッグしている間は `Some`。中身は今カーソルの下にあるウィンドウ。
    picking: Option<Option<PickedWindow>>,

    exe: String,
    arguments: String,
    working_dir: String,
    new_console: bool,

    layers: Vec<(&'static Layer, bool)>,
    keep_nested: bool,
    outputs: Vec<OutputSlot>,

    tracing: Option<Tracing>,
    /// トレースしている相手（見出しと出力の題に使う）。
    title: String,
    entries: Vec<Entry>,
    filter: String,
    only_execute: bool,
    follow: bool,
    /// 表に出す行（`entries` の位置）。絞り込みか件数が変わったときだけ作り直す。
    visible: Vec<usize>,
    visible_key: Option<(String, bool, usize)>,
    selected: Option<usize>,
    messages: Vec<Message>,
    /// 窓を閉じようとしたらトレース中だった。離れ終わったら閉じる。
    close_after_stop: bool,
    /// 起動時の引数で指定された対象。最初のフレームで開始する。
    start_on_launch: bool,
}

/// 起動時の引数（`--attach <PID>` か `--run <EXE> [引数...]`）。指定すれば、開いてすぐにトレースを始める。
pub enum StartRequest {
    Attach(u32),
    Run { exe: String, arguments: String },
}

impl App {
    pub fn new(font: Option<&str>, request: Option<StartRequest>) -> Self {
        let mut app = Self {
            zone: TimeZone {
                offset_minutes: sqltrace_win::utc_offset_minutes(),
            },
            mode: Mode::Attach,
            processes: Vec::new(),
            process_filter: String::new(),
            selected_pid: None,
            picking: None,
            exe: String::new(),
            arguments: String::new(),
            working_dir: String::new(),
            new_console: true,
            layers: sqltrace_catalog::CATALOG.layers.iter().map(|l| (*l, true)).collect(),
            keep_nested: false,
            outputs: [OutputKind::Html, OutputKind::Xlsx, OutputKind::Ndjson]
                .into_iter()
                .map(|kind| OutputSlot {
                    kind,
                    enabled: false,
                    path: String::new(),
                })
                .collect(),
            tracing: None,
            title: String::new(),
            entries: Vec::new(),
            filter: String::new(),
            only_execute: false,
            follow: true,
            visible: Vec::new(),
            visible_key: None,
            selected: None,
            messages: Vec::new(),
            close_after_stop: false,
            start_on_launch: request.is_some(),
        };
        match request {
            Some(StartRequest::Attach(pid)) => {
                app.mode = Mode::Attach;
                app.selected_pid = Some(pid);
            }
            Some(StartRequest::Run { exe, arguments }) => {
                app.mode = Mode::Launch;
                app.exe = exe;
                app.arguments = arguments;
            }
            None => {}
        }
        if font.is_none() {
            app.error(
                "日本語のフォント（游ゴシック、メイリオ、MS ゴシック）が見つからないので、日本語が正しく表示されません",
            );
        }
        // 管理者で動いていれば、別のユーザーのプロセスにも付けられるようにする。
        sqltrace_win::enable_debug_privilege();
        app.refresh_processes();
        app
    }

    fn info(&mut self, text: impl Into<String>) {
        self.messages.push(Message::Info(text.into()));
    }

    pub fn show_error(&mut self, text: impl Into<String>) {
        self.error(text);
    }

    fn error(&mut self, text: impl Into<String>) {
        self.messages.push(Message::Error(text.into()));
    }

    fn refresh_processes(&mut self) {
        let mut list = sqltrace_win::list_processes();
        list.sort_by_cached_key(|p| (p.name.to_lowercase(), p.pid));
        self.processes = list;
    }

    fn tracing(&self) -> bool {
        self.tracing.is_some()
    }

    // ------------------------------------------------------------------------------------------------
    // 毎フレームの処理（描画の前）

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(tracing) = &self.tracing {
            let new = tracing.drain();
            self.entries.extend(new);
            if tracing.is_finished() {
                if let Some(tracing) = self.tracing.take() {
                    let (finished, rest) = tracing.finish();
                    self.entries.extend(rest);
                    self.report(finished);
                }
                if self.close_after_stop {
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }
        if self.picking.is_some() {
            self.poll_picker(ctx);
        }
        // トレース中に窓を閉じようとしたら、離れてから閉じる（対象に int3 を残さない）。
        if ctx.input(|i| i.viewport().close_requested()) {
            if let Some(tracing) = &self.tracing {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                tracing.detach();
                self.close_after_stop = true;
            }
        }
        if self.tracing() {
            // 出来事が来れば再描画を頼まれるが、止めたときの終わりにも気づけるように時々見る。
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn report(&mut self, finished: Finished) {
        match finished.trace {
            Ok(()) => self.info(format!("{} のトレースを終えました", self.title)),
            Err(error) => self.error(error.to_string()),
        }
        match finished.written {
            Ok(written) => {
                for path in written {
                    self.info(format!("書き出しました: {}", path.display()));
                }
            }
            Err(error) => self.error(error.to_string()),
        }
    }

    fn poll_picker(&mut self, ctx: &egui::Context) {
        ctx.set_cursor_icon(CursorIcon::Crosshair);
        let hovered = sqltrace_win::window_under_cursor();
        if sqltrace_win::left_button_down() {
            self.picking = Some(hovered);
            ctx.request_repaint_after(Duration::from_millis(30));
            return;
        }
        // ボタンを離した。そのときカーソルの下にあったウィンドウのプロセスを選ぶ。
        self.picking = None;
        match hovered {
            Some(window) if window.pid == std::process::id() => {
                self.error("sqltrace 自身のウィンドウです。トレースしたいアプリのウィンドウの上で離してください");
            }
            Some(window) => {
                self.mode = Mode::Attach;
                self.selected_pid = Some(window.pid);
                if !self.processes.iter().any(|p| p.pid == window.pid) {
                    self.refresh_processes();
                }
                self.info(format!(
                    "PID {}（{}）を選びました。「開始」で付けます",
                    window.pid,
                    if window.title.is_empty() {
                        &window.class
                    } else {
                        &window.title
                    }
                ));
            }
            None => self.error("ウィンドウが見つかりませんでした"),
        }
    }

    // ------------------------------------------------------------------------------------------------
    // 開始と停止

    fn can_start(&self) -> Result<(), &'static str> {
        if self.tracing() {
            return Err("トレース中です");
        }
        if !self.layers.iter().any(|(_, on)| *on) {
            return Err("捕まえる層を 1 つ以上選んでください");
        }
        if self.outputs.iter().any(|o| o.enabled && o.path.trim().is_empty()) {
            return Err("出力先のファイルを指定してください");
        }
        match self.mode {
            Mode::Attach => match self
                .selected_pid
                .and_then(|pid| self.processes.iter().find(|p| p.pid == pid))
            {
                None if self.selected_pid.is_none() => Err("一覧から選ぶか、照準でウィンドウを選んでください"),
                Some(p) if p.arch.is_some() && !p.traceable() => {
                    Err("この sqltrace ではトレースできないアーキテクチャです")
                }
                _ => Ok(()),
            },
            Mode::Launch if self.exe.trim().is_empty() => Err("起動する実行ファイルを指定してください"),
            Mode::Launch => Ok(()),
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        let target = match self.mode {
            Mode::Attach => {
                let Some(pid) = self.selected_pid else {
                    return;
                };
                let name = self
                    .processes
                    .iter()
                    .find(|p| p.pid == pid)
                    .map_or_else(String::new, |p| format!(" {}", p.name));
                self.title = format!("PID {pid}{name}");
                TargetSpec::Attach { pid }
            }
            Mode::Launch => {
                let mut spec = LaunchSpec::new(self.exe.trim());
                spec.arguments = self.arguments.trim().to_owned();
                let dir = self.working_dir.trim();
                spec.working_dir = (!dir.is_empty()).then(|| PathBuf::from(dir));
                spec.new_console = self.new_console;
                self.title = format!("{} {}", spec.exe.display(), spec.arguments)
                    .trim_end()
                    .to_owned();
                TargetSpec::Launch(spec)
            }
        };
        let selection = Selection {
            layers: Some(
                self.layers
                    .iter()
                    .filter(|(_, on)| *on)
                    .map(|(l, _)| l.id.to_owned())
                    .collect(),
            ),
        };
        let config = SessionConfig {
            selection,
            suppress_nested: !self.keep_nested,
            ..SessionConfig::default()
        };
        let path = |kind: OutputKind| {
            self.outputs
                .iter()
                .find(|o| o.kind == kind && o.enabled)
                .map(|o| PathBuf::from(o.path.trim()))
        };
        let plan = OutputPlan {
            console: Console::Off,
            color: false,
            html: path(OutputKind::Html),
            xlsx: path(OutputKind::Xlsx),
            ndjson: path(OutputKind::Ndjson),
        };
        match Tracing::start(target, config, &plan, self.zone, self.title.clone(), ctx.clone()) {
            Ok(tracing) => {
                self.tracing = Some(tracing);
                self.entries.clear();
                self.selected = None;
                self.visible_key = None;
                self.messages.clear();
            }
            Err(message) => self.error(message),
        }
    }

    // ------------------------------------------------------------------------------------------------
    // 描画

    fn target_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_enabled_ui(!self.tracing(), |ui| {
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.mode, Mode::Attach, "動いているプロセスに付ける");
                ui.radio_value(&mut self.mode, Mode::Launch, "起動して最初から捕まえる");
            });
            match self.mode {
                Mode::Attach => self.attach_view(ui),
                Mode::Launch => self.launch_view(ui),
            }
            ui.separator();
            self.options_view(ui);
        });
        ui.separator();
        self.controls_view(ui);
        self.messages_view(ui);
    }

    fn attach_view(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("絞り込み");
            ui.add(egui::TextEdit::singleline(&mut self.process_filter).desired_width(200.0));
            if ui.button("一覧を更新").clicked() {
                self.refresh_processes();
            }
            let picker = ui
                .button("◎ 照準")
                .on_hover_text("押したまま、トレースしたいアプリのウィンドウの上へ動かして離します");
            if picker.is_pointer_button_down_on() && self.picking.is_none() {
                self.picking = Some(None);
            }
            match &self.picking {
                Some(Some(w)) => {
                    ui.label(format!(
                        "PID {}  {}",
                        w.pid,
                        if w.title.is_empty() { &w.class } else { &w.title }
                    ));
                }
                Some(None) => {
                    ui.label("トレースしたいウィンドウの上で離してください");
                }
                None => {}
            }
        });
        let needle = self.process_filter.to_lowercase();
        let rows: Vec<&ProcessInfo> = self
            .processes
            .iter()
            .filter(|p| {
                needle.is_empty() || p.name.to_lowercase().contains(&needle) || p.pid.to_string().contains(&needle)
            })
            .collect();
        let mut clicked = None;
        let mut double_clicked = None;
        TableBuilder::new(ui)
            .id_salt("processes")
            .striped(true)
            .sense(Sense::click())
            .max_scroll_height(170.0)
            .column(Column::exact(70.0))
            .column(Column::initial(220.0).resizable(true))
            .column(Column::exact(50.0))
            .column(Column::remainder())
            .header(20.0, |mut header| {
                for title in ["PID", "名前", "アーキ", "パス"] {
                    header.col(|ui| {
                        ui.strong(title);
                    });
                }
            })
            .body(|body| {
                body.rows(18.0, rows.len(), |mut row| {
                    let p = rows[row.index()];
                    row.set_selected(self.selected_pid == Some(p.pid));
                    // トレースできないもの（開けない、アーキテクチャが違う）は薄く出す。
                    let color = (!p.traceable()).then_some(Color32::GRAY);
                    let text = |s: String| match color {
                        Some(c) => RichText::new(s).color(c),
                        None => RichText::new(s),
                    };
                    row.col(|ui| {
                        ui.label(text(p.pid.to_string()));
                    });
                    row.col(|ui| {
                        ui.label(text(p.name.clone()));
                    });
                    row.col(|ui| {
                        ui.label(text(p.arch.map_or_else(|| "?".into(), |a| a.to_string())));
                    });
                    row.col(|ui| {
                        ui.label(text(
                            p.path
                                .clone()
                                .unwrap_or_else(|| "（開けません。管理者で実行すると見えます）".into()),
                        ));
                    });
                    let response = row.response();
                    if response.clicked() {
                        clicked = Some(p.pid);
                    }
                    if response.double_clicked() {
                        double_clicked = Some(p.pid);
                    }
                });
            });
        if let Some(pid) = clicked.or(double_clicked) {
            self.selected_pid = Some(pid);
        }
        if double_clicked.is_some() && self.can_start().is_ok() {
            let ctx = ui.ctx().clone();
            self.start(&ctx);
        }
    }

    fn launch_view(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("launch")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("実行ファイル");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.exe).desired_width(480.0));
                    if ui.button("参照…").clicked() {
                        let initial = (!self.exe.is_empty()).then(|| PathBuf::from(&self.exe));
                        if let Some(path) = sqltrace_win::open_file(
                            "起動する実行ファイル",
                            initial.as_deref(),
                            &[("実行ファイル", "*.exe"), ("すべてのファイル", "*.*")],
                        ) {
                            self.exe = path.display().to_string();
                        }
                    }
                });
                ui.end_row();
                ui.label("引数");
                ui.add(
                    egui::TextEdit::singleline(&mut self.arguments)
                        .desired_width(480.0)
                        .hint_text("コマンドラインに書くとおりに（例: -user scott \"C:\\My Data\\a.ini\"）"),
                );
                ui.end_row();
                ui.label("作業フォルダー");
                ui.add(
                    egui::TextEdit::singleline(&mut self.working_dir)
                        .desired_width(480.0)
                        .hint_text("空なら実行ファイルのあるフォルダー"),
                );
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut self.new_console, "コンソールのアプリは新しいコンソールで開く");
                ui.end_row();
            });
    }

    fn options_view(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("捕まえる層");
            for (layer, on) in &mut self.layers {
                ui.checkbox(on, layer.name);
            }
            ui.checkbox(&mut self.keep_nested, "内側の呼び出しも出す")
                .on_hover_text("ODBC ドライバの中から呼ばれた OCI なども出します（重複の除去を切る）");
        });
        egui::Grid::new("outputs")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                let suggested = self.suggested_name();
                for slot in &mut self.outputs {
                    ui.checkbox(&mut slot.enabled, slot.kind.label());
                    ui.horizontal(|ui| {
                        ui.add_enabled(
                            slot.enabled,
                            egui::TextEdit::singleline(&mut slot.path).desired_width(480.0),
                        );
                        if ui.button("参照…").clicked() {
                            let initial = if slot.path.is_empty() {
                                PathBuf::from(format!("{suggested}.{}", slot.kind.extension()))
                            } else {
                                PathBuf::from(&slot.path)
                            };
                            let pattern = format!("*.{}", slot.kind.extension());
                            if let Some(path) = sqltrace_win::save_file(
                                "保存先",
                                Some(&initial),
                                &[(slot.kind.label(), &pattern)],
                                slot.kind.extension(),
                            ) {
                                slot.path = path.display().to_string();
                                slot.enabled = true;
                            }
                        }
                    });
                    ui.end_row();
                }
            });
    }

    /// 保存先の名前の案（対象の名前）。
    fn suggested_name(&self) -> String {
        let name = match self.mode {
            Mode::Attach => self
                .selected_pid
                .and_then(|pid| self.processes.iter().find(|p| p.pid == pid))
                .map(|p| p.name.trim_end_matches(".exe").to_owned()),
            Mode::Launch => Path::new(self.exe.trim())
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned()),
        };
        format!("sqltrace-{}", name.unwrap_or_else(|| "trace".into()))
    }

    fn controls_view(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let ready = self.can_start();
            let start = ui.add_enabled(ready.is_ok(), egui::Button::new("▶ 開始"));
            if let Err(reason) = ready {
                if !self.tracing() {
                    start.clone().on_disabled_hover_text(reason);
                }
            }
            if start.clicked() {
                let ctx = ui.ctx().clone();
                self.start(&ctx);
            }
            if let Some(tracing) = &self.tracing {
                if ui
                    .button("■ 離れる")
                    .on_hover_text("ブレークポイントを外して離れます。対象は動き続けます")
                    .clicked()
                {
                    tracing.detach();
                }
                if ui.button("対象を終了させる").clicked() {
                    tracing.kill();
                }
                ui.label(RichText::new(format!("トレース中: {}", self.title)).color(Color32::from_rgb(0, 130, 60)));
            }
        });
    }

    fn messages_view(&mut self, ui: &mut egui::Ui) {
        // 最後の数件だけ（全体はログに残る）。
        for message in self.messages.iter().rev().take(3).rev() {
            match message {
                Message::Info(text) => ui.label(text),
                Message::Error(text) => ui.label(RichText::new(text).color(Color32::from_rgb(200, 40, 40))),
            };
        }
    }

    fn refresh_visible(&mut self) {
        let key = (self.filter.clone(), self.only_execute, self.entries.len());
        if self.visible_key.as_ref() == Some(&key) {
            return;
        }
        let needle = self.filter.to_lowercase();
        self.visible = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| match e {
                Entry::Sql(s) => Some((i, s)),
                _ => None,
            })
            .filter(|(_, s)| !self.only_execute || s.kind == Kind::Execute)
            .filter(|(_, s)| {
                needle.is_empty()
                    || s.sql.as_deref().is_some_and(|q| q.to_lowercase().contains(&needle))
                    || s.function.to_lowercase().contains(&needle)
                    || s.tid.to_string() == needle
                    || s.caller
                        .as_ref()
                        .is_some_and(|c| c.display().to_lowercase().contains(&needle))
            })
            .map(|(i, _)| i)
            .collect();
        self.visible_key = Some(key);
    }

    fn sql_view(&mut self, ui: &mut egui::Ui) {
        self.refresh_visible();
        let total = self.entries.iter().filter(|e| matches!(e, Entry::Sql(_))).count();
        ui.horizontal(|ui| {
            ui.label("絞り込み");
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .desired_width(260.0)
                    .hint_text("SQL、関数、呼び出し元、TID"),
            );
            ui.checkbox(&mut self.only_execute, "実行だけ");
            ui.checkbox(&mut self.follow, "最新を追う");
            ui.label(format!("{} / {} 件", self.visible.len(), total));
            if ui
                .button("消去")
                .on_hover_text("画面の表だけを消します（出力のファイルには残ります）")
                .clicked()
            {
                self.entries.retain(|e| !matches!(e, Entry::Sql(_)));
                self.selected = None;
                self.visible_key = None;
            }
        });
        let zone = self.zone;
        let entries = &self.entries;
        let visible = &self.visible;
        let selected = self.selected;
        let mut clicked = None;
        TableBuilder::new(ui)
            .id_salt("sql")
            .striped(true)
            .resizable(true)
            .sense(Sense::click())
            .stick_to_bottom(self.follow)
            .column(Column::exact(50.0))
            .column(Column::exact(90.0))
            .column(Column::exact(60.0))
            .column(Column::initial(90.0))
            .column(Column::initial(130.0))
            .column(Column::exact(40.0))
            .column(Column::initial(170.0))
            .column(Column::remainder().clip(true))
            .header(20.0, |mut header| {
                for title in ["#", "時刻", "TID", "層", "関数", "種別", "呼び出し元", "SQL"] {
                    header.col(|ui| {
                        ui.strong(title);
                    });
                }
            })
            .body(|body| {
                body.rows(18.0, visible.len(), |mut row| {
                    let index = visible[row.index()];
                    let Entry::Sql(s) = &entries[index] else {
                        return;
                    };
                    row.set_selected(selected == Some(index));
                    let number = row.index() + 1;
                    row.col(|ui| {
                        ui.label(number.to_string());
                    });
                    row.col(|ui| {
                        ui.label(zone.clock(s.time_ms));
                    });
                    row.col(|ui| {
                        ui.label(s.tid.to_string());
                    });
                    row.col(|ui| {
                        ui.label(&s.layer_name);
                    });
                    row.col(|ui| {
                        ui.label(&s.function);
                    });
                    row.col(|ui| {
                        ui.label(s.kind.label());
                    });
                    row.col(|ui| {
                        ui.label(s.caller.as_ref().map(|c| c.display()).unwrap_or_default());
                    });
                    row.col(|ui| {
                        // 表では 1 行目だけ。全文は下の欄に出す。
                        match &s.sql {
                            Some(sql) => {
                                let first = sql.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                                let more = sql.lines().filter(|l| !l.trim().is_empty()).count() > 1;
                                let line = if more { format!("{first} …") } else { first.to_owned() };
                                let job = sql_job(ui, &line, f32::INFINITY);
                                ui.label(job);
                            }
                            None => {
                                ui.label(
                                    RichText::new("（不明: このハンドルの準備を捕まえていない）").color(Color32::GRAY),
                                );
                            }
                        }
                    });
                    if row.response().clicked() {
                        clicked = Some(index);
                    }
                });
            });
        if clicked.is_some() {
            self.selected = clicked;
        }
    }

    fn detail_view(&mut self, ui: &mut egui::Ui) {
        let Some(Entry::Sql(s)) = self.selected.and_then(|i| self.entries.get(i)) else {
            ui.label(RichText::new("表の行を選ぶと、SQL の全文がここに出ます").color(Color32::GRAY));
            self.log_view(ui);
            return;
        };
        ui.horizontal(|ui| {
            ui.strong(format!(
                "{}  PID {}  TID {}  {}  {}  [{}]",
                self.zone.date_time(s.time_ms),
                s.pid,
                s.tid,
                s.layer_name,
                s.function,
                s.kind.label()
            ));
            if let Some(caller) = &s.caller {
                ui.label(format!("呼び出し元 {}", caller.display()));
            }
            if s.truncated {
                ui.label(RichText::new("長いので切り詰めました").color(Color32::from_rgb(160, 110, 0)));
            }
            if let Some(sql) = &s.sql {
                if ui.button("SQL をコピー").clicked() {
                    ui.ctx().copy_text(sql.clone());
                }
            }
        });
        let text = s
            .sql
            .clone()
            .unwrap_or_else(|| "（不明: このハンドルの準備を捕まえていない）".into());
        let highlight = s.sql.is_some();
        egui::ScrollArea::vertical().id_salt("detail").show(ui, |ui| {
            // 色分けして描く（SQL が分からないときの文はそのまま）。
            let mut layouter = |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, wrap_width: f32| {
                let job = if highlight {
                    sql_job(ui, buffer.as_str(), wrap_width)
                } else {
                    LayoutJob::simple(
                        buffer.as_str().to_owned(),
                        egui::TextStyle::Monospace.resolve(ui.style()),
                        Color32::GRAY,
                        wrap_width,
                    )
                };
                ui.ctx().fonts_mut(|fonts| fonts.layout_job(job))
            };
            // 読むだけ（選んでコピーはできる）。
            ui.add(
                egui::TextEdit::multiline(&mut text.as_str())
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY)
                    .layouter(&mut layouter),
            );
        });
        self.log_view(ui);
    }

    fn log_view(&self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("ログ（状態と警告）")
            .id_salt("log")
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("log_scroll")
                    .max_height(120.0)
                    .show(ui, |ui| {
                        for entry in &self.entries {
                            match entry {
                                Entry::Sql(_) => {}
                                Entry::Status { time_ms, message, .. } => {
                                    ui.label(format!("{}  {message}", self.zone.clock(*time_ms)));
                                }
                                Entry::Warning { time_ms, message, .. } => {
                                    ui.label(
                                        RichText::new(format!("{}  警告: {message}", self.zone.clock(*time_ms)))
                                            .color(Color32::from_rgb(160, 110, 0)),
                                    );
                                }
                            }
                        }
                    });
            });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        if std::mem::take(&mut self.start_on_launch) {
            match self.can_start() {
                Ok(()) => self.start(&ctx),
                Err(reason) => self.error(reason),
            }
        }
        egui::Panel::top("target").show(ui, |ui| self.target_panel(ui));
        egui::Panel::bottom("detail")
            .resizable(true)
            .default_size(180.0)
            .show(ui, |ui| self.detail_view(ui));
        egui::CentralPanel::default().show(ui, |ui| self.sql_view(ui));
    }
}

/// SQL を色分けした文字の並び（色は export の `highlight` で決める。暗い配色と明るい配色で使い分ける）。
fn sql_job(ui: &egui::Ui, sql: &str, wrap_width: f32) -> LayoutJob {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let default = ui.visuals().text_color();
    let dark = ui.visuals().dark_mode;
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width;
    for (kind, text) in pieces(sql) {
        let rgb = if dark { kind.dark() } else { kind.light() };
        let color = rgb.map_or(default, |c| Color32::from_rgb(c.0, c.1, c.2));
        job.append(
            text,
            0.0,
            TextFormat {
                font_id: font.clone(),
                color,
                // egui の TextFormat には太字がないので、キーワードは色だけで見分ける。
                italics: kind.italic(),
                ..TextFormat::default()
            },
        );
    }
    job
}
