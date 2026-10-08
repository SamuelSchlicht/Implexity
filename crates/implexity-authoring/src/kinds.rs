// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KindShape {
    pub arity: Option<usize>,
    pub structural: Vec<String>,
    pub init: Option<Vec<String>>,
}

fn table() -> &'static BTreeMap<String, KindShape> {
    static TABLE: OnceLock<BTreeMap<String, KindShape>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let raw: Value =
            serde_json::from_str(include_str!("../data/kind_shapes.json")).unwrap_or(Value::Null);
        let mut out = BTreeMap::new();
        if let Some(kinds) = raw.get("kinds").and_then(Value::as_object) {
            for (kind, row) in kinds {
                let strings = |v: Option<&Value>| -> Option<Vec<String>> {
                    v.and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(|x| x.as_str().map(ToString::to_string)).collect())
                };
                out.insert(
                    kind.clone(),
                    KindShape {
                        arity: row.get("arity").and_then(Value::as_u64).map(|a| a as usize),
                        structural: strings(row.get("struct")).unwrap_or_default(),
                        init: strings(row.get("init")),
                    },
                );
            }
        }
        out
    })
}

#[must_use]
pub fn shape(kind: &str) -> KindShape {
    table().get(kind).cloned().unwrap_or_default()
}

#[must_use]
pub fn recorded(kind: &str) -> bool {
    table().contains_key(kind)
}
