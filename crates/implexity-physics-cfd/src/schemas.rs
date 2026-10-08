// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


pub const SCHEMAS: [implexity_core::schemas::SchemaDescriptor; 2] = [
    implexity_core::schemas::SchemaDescriptor { relative: "cfd/saddle_point_linear_solver.schema.json", text: include_str!("../schemas/cfd/saddle_point_linear_solver.schema.json") },
    implexity_core::schemas::SchemaDescriptor { relative: "cfd/saddle_point_linear_solver_stage1.schema.json", text: include_str!("../schemas/cfd/saddle_point_linear_solver_stage1.schema.json") },
];

pub fn get(relative: &str) -> Option<&'static str> {
    SCHEMAS.iter().find(|s| s.relative == relative).map(|s| s.text).or_else(|| implexity_core::schemas::get(relative))
}


pub fn parsed(relative: &str) -> Result<serde_json::Value, String> {
    let text = get(relative).ok_or_else(|| format!("no published schema {relative}"))?;
    implexity_core::json::parse_strict(text).map_err(|e| format!("schemas/{relative}: {e}"))
}
