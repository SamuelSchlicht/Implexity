// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use implexity_core::registries::Registries;

pub const PRODUCER_INTERFACE: &str = "implexity-rust-extension/1:dynamic-frame-producer";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameProducer {
    pub description: &'static str,
}

#[must_use]
pub fn producers(registries: &Registries) -> Vec<String> {
    let mut out: Vec<String> = registries
        .providers
        .snapshot()
        .entries
        .iter()
        .filter(|(_, p)| {
            p.interface(PRODUCER_INTERFACE).is_some_and(|i| i.downcast_ref::<FrameProducer>().is_some())
        })
        .map(|(name, _)| name.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

#[must_use]
pub fn available(registries: &Registries) -> bool {
    registries.providers.snapshot().entries.iter().any(|(_, p)| {
        p.interface(PRODUCER_INTERFACE).is_some_and(|i| i.downcast_ref::<FrameProducer>().is_some())
    })
}

#[must_use]
pub fn active() -> bool {
    available(implexity_core::registries::global())
}

