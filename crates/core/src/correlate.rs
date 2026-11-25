//! 相関器: 読み取った呼び出し（[`ProbeHit`]）を、出力する出来事（[`TraceEvent`]）にする。
//!
//! - ハンドルごとに SQL を覚え、SQL を持たない Execute に SQL を付ける。
//! - DB の API の内側から呼ばれた分（odbc32 の A 系 → W 系、ODBC ドライバ → OCI など）は、外側で既に捕まえて
//!   いるので捨てる。判定は呼び出し元のモジュールで行う（[`ProbeSet::is_stack_module`]）。

use std::collections::HashMap;

use crate::capture::{ProbeHit, SqlText};
use crate::event::{SqlEvent, SqlKind, TraceEvent, Warning};
use crate::plan::ProbeSet;
use crate::probe::Role;

/// 覚えておくハンドルの上限。解放（`SQLFreeHandle` など）はまだ捕まえていないので、使い捨てのハンドルが
/// 溜まり続けないように、超えたら一度すべて忘れる（その直後の Execute だけ SQL が分からなくなる）。
const MAX_STATEMENTS: usize = 100_000;

#[derive(Debug)]
struct Statement {
    sql: String,
    truncated: bool,
    /// 実行済み。DB-Library では実行の後の `dbcmd` が新しい文を始める。
    executed: bool,
}

pub struct Correlator {
    probes: ProbeSet,
    /// 内側の呼び出しを捨てるか（重複の除去）。調べるときは切れるようにする。
    suppress_nested: bool,
    statements: HashMap<(u32, u64), Statement>,
}

impl Correlator {
    pub fn new(probes: ProbeSet, suppress_nested: bool) -> Self {
        Self {
            probes,
            suppress_nested,
            statements: HashMap::new(),
        }
    }

    pub fn process(&mut self, hit: ProbeHit) -> Vec<TraceEvent> {
        if self.suppress_nested && self.is_nested(&hit) {
            return Vec::new();
        }
        let mut events = Vec::new();
        let text = match hit.sql {
            SqlText::Text(ref t) => Some(t.clone()),
            SqlText::Absent => None,
            SqlText::Unreadable(ref error) => {
                events.push(TraceEvent::Warning(Warning {
                    pid: Some(hit.pid),
                    message: format!("{}: {error}", hit.probe.function),
                }));
                None
            }
        };
        let key = hit.handle.map(|h| (hit.pid, h));
        let (kind, sql, truncated) = match hit.probe.role {
            Role::ExecDirect => (
                SqlKind::Execute,
                text.as_ref().map(|t| t.value.clone()),
                text.as_ref().is_some_and(|t| t.truncated),
            ),
            Role::Prepare => {
                match (key, &text) {
                    (Some(key), Some(t)) => self.remember(
                        key,
                        Statement {
                            sql: t.value.clone(),
                            truncated: t.truncated,
                            executed: false,
                        },
                    ),
                    // 読めなかった準備で、前の SQL を付けて Execute を出さないように忘れる。
                    (Some(key), None) => {
                        self.statements.remove(&key);
                    }
                    _ => {}
                }
                (
                    SqlKind::Prepare,
                    text.as_ref().map(|t| t.value.clone()),
                    text.as_ref().is_some_and(|t| t.truncated),
                )
            }
            Role::Append => {
                if let (Some(key), Some(t)) = (key, &text) {
                    match self.statements.get_mut(&key) {
                        Some(s) if !s.executed => {
                            s.sql.push_str(&t.value);
                            s.truncated |= t.truncated;
                        }
                        _ => self.remember(
                            key,
                            Statement {
                                sql: t.value.clone(),
                                truncated: t.truncated,
                                executed: false,
                            },
                        ),
                    }
                }
                // 足している途中の断片は出さない（実行時にまとめて出す）。
                return events;
            }
            Role::Execute => match key.and_then(|k| self.statements.get_mut(&k)) {
                Some(s) => {
                    s.executed = true;
                    (SqlKind::Execute, Some(s.sql.clone()), s.truncated)
                }
                None => (SqlKind::Execute, None, false),
            },
        };
        events.push(TraceEvent::Sql(SqlEvent {
            time: hit.time,
            pid: hit.pid,
            tid: hit.tid,
            layer: hit.probe.layer,
            function: hit.probe.function,
            kind,
            sql,
            truncated,
            handle: hit.handle,
            caller: hit.caller,
        }));
        events
    }

    /// プロセスが終わったら、そのハンドルを忘れる（同じ値のハンドルが別のプロセスで使われるため）。
    pub fn forget_process(&mut self, pid: u32) {
        self.statements.retain(|(p, _), _| *p != pid);
    }

    fn is_nested(&self, hit: &ProbeHit) -> bool {
        let module = hit.caller.as_ref().and_then(|c| c.module.as_deref());
        module.is_some_and(|m| self.probes.is_stack_module(m))
    }

