//! sqlTrace のドメイン型と、OS に依存しない処理。
//!
//! 流れ: デバッグ層（`win`）が API の入口で止めたスレッドのレジスタとメモリを [`CallSite`] として渡す →
//! [`capture`] が [`ProbeDef`] に従って引数を読み [`ProbeHit`] にする → [`Correlator`] がハンドルと SQL を
//! 対応付けて [`TraceEvent`] にする。何を捕まえるかは `catalog` crate がデータで持ち、[`ProbeSet`] が絞り込む。
//!
//! この crate は Windows の API に触れない。`tools/check.ps1` が Linux ターゲットでも check して、それを確かめる。

pub mod abi;
pub mod arch;
pub mod capture;
pub mod correlate;
pub mod event;
pub mod memory;
pub mod pattern;
pub mod pe;
pub mod plan;
pub mod probe;
pub mod prologue;
pub mod target;
pub mod text;

pub use abi::{Abi, CallSite, Registers};
pub use arch::Arch;
pub use capture::{capture, CallerInfo, HitContext, ProbeHit, SqlText};
pub use correlate::Correlator;
pub use event::{SqlEvent, SqlKind, Status, TraceEvent, Warning};
pub use memory::{BufferMemory, Memory};
pub use plan::{PlanError, ProbeSet, Selection};
pub use probe::{Catalog, Encoding, Layer, LengthArg, ProbeDef, Role, ScalarArg, SkipIf, TextArg, Unit, Width};
pub use target::{LaunchSpec, TargetSpec};
pub use text::{CodepageDecoder, Text, TextError, TextOptions};
