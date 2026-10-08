// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::{Arc, LazyLock};

use crate::contracts::CaeProvider;
use crate::contributions::{ContributionRegistry, ContributionSnapshot, ContributionToken};
use crate::error::{CaeError, CaeResult};
use crate::extensions::{ExtensionRegistry, ExtensionRegistrySnapshot, ExtensionRegistryToken};
use crate::orchestration::{AddInRegistry, AddInRegistrySnapshot, RegistryBindingToken};
use crate::providers::{ProviderRegistry, ProviderRegistrySnapshot, ProviderRegistryToken};
use crate::sufficiency::{SufficiencyRegistry, SufficiencyRegistrySnapshot, SufficiencyRegistryToken};

#[derive(Debug, Default)]
pub struct Registries {
    pub providers: ProviderRegistry,
    pub addins: AddInRegistry,
    pub sufficiency: SufficiencyRegistry,
    pub extensions: ExtensionRegistry,
    pub contributions: ContributionRegistry,
}

#[derive(Debug, Clone)]
pub struct RegistryState {
    pub providers: ProviderRegistrySnapshot,
    pub addins: AddInRegistrySnapshot,
    pub sufficiency: SufficiencyRegistrySnapshot,
    pub extensions: ExtensionRegistrySnapshot,
    pub contributions: ContributionSnapshot,
}

impl std::fmt::Debug for ExtensionRegistrySnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionRegistrySnapshot").field("token", &self.token).finish_non_exhaustive()
    }
}

pub type BindingTokens = (
    ProviderRegistryToken,
    RegistryBindingToken,
    SufficiencyRegistryToken,
    ExtensionRegistryToken,
    ContributionToken,
);

impl RegistryState {
    #[must_use]
    pub fn binding_tokens(&self) -> BindingTokens {
        (
            self.providers.token.clone(),
            self.addins.token.clone(),
            self.sufficiency.token.clone(),
            self.extensions.token.clone(),
            self.contributions.token.clone(),
        )
    }
}

impl Registries {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn state(&self) -> RegistryState {
        RegistryState {
            providers: self.providers.snapshot(),
            addins: self.addins.snapshot(),
            sufficiency: self.sufficiency.snapshot(),
            extensions: self.extensions.snapshot(),
            contributions: self.contributions.snapshot(),
        }
    }


    pub fn register_provider(&self, provider: Arc<dyn CaeProvider>) -> CaeResult<Arc<dyn CaeProvider>> {
        self.providers.register(&self.addins, provider)
    }


    pub fn transaction<T>(&self, body: impl FnOnce() -> CaeResult<T>) -> CaeResult<T> {
        self.providers.transaction(&self.addins, || {
            self.sufficiency
                .transaction(|| self.extensions.transaction(|| self.contributions.transaction(body)))
        })
    }


    pub fn restore(&self, state: &RegistryState) -> CaeResult<()> {
        self.providers.restore(&state.providers);
        self.addins.restore(&state.addins)?;
        self.sufficiency.restore(&state.sufficiency);
        self.extensions.restore(&state.extensions)?;
        self.contributions.restore(&state.contributions).map_err(|e| CaeError::contract(e.0))
    }
}

static GLOBAL: LazyLock<Registries> = LazyLock::new(Registries::new);

#[must_use]
pub fn global() -> &'static Registries {
    &GLOBAL
}
