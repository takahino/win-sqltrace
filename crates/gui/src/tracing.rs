//! 1 回のトレース: デバッグのスレッド（session）を動かし、出来事を画面と出力（Recorder）に渡す。
//!
//! 出来事はデバッグのスレッドで [`Entry`] にし、出力に書いてから画面のスレッドに送る。画面は送られた分を
//! 毎フレーム取り出すだけで、デバッグのスレッドを待たせない。

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use eframe::egui::Context;
use sqltrace_core::TargetSpec;
use sqltrace_export::{Entry, ExportError, OutputPlan, Recorder, TimeZone};
use sqltrace_session::{Session, SessionConfig, SessionError};

pub struct Tracing {
    session: Session,
    entries: Receiver<Entry>,
    recorder: Arc<Mutex<Option<Recorder>>>,
}

/// トレースが終わったときの結果。
pub struct Finished {
    pub trace: Result<(), SessionError>,
    pub written: Result<Vec<PathBuf>, ExportError>,
}

impl Tracing {
    pub fn start(
        target: TargetSpec,
        config: SessionConfig,
        plan: &OutputPlan,
        zone: TimeZone,
        title: String,
        ctx: Context,
    ) -> Result<Self, String> {
        // 出力先の誤りは、対象に触る前に知らせる。
        let recorder = Recorder::open(plan, zone, title).map_err(|e| e.to_string())?;
        let recorder = Arc::new(Mutex::new(Some(recorder)));
        let (sender, entries) = mpsc::channel();
        let shared = recorder.clone();
        let session = sqltrace_session::spawn(target, config, move |event| {
            let entry = Entry::from_event(&event, SystemTime::now());
            if let Some(recorder) = shared.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                recorder.record_entry(entry.clone());
            }
            // 画面が閉じられた後は送れないが、出力には書き続ける。
            let _ = sender.send(entry);
            ctx.request_repaint();
        })
        .map_err(|e| format!("デバッグのスレッドを作れません（{e}）"))?;
        Ok(Self {
            session,
            entries,
            recorder,
        })
    }

    /// 届いた分をすべて取り出す。
    pub fn drain(&self) -> Vec<Entry> {
        self.entries.try_iter().collect()
    }

    pub fn detach(&self) {
        self.session.detach();
    }

    pub fn kill(&self) {
        self.session.kill();
    }

    pub fn is_finished(&self) -> bool {
        self.session.is_finished()
    }

    /// デバッグのスレッドが終わるのを待ち、HTML と Excel を書く。
    pub fn finish(self) -> (Finished, Vec<Entry>) {
        let trace = self.session.join();
        // スレッドが終わった後に届いていた分も画面に渡す。
        let rest: Vec<Entry> = self.entries.try_iter().collect();
        let recorder = self.recorder.lock().unwrap_or_else(|e| e.into_inner()).take();
        let written = recorder.map_or_else(|| Ok(Vec::new()), Recorder::finish);
        (Finished { trace, written }, rest)
    }
}
