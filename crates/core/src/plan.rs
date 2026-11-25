//! 何を捕まえるかを決める（純粋な処理）。
//!
//! 起動モードでは事前に対象を調べられず、DLL も後から読み込まれる。そのためここでは層の絞り込みだけを決め、
//! アーキテクチャでの絞り込みはプロセスの生成時、関数の解決は DLL の読み込み時にデバッグ層が行う。

use std::fmt;

use crate::pattern;
use crate::probe::{Catalog, Layer, ProbeDef};

/// 利用者の指定。`None` はすべての層。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub layers: Option<Vec<String>>,
}

impl Selection {
    /// `odbc,oci` のようなカンマ区切り。空の項目は無視する。
    pub fn parse(spec: &str) -> Self {
        let layers = spec
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        Self { layers: Some(layers) }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanError {
    UnknownLayer { given: String, known: Vec<&'static str> },
    Empty,
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanError::UnknownLayer { given, known } => {
                write!(f, "層 '{given}' はありません。指定できるのは {} です", known.join(", "))
            }
            PlanError::Empty => f.write_str("捕まえる層が 1 つも指定されていません。層を 1 つ以上指定してください"),
        }
    }
}

impl std::error::Error for PlanError {}

/// 選んだ層の定義。デバッグ層と相関器はこれだけを見る。
#[derive(Clone, Debug)]
pub struct ProbeSet {
    layers: Vec<&'static Layer>,
    probes: Vec<&'static ProbeDef>,
}

impl ProbeSet {
    pub fn plan(catalog: &Catalog, selection: &Selection) -> Result<Self, PlanError> {
        let layers: Vec<&'static Layer> =
            match &selection.layers {
                None => catalog.layers.to_vec(),
                Some(ids) => {
                    let mut chosen = Vec::new();
                    for id in ids {
                        let layer = catalog.layers.iter().copied().find(|l| l.id == id).ok_or_else(|| {
                            PlanError::UnknownLayer {
                                given: id.clone(),
                                known: catalog.layers.iter().map(|l| l.id).collect(),
                            }
                        })?;
                        if !chosen.contains(&layer) {
                            chosen.push(layer);
                        }
                    }
                    chosen
                }
            };
        if layers.is_empty() {
            return Err(PlanError::Empty);
        }
        let probes = catalog.probes.iter().filter(|p| layers.contains(&p.layer)).collect();
        Ok(Self { layers, probes })
    }

    pub fn layers(&self) -> &[&'static Layer] {
        &self.layers
    }

    pub fn probes(&self) -> &[&'static ProbeDef] {
        &self.probes
    }

    /// そのモジュール（ファイル名）で探す定義と、[`Self::probes`] での位置（デバッグ層に渡す目印にする）。
    pub fn for_module<'a>(&'a self, module: &'a str) -> impl Iterator<Item = (usize, &'static ProbeDef)> + 'a {
        self.probes
            .iter()
            .copied()
            .enumerate()
            .filter(move |(_, p)| p.modules.iter().any(|pat| pattern::matches(pat, module)))
    }

    /// 選んだ層に属するモジュールか（フックするものと、ドライバなどフックしないもの）。
    /// ここから呼ばれた分は、外側で既に捕まえた呼び出しの内側とみなす。
    pub fn is_stack_module(&self, module: &str) -> bool {
        let hooked = self.probes.iter().flat_map(|p| p.modules.iter());
        let related = self.layers.iter().flat_map(|l| l.related_modules.iter());
        hooked.chain(related).any(|pat| pattern::matches(pat, module))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::probe::Role;

    pub(crate) static ODBC: Layer = Layer {
        id: "odbc",
        name: "ODBC",
        related_modules: &["sqora*.dll"],
    };
    pub(crate) static OCI: Layer = Layer {
        id: "oci",
        name: "OCI",
        related_modules: &[],
    };

    const fn probe(
        layer: &'static Layer,
        modules: &'static [&'static str],
        function: &'static str,
        role: Role,
    ) -> ProbeDef {
        ProbeDef {
            layer,
            modules,
            function,
            role,
            sql: None,
            handle: None,
            skip_if: None,
        }
    }

    pub(crate) static PROBES: [ProbeDef; 2] = [
        probe(&ODBC, &["odbc32.dll"], "SQLExecDirectW", Role::ExecDirect),
        probe(&OCI, &["oci.dll"], "OCIStmtPrepare", Role::Prepare),
    ];
    pub(crate) static CATALOG: Catalog = Catalog {
        layers: &[&ODBC, &OCI],
        probes: &PROBES,
    };

    #[test]
    fn all_layers_by_default() {
        let set = ProbeSet::plan(&CATALOG, &Selection::default()).unwrap();
        assert_eq!(set.probes().len(), 2);
    }

    #[test]
    fn selection_filters_probes_and_stack_modules() {
        let set = ProbeSet::plan(&CATALOG, &Selection::parse(" OCI ,")).unwrap();
        assert_eq!(
            set.probes().iter().map(|p| p.function).collect::<Vec<_>>(),
            ["OCIStmtPrepare"]
        );
        // ODBC を選ばなければ、ODBC ドライバからの OCI 呼び出しは内側ではない（捨てると何も残らない）。
        assert!(!set.is_stack_module("SQORA32.DLL"));
        assert!(set.is_stack_module("OCI.DLL"));
        assert_eq!(set.for_module("Oci.dll").count(), 1);
        assert_eq!(set.for_module("odbc32.dll").count(), 0);
    }

    #[test]
    fn related_modules_count_as_stack_when_layer_selected() {
        let set = ProbeSet::plan(&CATALOG, &Selection::default()).unwrap();
        assert!(set.is_stack_module("sqora32.dll"));
        assert!(!set.is_stack_module("app.exe"));
    }

    #[test]
    fn unknown_or_empty_selection_is_an_error() {
        let unknown = ProbeSet::plan(&CATALOG, &Selection::parse("odbc,jdbc")).unwrap_err();
        assert_eq!(
            unknown,
            PlanError::UnknownLayer {
                given: "jdbc".into(),
                known: vec!["odbc", "oci"]
            }
        );
        assert_eq!(
            ProbeSet::plan(&CATALOG, &Selection::parse(",")).unwrap_err(),
            PlanError::Empty
        );
    }
}