    fn remember(&mut self, key: (u32, u64), statement: Statement) {
        if self.statements.len() >= MAX_STATEMENTS && !self.statements.contains_key(&key) {
            self.statements.clear();
        }
        self.statements.insert(key, statement);
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;
    use crate::capture::CallerInfo;
    use crate::plan::tests::{CATALOG, ODBC};
    use crate::plan::Selection;
    use crate::probe::{Layer, ProbeDef};
    use crate::text::Text;

    fn def(role: Role, function: &'static str) -> &'static ProbeDef {
        Box::leak(Box::new(ProbeDef {
            layer: &ODBC,
            modules: &["odbc32.dll"],
            function,
            role,
            sql: None,
            handle: None,
            skip_if: None,
        }))
    }

    fn hit(probe: &'static ProbeDef, sql: Option<&str>, handle: Option<u64>, caller: Option<&str>) -> ProbeHit {
        ProbeHit {
            time: SystemTime::UNIX_EPOCH,
            pid: 10,
            tid: 20,
            probe,
            sql: sql.map_or(SqlText::Absent, |s| {
                SqlText::Text(Text {
                    value: s.into(),
                    truncated: false,
                })
            }),
            handle,
            caller: caller.map(|m| CallerInfo {
                address: 0x1234,
                module: Some(m.into()),
                offset: 0x34,
            }),
        }
    }

    fn correlator() -> Correlator {
        Correlator::new(ProbeSet::plan(&CATALOG, &Selection::default()).unwrap(), true)
    }

    fn sql_of(events: &[TraceEvent]) -> Vec<(SqlKind, Option<String>)> {
        events
            .iter()
            .filter_map(|e| match e {
                TraceEvent::Sql(s) => Some((s.kind, s.sql.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn execute_gets_sql_from_prepare_on_same_handle() {
        let mut c = correlator();
        let prepare = def(Role::Prepare, "SQLPrepareW");
        let execute = def(Role::Execute, "SQLExecute");
        assert_eq!(
            sql_of(&c.process(hit(prepare, Some("SELECT 1"), Some(7), None))),
            [(SqlKind::Prepare, Some("SELECT 1".into()))]
        );
        assert_eq!(
            sql_of(&c.process(hit(execute, None, Some(7), None))),
            [(SqlKind::Execute, Some("SELECT 1".into()))]
        );
        // 再実行しても同じ SQL。
        assert_eq!(
            sql_of(&c.process(hit(execute, None, Some(7), None))),
            [(SqlKind::Execute, Some("SELECT 1".into()))]
        );
        // 知らないハンドルは SQL なしで出す（準備がトレース開始前だった）。
        assert_eq!(
            sql_of(&c.process(hit(execute, None, Some(8), None))),
            [(SqlKind::Execute, None)]
        );
    }

    #[test]
    fn append_accumulates_until_execute_then_starts_over() {
        let mut c = correlator();
        let append = def(Role::Append, "dbcmd");
        let execute = def(Role::Execute, "dbsqlexec");
        assert!(c.process(hit(append, Some("SELECT "), Some(1), None)).is_empty());
        assert!(c.process(hit(append, Some("1"), Some(1), None)).is_empty());
        assert_eq!(
            sql_of(&c.process(hit(execute, None, Some(1), None))),
            [(SqlKind::Execute, Some("SELECT 1".into()))]
        );
        c.process(hit(append, Some("SELECT 2"), Some(1), None));
        assert_eq!(
            sql_of(&c.process(hit(execute, None, Some(1), None))),
            [(SqlKind::Execute, Some("SELECT 2".into()))]
        );
    }

    #[test]
    fn calls_from_inside_the_db_stack_are_dropped() {
        let mut c = correlator();
        let exec = def(Role::ExecDirect, "SQLExecDirectW");
        assert!(c.process(hit(exec, Some("X"), None, Some("ODBC32.dll"))).is_empty());
        assert!(c.process(hit(exec, Some("X"), None, Some("sqora32.dll"))).is_empty());
        assert_eq!(c.process(hit(exec, Some("X"), None, Some("app.exe"))).len(), 1);
        // 呼び出し元が分からなければ残す。
        assert_eq!(c.process(hit(exec, Some("X"), None, None)).len(), 1);
    }

    #[test]
    fn suppression_can_be_turned_off() {
        let mut c = Correlator::new(ProbeSet::plan(&CATALOG, &Selection::default()).unwrap(), false);
        let exec = def(Role::ExecDirect, "SQLExecDirectW");
        assert_eq!(c.process(hit(exec, Some("X"), None, Some("odbc32.dll"))).len(), 1);
    }

    #[test]
    fn unreadable_sql_becomes_a_warning_plus_event() {
        static OTHER: Layer = Layer {
            id: "x",
            name: "X",
            related_modules: &[],
        };
        let probe: &'static ProbeDef = Box::leak(Box::new(ProbeDef {
            layer: &OTHER,
            modules: &[],
            function: "F",
            role: Role::ExecDirect,
            sql: None,
            handle: None,
            skip_if: None,
        }));
        let mut c = correlator();
        let mut h = hit(probe, None, None, None);
        h.sql = SqlText::Unreadable(crate::TextError::Unreadable { address: 0x10 });
        let events = c.process(h);
        assert!(matches!(events[0], TraceEvent::Warning(_)));
        assert_eq!(sql_of(&events), [(SqlKind::Execute, None)]);
    }
}
