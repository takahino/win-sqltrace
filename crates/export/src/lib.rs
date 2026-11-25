//! sqlTrace の出力。OS に依存しない（ファイルとコンソールは標準ライブラリで書く）。
//!
//! - コンソールと NDJSON は 1 件ずつすぐ書く（止める前に強制終了されても、そこまでは残る）。
//! - HTML と Excel は表全体を作るので、トレースの終わりにまとめて書く。
//! - NDJSON に保存しておけば、後から [`export_saved`] で HTML や Excel に書き出せる。

pub mod console;
pub mod entry;
pub mod highlight;
pub mod html;
pub mod ndjson;
pub mod time;
pub mod xlsx;

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub use console::ConsoleWriter;
pub use entry::{Caller, Entry, Kind, SqlEntry};
pub use html::HtmlReport;
pub use ndjson::NdjsonWriter;
pub use time::TimeZone;
pub use xlsx::XlsxReport;

use sqltrace_core::TraceEvent;

/// コンソールに何を出すか。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Console {
    Off,
    /// 状態と警告だけ（標準エラー）。
    StatusOnly,
    /// SQL を標準出力に、状態と警告を標準エラーに出す。
    #[default]
    All,
}

/// どこに出すか。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputPlan {
    pub console: Console,
    /// コンソールの SQL を ANSI の色で色分けする（呼び出し側が、端末に出すときだけ立てる）。
    pub color: bool,
    pub ndjson: Option<PathBuf>,
    pub html: Option<PathBuf>,
    pub xlsx: Option<PathBuf>,
}

#[derive(Debug)]
pub enum ExportError {
    Create {
        path: PathBuf,
        source: io::Error,
    },
    Write {
        path: PathBuf,
        source: io::Error,
    },
    Xlsx {
        path: PathBuf,
        source: rust_xlsxwriter::XlsxError,
    },
    Read {
        path: PathBuf,
        message: String,
    },
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExportError::Create { path, source } => write!(
                f,
                "{} を作れません（{source}）。フォルダーがあるか、ほかのアプリ（Excel など）で開いていないかを確かめてください",
                path.display()
            ),
            ExportError::Write { path, source } => {
                write!(f, "{} に書けません（{source}）。空き容量を確かめてください", path.display())
            }
            ExportError::Xlsx { path, source } => write!(f, "{} の Excel を作れません（{source}）", path.display()),
            ExportError::Read { path, message } => write!(f, "{} を読めません: {message}", path.display()),
        }
    }
}

impl std::error::Error for ExportError {}

fn create(path: &Path) -> Result<File, ExportError> {
    File::create(path).map_err(|source| ExportError::Create {
        path: path.to_owned(),
        source,
    })
}

/// トレースの出来事を受け取り、[`OutputPlan`] のとおりに出す。
pub struct Recorder {
    zone: TimeZone,
    title: String,
    console: Option<ConsoleWriter<io::Stdout, io::Stderr>>,
    ndjson: Option<(PathBuf, NdjsonWriter<BufWriter<File>>)>,
    /// HTML と Excel のファイルは最初に作っておく（パスの誤りをトレースの前に分かるようにする）。
    html: Option<(PathBuf, File)>,
    xlsx: Option<(PathBuf, File)>,
    entries: Vec<Entry>,
    /// 書けなかった誤り（最初の 1 つ）。トレースは止めず、終わりに返す。
    failure: Option<ExportError>,
}

impl Recorder {
    /// `title` は報告の見出し（何をトレースしたか）。
    pub fn open(plan: &OutputPlan, zone: TimeZone, title: impl Into<String>) -> Result<Self, ExportError> {
        let ndjson = match &plan.ndjson {
            Some(path) => {
                let writer = NdjsonWriter::new(BufWriter::new(create(path)?)).map_err(|source| ExportError::Write {
                    path: path.clone(),
                    source,
                })?;
                Some((path.clone(), writer))
            }
            None => None,
        };
        let html = plan
            .html
            .as_ref()
            .map(|p| create(p).map(|f| (p.clone(), f)))
            .transpose()?;
        let xlsx = plan
            .xlsx
            .as_ref()
            .map(|p| create(p).map(|f| (p.clone(), f)))
            .transpose()?;
        Ok(Self {
            zone,
            title: title.into(),
            console: (plan.console != Console::Off).then(|| {
                ConsoleWriter::new(io::stdout(), io::stderr(), zone)
                    .show_sql(plan.console == Console::All)
                    .color(plan.color)
            }),
            ndjson,
            html,
            xlsx,
            entries: Vec::new(),
            failure: None,
        })
    }

    pub fn record(&mut self, event: &TraceEvent) {
        self.record_entry(Entry::from_event(event, SystemTime::now()));
    }

    pub fn record_entry(&mut self, entry: Entry) {
        if let Some(console) = &mut self.console {
            // コンソールに書けない（パイプが閉じた）のは、トレースを止める理由にしない。
            let _ = console.write(&entry);
        }
        if let Some((path, writer)) = &mut self.ndjson {
            if let Err(source) = writer.write(&entry) {
                if self.failure.is_none() {
                    self.failure = Some(ExportError::Write {
                        path: path.clone(),
                        source,
                    });
                }
                self.ndjson = None;
            }
        }
        if self.html.is_some() || self.xlsx.is_some() {
            self.entries.push(entry);
        }
    }

    /// HTML と Excel を書き、書いたファイルを返す。
    pub fn finish(self) -> Result<Vec<PathBuf>, ExportError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let mut written = Vec::new();
        if let Some((path, _)) = self.ndjson {
            written.push(path);
        }
        if let Some((path, file)) = self.html {
            let report = HtmlReport {
                title: &self.title,
                zone: self.zone,
            };
            report
                .write(BufWriter::new(file), &self.entries)
                .map_err(|source| ExportError::Write {
                    path: path.clone(),
                    source,
                })?;
            written.push(path);
        }
        if let Some((path, mut file)) = self.xlsx {
            let report = XlsxReport {
                title: &self.title,
                zone: self.zone,
            };
            let bytes = report.to_bytes(&self.entries).map_err(|source| ExportError::Xlsx {
                path: path.clone(),
                source,
            })?;
            file.write_all(&bytes).map_err(|source| ExportError::Write {
                path: path.clone(),
                source,
            })?;
            written.push(path);
        }
        Ok(written)
    }
}

/// 保存した NDJSON を読み、`plan` のとおりに書き出す（コンソールにも出せる）。
pub fn export_saved(input: &Path, plan: &OutputPlan, zone: TimeZone) -> Result<Vec<PathBuf>, ExportError> {
    let file = File::open(input).map_err(|e| ExportError::Read {
        path: input.to_owned(),
        message: e.to_string(),
    })?;
    let entries = ndjson::read(BufReader::new(file)).map_err(|e| ExportError::Read {
        path: input.to_owned(),
        message: e.to_string(),
    })?;
    let title = input
        .file_name()
        .map_or_else(|| input.display().to_string(), |n| n.to_string_lossy().into_owned());
    let mut recorder = Recorder::open(plan, zone, title)?;
    for entry in entries {
        recorder.record_entry(entry);
    }
    recorder.finish()
}
