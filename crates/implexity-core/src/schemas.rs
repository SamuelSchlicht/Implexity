// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


pub const SCHEMAS: [(&str, &str); 9] = [
    ("agent/action.schema.json", include_str!("../schemas/agent/action.schema.json")),
    ("agent/engineering_intent.schema.json", include_str!("../schemas/agent/engineering_intent.schema.json")),
    ("execution_plan.schema.json", include_str!("../schemas/execution_plan.schema.json")),
    (
        "cae/coupled_convergence_v2.schema.json",
        include_str!("../schemas/cae/coupled_convergence_v2.schema.json"),
    ),
    (
        "cae/provider_block_preconditioner.schema.json",
        include_str!("../schemas/cae/provider_block_preconditioner.schema.json"),
    ),
    (
        "cae/provider_block_preconditioner_v40.schema.json",
        include_str!("../schemas/cae/provider_block_preconditioner_v40.schema.json"),
    ),
    ("physics_addin.schema.json", include_str!("../schemas/physics_addin.schema.json")),
    ("physics_addin_v32.schema.json", include_str!("../schemas/physics_addin_v32.schema.json")),
    ("problem.schema.json", include_str!("../schemas/problem.schema.json")),
];

#[must_use]
pub fn get(relative: &str) -> Option<&'static str> {
    SCHEMAS.iter().find(|(p, _)| *p == relative).map(|(_, t)| *t)
}


pub fn parsed(relative: &str) -> Result<serde_json::Value, String> {
    let text = get(relative).ok_or_else(|| format!("no published schema {relative}"))?;
    crate::json::parse_strict(text).map_err(|e| format!("schemas/{relative}: {e}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchemaDescriptor {
    pub relative: &'static str,
    pub text: &'static str,
}


pub fn get_registered(relative: &str, providers: &crate::providers::ProviderRegistry) -> Result<Option<&'static str>, String> {
    let mut found = get(relative);
    for (_, provider) in providers.snapshot().entries {
        for schema in provider.published_schemas() {
            if schema.relative == relative {
                if found.is_some_and(|text| text != schema.text) {
                    return Err(format!("conflicting published schema {relative}"));
                }
                found = Some(schema.text);
            }
        }
    }
    Ok(found)
}


pub fn parsed_registered(relative: &str, providers: &crate::providers::ProviderRegistry) -> Result<serde_json::Value, String> {
    let text = get_registered(relative, providers)?.ok_or_else(|| format!("no published schema {relative}"))?;
    crate::json::parse_strict(text).map_err(|e| format!("schemas/{relative}: {e}"))
}
