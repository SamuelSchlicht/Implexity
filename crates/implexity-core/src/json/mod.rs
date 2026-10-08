// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




mod strict;
mod write;

pub use strict::{
    JsonError, MAX_DEPTH, ParseOptions, parse_strict, parse_strict_bytes, parse_with, parse_with_depth,
};
pub use write::{
    DumpOptions, canonical, canonical_sha256, dumps, number_text, sha256_hex, sha256_of, write_string,
};


pub fn read_file(path: &std::path::Path) -> Result<serde_json::Value, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    parse_strict_bytes(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}
