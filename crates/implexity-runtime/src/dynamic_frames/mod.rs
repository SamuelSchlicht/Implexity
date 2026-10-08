// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


pub mod analysis;
pub mod availability;
pub mod capture;
pub mod catalogue;
pub mod import;
pub mod manifest;
pub mod store;

pub use manifest::{
    FieldKind, FieldSpec, FrameManifest, GridSpec, PaletteHint, Precision, Retention, SeriesSpec, TimeBase,
};
pub use store::{DynamicStore, FrameEntry, FrameOutcome, FrameWriter, SeriesTable, StoreSummary};

pub const SCHEMA: &str = "implexity-dynamic-frames/1";
pub const STATUS_SCHEMA: &str = "implexity-dynamic-frames-status/1";
pub const EXTENSION: &str = "implexity-rust-extension/1";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DynamicError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
    Corrupt(String),
    #[error("{0}")]
    Bound(String),
}

impl DynamicError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }

    pub(crate) fn io(what: &str, path: &std::path::Path, e: &dyn std::fmt::Display) -> Self {
        Self::Io(format!("dynamic frame store {what} failed for {}: {e}", path.display()))
    }
}

impl From<DynamicError> for implexity_core::CaeError {
    fn from(e: DynamicError) -> Self {
        Self::contract(e.to_string())
    }
}

pub type DynamicResult<T> = Result<T, DynamicError>;

#[must_use]
pub fn is_identifier(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes.iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}
