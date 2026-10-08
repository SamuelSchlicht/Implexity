// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use serde_json::{Map, Value, json};

use crate::capability_status;
use crate::contracts::CaeProvider;
use crate::contributions::{Contribution, ContributionValue};
use crate::distributions::DistributionSet;
use crate::error::{CaeError, CaeResult};
use crate::extensions::{
    CaseBuilderFn, CouplingRule, MetricSpec, MonitorFn, RegisteredCaseAuthoring, RegisteredRegimeMonitor,
};
use crate::json::canonical_sha256;
use crate::orchestration::{
    AddInAdapter, AddInContract, ContractInput, RegisteredAddIn, numerical_owner, stable_implementation,
};
use crate::package_catalog::{PackageDescriptor, load_distribution_catalog};
use crate::py_repr::repr_str;
use crate::registries::{Registries, RegistryState};
use crate::sufficiency::SufficiencyRule;
use crate::sync::{ReLock, lock};

pub const OWNER_VERSION: u32 = 1;
pub const STATUS_SCHEMA: &str = "implexity-physics-packages/2";
pub const MANIFEST_SCHEMA: &str = "implexity-physics-package-manifest/2";
pub const DEACTIVATION_TEXT: &str = "only the exact package-owned registrations are removed; imported module memory remains until process exit";

#[must_use]
pub fn owner_id(package: &str) -> String {
    format!("physics-package:{package}:v{OWNER_VERSION}")
}

pub struct InstallContext<'a> {
    registries: &'a Registries,
    owner_id: String,
}

impl InstallContext<'_> {
    #[must_use]
    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    #[must_use]
    pub fn registries(&self) -> &Registries {
        self.registries
    }


    pub fn register_provider(&self, provider: Arc<dyn CaeProvider>) -> CaeResult<Arc<dyn CaeProvider>> {
        self.registries.register_provider(provider)
    }


    pub fn register_addin(
        &self,
        contract: ContractInput,
        adapter: Option<Arc<dyn AddInAdapter>>,
    ) -> CaeResult<AddInContract> {
        self.registries.addins.register(contract, adapter, None)
    }


    pub fn register_sufficiency_rule(&self, rule: SufficiencyRule) -> CaeResult<()> {
        self.registries.sufficiency.register(rule).map(|_| ())
    }


    pub fn register_coupling_rules(&self, module_id: &str, rules: Vec<CouplingRule>) -> CaeResult<()> {
        self.registries.extensions.register_coupling_rules(module_id, rules)
    }


    pub fn register_regime_monitor(
        &self,
        name: &str,
        monitor: Arc<MonitorFn>,
        metrics: Option<Vec<MetricSpec>>,
        owner_id: Option<&str>,
    ) -> CaeResult<()> {
        self.registries.extensions.register_regime_monitor(name, monitor, metrics, owner_id)
    }


    #[allow(clippy::too_many_arguments)]
    pub fn register_case_authoring(
        &self,
        schema: &str,
        builder: Arc<CaseBuilderFn>,
        implementation: &str,
        owner_id: &str,
        label: &str,
        description: &str,
        report_keys: Vec<String>,
    ) -> CaeResult<()> {
        self.registries.extensions.register_case_authoring(
            schema,
            builder,
            implementation,
            owner_id,
            label,
            description,
            report_keys,
        )
    }


    pub fn register_contribution(&self, kind: &str, key: &str, value: ContributionValue) -> CaeResult<()> {
        self.registries
            .contributions
            .register(kind, key, value, &self.owner_id)
            .map(|_| ())
            .map_err(|e| CaeError::contract(e.0))
    }
}

pub type InstallerFn = fn(&InstallContext<'_>) -> CaeResult<()>;

static INSTALLERS: LazyLock<Mutex<BTreeMap<String, InstallerFn>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

pub fn link_installer(module: &str, installer: InstallerFn) {
    lock(&INSTALLERS).insert(module.to_string(), installer);
}

#[must_use]
pub fn linked_installers() -> Vec<String> {
    lock(&INSTALLERS).keys().cloned().collect()
}

fn installer_for(module: &str) -> Option<InstallerFn> {
    lock(&INSTALLERS).get(module).copied()
}

#[derive(Clone)]
pub struct OwnedManifest {
    pub package_id: String,
    pub owner_id: String,
    pub provider_entries: Vec<(String, Arc<dyn CaeProvider>)>,
    pub addin_entries: Vec<Arc<RegisteredAddIn>>,
    pub sufficiency_rules: Vec<Arc<SufficiencyRule>>,
    pub coupling_entries: Vec<(String, Arc<Vec<CouplingRule>>)>,
    pub monitor_entries: Vec<(String, Arc<RegisteredRegimeMonitor>)>,
    pub case_authoring_entries: Vec<(String, Arc<RegisteredCaseAuthoring>)>,
    pub contribution_entries: Vec<((String, String), Arc<Contribution>)>,
    pub payload: Value,
    pub manifest_fingerprint: String,
}

impl std::fmt::Debug for OwnedManifest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnedManifest")
            .field("package_id", &self.package_id)
            .field("manifest_fingerprint", &self.manifest_fingerprint)
            .finish_non_exhaustive()
    }
}

fn provider_same(a: &Arc<dyn CaeProvider>, b: &Arc<dyn CaeProvider>) -> bool {
    Arc::ptr_eq(a, b)
}

fn assert_preserved(before: &RegistryState, after: &RegistryState) -> CaeResult<()> {
    let after_providers: BTreeMap<&String, &Arc<dyn CaeProvider>> =
        after.providers.entries.iter().map(|(k, v)| (k, v)).collect();
    for (key, value) in &before.providers.entries {
        if !after_providers.get(key).is_some_and(|v| provider_same(v, value)) {
            return Err(CaeError::contract(format!("package replaced existing provider {}", repr_str(key))));
        }
    }
    let after_addins: BTreeMap<&String, &Arc<RegisteredAddIn>> =
        after.addins.entries.iter().map(|r| (&r.contract.addin_id, r)).collect();
    for row in &before.addins.entries {
        if !after_addins.get(&row.contract.addin_id).is_some_and(|v| Arc::ptr_eq(v, row)) {
            return Err(CaeError::contract(format!(
                "package replaced existing add-in {}",
                repr_str(&row.contract.addin_id)
            )));
        }
    }
    let after_rules: BTreeMap<String, &Arc<SufficiencyRule>> =
        after.sufficiency.rules.iter().map(|r| (r.identity(), r)).collect();
    for rule in &before.sufficiency.rules {
        let id = rule.identity();
        if !after_rules.get(&id).is_some_and(|v| Arc::ptr_eq(v, rule)) {
            return Err(CaeError::contract(format!(
                "package replaced existing sufficiency rule {}",
                repr_str(&id)
            )));
        }
    }
    let coupling: BTreeMap<&String, &Arc<Vec<CouplingRule>>> =
        after.extensions.coupling.iter().map(|(k, v)| (k, v)).collect();
    for (k, v) in &before.extensions.coupling {
        if !coupling.get(k).is_some_and(|x| Arc::ptr_eq(x, v)) {
            return Err(CaeError::contract(format!(
                "package replaced existing coupling extension {}",
                repr_str(k)
            )));
        }
    }
    let monitors: BTreeMap<&String, &Arc<RegisteredRegimeMonitor>> =
        after.extensions.monitors.iter().map(|(k, v)| (k, v)).collect();
    for (k, v) in &before.extensions.monitors {
        if !monitors.get(k).is_some_and(|x| Arc::ptr_eq(x, v)) {
            return Err(CaeError::contract(format!(
                "package replaced existing monitor extension {}",
                repr_str(k)
            )));
        }
    }
    let cases: BTreeMap<&String, &Arc<RegisteredCaseAuthoring>> =
        after.extensions.case_authoring.iter().map(|(k, v)| (k, v)).collect();
    for (k, v) in &before.extensions.case_authoring {
        if !cases.get(k).is_some_and(|x| Arc::ptr_eq(x, v)) {
            return Err(CaeError::contract(format!(
                "package replaced existing case authoring extension {}",
                repr_str(k)
            )));
        }
    }
    let contributions: BTreeMap<&(String, String), &Arc<Contribution>> =
        after.contributions.entries.iter().map(|(k, v)| (k, v)).collect();
    for (key, row) in &before.contributions.entries {
        if !contributions.get(key).is_some_and(|x| Arc::ptr_eq(x, row)) {
            return Err(CaeError::contract(format!(
                "package replaced existing {} contribution {}",
                key.0,
                repr_str(&key.1)
            )));
        }
    }
    Ok(())
}

fn canonicalise_direct_addin_owners(
    registries: &Registries,
    package: &str,
    before: &RegistryState,
    after_install: &RegistryState,
) -> CaeResult<()> {
    let old_ids: BTreeSet<&String> = before.addins.entries.iter().map(|r| &r.contract.addin_id).collect();
    let old_providers: BTreeSet<&String> = before.providers.entries.iter().map(|(k, _)| k).collect();
    let provider_ids: BTreeSet<&String> = after_install
        .providers
        .entries
        .iter()
        .map(|(k, _)| k)
        .filter(|k| !old_providers.contains(k))
        .collect();
    let owner = owner_id(package);
    for row in &after_install.addins.entries {
        let aid = &row.contract.addin_id;
        if old_ids.contains(aid) || provider_ids.contains(aid) {
            continue;
        }
        if row.owner_identity == owner && row.contract.owner_id == owner {
            continue;
        }
        registries.addins.unregister(aid, row.adapter.as_ref().map(numerical_owner))?;
        let mut contract = row.contract.clone();
        contract.owner_id.clone_from(&owner);
        registries.addins.register(
            ContractInput::Typed(Box::new(contract)),
            row.adapter.clone(),
            Some(&owner),
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn build_manifest(package: &str, before: &RegistryState, after: &RegistryState) -> CaeResult<OwnedManifest> {
    let old_providers: BTreeSet<&String> = before.providers.entries.iter().map(|(k, _)| k).collect();
    let providers: Vec<(String, Arc<dyn CaeProvider>)> =
        after.providers.entries.iter().filter(|(k, _)| !old_providers.contains(k)).cloned().collect();
    let old_addins: BTreeSet<&String> = before.addins.entries.iter().map(|r| &r.contract.addin_id).collect();
    let addins: Vec<Arc<RegisteredAddIn>> =
        after.addins.entries.iter().filter(|r| !old_addins.contains(&r.contract.addin_id)).cloned().collect();
    let old_rules: BTreeSet<String> = before.sufficiency.rules.iter().map(|r| r.identity()).collect();
    let rules: Vec<Arc<SufficiencyRule>> =
        after.sufficiency.rules.iter().filter(|r| !old_rules.contains(&r.identity())).cloned().collect();
    let old_coupling: BTreeSet<&String> = before.extensions.coupling.iter().map(|(k, _)| k).collect();
    let coupling: Vec<(String, Arc<Vec<CouplingRule>>)> =
        after.extensions.coupling.iter().filter(|(k, _)| !old_coupling.contains(k)).cloned().collect();
    let old_monitors: BTreeSet<&String> = before.extensions.monitors.iter().map(|(k, _)| k).collect();
    let monitors: Vec<(String, Arc<RegisteredRegimeMonitor>)> =
        after.extensions.monitors.iter().filter(|(k, _)| !old_monitors.contains(k)).cloned().collect();
    let old_cases: BTreeSet<&String> = before.extensions.case_authoring.iter().map(|(k, _)| k).collect();
    let cases: Vec<(String, Arc<RegisteredCaseAuthoring>)> =
        after.extensions.case_authoring.iter().filter(|(k, _)| !old_cases.contains(k)).cloned().collect();
    let old_contrib: BTreeSet<&(String, String)> =
        before.contributions.entries.iter().map(|(k, _)| k).collect();
    let contributed: Vec<((String, String), Arc<Contribution>)> =
        after.contributions.entries.iter().filter(|(k, _)| !old_contrib.contains(k)).cloned().collect();

    if providers.is_empty()
        && addins.is_empty()
        && rules.is_empty()
        && coupling.is_empty()
        && monitors.is_empty()
        && cases.is_empty()
        && contributed.is_empty()
    {
        return Err(CaeError::contract(format!(
            "physics package {} installed no executable manifest delta",
            repr_str(package)
        )));
    }
    let owner = owner_id(package);
    let old_rule_owners: BTreeSet<&String> =
        before.sufficiency.rules.iter().map(|r| &r.owner_addin_id).collect();
    for rule in &rules {
        if rule.owner_addin_id.is_empty() {
            return Err(CaeError::contract(format!(
                "package {} registered an unowned sufficiency rule",
                repr_str(package)
            )));
        }
        if old_rule_owners.contains(&rule.owner_addin_id) {
            return Err(CaeError::contract(format!(
                "package {} shares sufficiency owner {}",
                repr_str(package),
                repr_str(&rule.owner_addin_id)
            )));
        }
    }
    let mut old_ext_owners: BTreeSet<String> =
        before.extensions.coupling.iter().map(|(k, _)| k.clone()).collect();
    old_ext_owners.extend(before.extensions.monitors.iter().map(|(_, r)| r.owner_id.clone()));
    old_ext_owners.extend(before.extensions.case_authoring.iter().map(|(_, r)| r.owner_id.clone()));
    for (key, _) in &coupling {
        if old_ext_owners.contains(key) {
            return Err(CaeError::contract(format!(
                "package {} shares extension owner {}",
                repr_str(package),
                repr_str(key)
            )));
        }
    }
    for owner_of in monitors.iter().map(|(_, r)| &r.owner_id).chain(cases.iter().map(|(_, r)| &r.owner_id)) {
        if old_ext_owners.contains(owner_of) {
            return Err(CaeError::contract(format!(
                "package {} shares extension owner {}",
                repr_str(package),
                repr_str(owner_of)
            )));
        }
    }
    let payload = manifest_payload(
        package,
        &owner,
        &providers,
        &addins,
        &rules,
        &coupling,
        &monitors,
        &cases,
        &contributed,
    );
    let fingerprint = canonical_sha256(&payload);
    Ok(OwnedManifest {
        package_id: package.to_string(),
        owner_id: owner,
        provider_entries: providers,
        addin_entries: addins,
        sufficiency_rules: rules,
        coupling_entries: coupling,
        monitor_entries: monitors,
        case_authoring_entries: cases,
        contribution_entries: contributed,
        payload,
        manifest_fingerprint: fingerprint,
    })
}

#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn manifest_payload(
    package: &str,
    owner: &str,
    providers: &[(String, Arc<dyn CaeProvider>)],
    addins: &[Arc<RegisteredAddIn>],
    rules: &[Arc<SufficiencyRule>],
    coupling: &[(String, Arc<Vec<CouplingRule>>)],
    monitors: &[(String, Arc<RegisteredRegimeMonitor>)],
    cases: &[(String, Arc<RegisteredCaseAuthoring>)],
    contributed: &[((String, String), Arc<Contribution>)],
) -> Value {
    json!({
        "schema": MANIFEST_SCHEMA,
        "package_id": package,
        "owner_id": owner,
        "providers": providers.iter().map(|(k, p)| json!({"provider_id": k, "implementation": p.implementation()})).collect::<Vec<_>>(),
        "addins": addins.iter().map(|r| json!({
            "addin_id": r.contract.addin_id,
            "owner_identity": r.owner_identity,
            "contract_fingerprint": r.contract_fingerprint,
            "compatibility_mode": r.compatibility_mode,
            "implementation": stable_implementation(r.adapter.as_ref()),
        })).collect::<Vec<_>>(),
        "sufficiency_rule_identities": rules.iter().map(|r| r.identity()).collect::<Vec<_>>(),
        "coupling_extension_ids": coupling.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(),
        "regime_monitors": monitors.iter().map(|(k, r)| json!({"monitor_id": k, "owner_id": r.owner_id})).collect::<Vec<_>>(),
        "case_authoring": cases.iter().map(|(k, r)| json!({"schema": k, "owner_id": r.owner_id, "implementation": r.implementation})).collect::<Vec<_>>(),
        "contributions": contributed.iter().map(|((kind, key), r)| json!({
            "kind": kind, "key": key, "owner_id": r.owner_id, "implementation": r.value.implementation,
        })).collect::<Vec<_>>(),
    })
}

#[must_use]
pub fn load_order_fingerprint(rows: &[Value]) -> String {
    canonical_sha256(&Value::Array(rows.to_vec()))
}

#[derive(Default)]
struct PmState {
    loaded: BTreeMap<String, OwnedManifest>,
    order: Vec<String>,
    generation: u64,
}

pub struct PackageManager {
    registries: &'static Registries,
    distributions: &'static DistributionSet,
    descriptors: OnceLock<CaeResult<Vec<PackageDescriptor>>>,
    relock: ReLock,
    state: Mutex<PmState>,
    publish_selection: bool,
}

impl std::fmt::Debug for PackageManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackageManager").field("selected", &self.selected()).finish_non_exhaustive()
    }
}

impl PackageManager {
    #[must_use]
    pub fn new(
        registries: &'static Registries,
        distributions: &'static DistributionSet,
        publish_selection: bool,
    ) -> Self {
        Self {
            registries,
            distributions,
            descriptors: OnceLock::new(),
            relock: ReLock::new(),
            state: Mutex::new(PmState::default()),
            publish_selection,
        }
    }

    #[must_use]
    pub fn registries(&self) -> &'static Registries {
        self.registries
    }


    pub fn descriptors(&self) -> CaeResult<&[PackageDescriptor]> {
        match self.descriptors.get_or_init(|| load_distribution_catalog(self.distributions)) {
            Ok(d) => Ok(d.as_slice()),
            Err(e) => Err(e.clone()),
        }
    }

    fn descriptor(&self, package: &str) -> CaeResult<PackageDescriptor> {
        self.descriptors()?.iter().find(|d| d.package_id == package).cloned().ok_or_else(|| {
            CaeError::contract(format!("unknown installed physics package {}", repr_str(package)))
        })
    }

    pub fn guard<T>(&self, body: impl FnOnce() -> T) -> T {
        let _g = self.relock.lock();
        body()
    }

    pub fn try_hold(&self) -> Option<crate::sync::ReLockGuard<'_>> {
        self.relock.try_lock()
    }

    pub fn hold(&self) -> crate::sync::ReLockGuard<'_> {
        self.relock.lock()
    }

    fn publish(&self) {
        if self.publish_selection {
            crate::package_state::replace(&self.selected());
        }
    }


    pub fn load(&self, package: &str) -> CaeResult<Value> {
        let _g = self.relock.lock();
        let descriptor = self.descriptor(package)?;
        if lock(&self.state).loaded.contains_key(package) {
            return self.status();
        }
        let registries = self.registries;
        let manifest = registries.transaction(|| {
            let before = registries.state();
            let installer = installer_for(&descriptor.installer).ok_or_else(|| {
                CaeError::contract(format!(
                    "physics package {} installer {} is not linked into this executable",
                    repr_str(package),
                    repr_str(&descriptor.installer)
                ))
            })?;
            let imported = registries.state();
            if imported.binding_tokens() != before.binding_tokens() {
                return Err(CaeError::contract(format!(
                    "physics package {} mutated a runtime registry during import",
                    repr_str(package)
                )));
            }
            let context = InstallContext { registries, owner_id: owner_id(package) };
            installer(&context)?;
            let installed = registries.state();
            assert_preserved(&before, &installed)?;
            canonicalise_direct_addin_owners(registries, package, &before, &installed)?;
            let after = registries.state();
            assert_preserved(&before, &after)?;
            build_manifest(package, &before, &after)
        })?;
        {
            let mut s = lock(&self.state);
            s.loaded.insert(package.to_string(), manifest);
            s.order.push(package.to_string());
            s.generation += 1;
        }
        self.publish();
        self.status()
    }

    fn validate_owned(&self, manifest: &OwnedManifest) -> CaeResult<()> {
        let now = self.registries.state();
        let pid = repr_str(&manifest.package_id);
        let providers: BTreeMap<&String, &Arc<dyn CaeProvider>> =
            now.providers.entries.iter().map(|(k, v)| (k, v)).collect();
        for (key, value) in &manifest.provider_entries {
            if !providers.get(key).is_some_and(|v| provider_same(v, value)) {
                return Err(CaeError::contract(format!(
                    "package {pid} provider ownership drifted for {}",
                    repr_str(key)
                )));
            }
        }
        let addins: BTreeMap<&String, &Arc<RegisteredAddIn>> =
            now.addins.entries.iter().map(|r| (&r.contract.addin_id, r)).collect();
        for row in &manifest.addin_entries {
            if !addins.get(&row.contract.addin_id).is_some_and(|v| Arc::ptr_eq(v, row)) {
                return Err(CaeError::contract(format!(
                    "package {pid} add-in ownership drifted for {}",
                    repr_str(&row.contract.addin_id)
                )));
            }
        }
        let rules: BTreeMap<String, &Arc<SufficiencyRule>> =
            now.sufficiency.rules.iter().map(|r| (r.identity(), r)).collect();
        for rule in &manifest.sufficiency_rules {
            if !rules.get(&rule.identity()).is_some_and(|v| Arc::ptr_eq(v, rule)) {
                return Err(CaeError::contract(format!("package {pid} sufficiency ownership drifted")));
            }
        }
        for (key, value) in &manifest.coupling_entries {
            if !now.extensions.coupling.iter().any(|(k, v)| k == key && Arc::ptr_eq(v, value)) {
                return Err(CaeError::contract(format!(
                    "package {pid} coupling ownership drifted for {}",
                    repr_str(key)
                )));
            }
        }
        for (key, value) in &manifest.monitor_entries {
            if !now.extensions.monitors.iter().any(|(k, v)| k == key && Arc::ptr_eq(v, value)) {
                return Err(CaeError::contract(format!(
                    "package {pid} monitor ownership drifted for {}",
                    repr_str(key)
                )));
            }
        }
        for (key, value) in &manifest.case_authoring_entries {
            if !now.extensions.case_authoring.iter().any(|(k, v)| k == key && Arc::ptr_eq(v, value)) {
                return Err(CaeError::contract(format!(
                    "package {pid} case-authoring ownership drifted for {}",
                    repr_str(key)
                )));
            }
        }
        for (key, row) in &manifest.contribution_entries {
            if !now.contributions.entries.iter().any(|(k, v)| k == key && Arc::ptr_eq(v, row)) {
                return Err(CaeError::contract(format!(
                    "package {pid} {} contribution drifted for {}",
                    key.0,
                    repr_str(&key.1)
                )));
            }
        }
        Ok(())
    }


    pub fn unload(&self, package: &str) -> CaeResult<Value> {
        let _g = self.relock.lock();
        self.descriptor(package)?;
        let Some(manifest) = lock(&self.state).loaded.get(package).cloned() else {
            return self.status();
        };
        let r = self.registries;
        r.transaction(|| {
            self.validate_owned(&manifest)?;
            for rule in &manifest.sufficiency_rules {
                r.sufficiency.unregister(
                    &rule.identity(),
                    Some(&rule.owner_addin_id),
                    Some(&rule.module_id),
                )?;
            }
            let mut owners: BTreeSet<String> =
                manifest.coupling_entries.iter().map(|(k, _)| k.clone()).collect();
            owners.extend(manifest.monitor_entries.iter().map(|(_, m)| m.owner_id.clone()));
            owners.extend(manifest.case_authoring_entries.iter().map(|(_, c)| c.owner_id.clone()));
            for owner in &owners {
                r.extensions.unregister_extensions(owner);
            }
            for ((kind, key), row) in manifest.contribution_entries.iter().rev() {
                r.contributions
                    .unregister(kind, key, row.value.ptr())
                    .map_err(|e| CaeError::contract(e.0))?;
            }
            let provider_ids: BTreeSet<&String> = manifest.provider_entries.iter().map(|(k, _)| k).collect();
            for row in manifest.addin_entries.iter().rev() {
                if provider_ids.contains(&row.contract.addin_id) {
                    continue;
                }
                r.addins.unregister(&row.contract.addin_id, row.adapter.as_ref().map(numerical_owner))?;
            }
            for (key, value) in manifest.provider_entries.iter().rev() {
                r.providers.unregister(&r.addins, key, Some(value))?;
            }
            Ok(())
        })?;
        {
            let mut s = lock(&self.state);
            s.loaded.remove(package);
            s.order.retain(|p| p != package);
            s.generation += 1;
        }
        self.publish();
        self.status()
    }

    #[must_use]
    pub fn selected(&self) -> Vec<String> {
        lock(&self.state).order.clone()
    }

    #[must_use]
    pub fn manifest(&self, package: &str) -> Option<OwnedManifest> {
        lock(&self.state).loaded.get(package).cloned()
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        lock(&self.state).generation
    }

    fn loaded_rows(&self) -> Vec<Value> {
        let s = lock(&self.state);
        s.order
            .iter()
            .filter_map(|name| s.loaded.get(name))
            .map(|m| json!({"package_id": m.package_id, "owner_id": m.owner_id, "manifest_fingerprint": m.manifest_fingerprint}))
            .collect()
    }


    pub fn status(&self) -> CaeResult<Value> {
        let _g = self.relock.lock();
        let r = self.registries;
        let providers = r.providers.snapshot();
        let addins = r.addins.snapshot();
        let contracts = r.addins.catalogue();
        let component_status = capability_status::report(&r.addins);
        let coordinates: BTreeSet<&String> = addins
            .entries
            .iter()
            .flat_map(|e| e.contract.design_inputs.iter().map(|d| &d.coordinate))
            .collect();
        let (order, generation) = {
            let s = lock(&self.state);
            (s.order.clone(), s.generation)
        };
        let loaded_rows = self.loaded_rows();
        let packages: Vec<Value> =
            self.descriptors()?.iter().map(|d| d.status_row(order.contains(&d.package_id))).collect();
        let mut m = Map::new();
        m.insert("schema".into(), json!(STATUS_SCHEMA));
        m.insert("generation".into(), json!(generation));
        m.insert("design_coordinates".into(), json!(coordinates.into_iter().collect::<Vec<_>>()));
        m.insert(
            "compatibility".into(),
            json!({"legacy_single_array_coordinate": crate::contracts::TOPOLOGY_COORDINATE}),
        );
        m.insert("loaded".into(), json!(order));
        m.insert("load_order_fingerprint".into(), json!(load_order_fingerprint(&loaded_rows)));
        m.insert("loaded_manifests".into(), Value::Array(loaded_rows));
        m.insert("packages".into(), Value::Array(packages));
        m.insert("registry_fingerprint".into(), json!(addins.token.fingerprint));
        m.insert(
            "active_physics_provider_count".into(),
            json!(providers.entries.iter().filter(|(_, p)| !p.orchestration_meta()).count()),
        );
        m.insert("active_addin_count".into(), json!(contracts.len()));
        m.insert("component_status".into(), component_status);
        m.insert("deactivation".into(), json!(DEACTIVATION_TEXT));
        Ok(Value::Object(m))
    }


    pub fn load_configured(&self, value: Option<&str>) -> CaeResult<()> {
        for name in value.unwrap_or_default().split(',').map(str::trim).filter(|s| !s.is_empty()) {
            self.load(name)?;
        }
        Ok(())
    }


    pub fn activate_snapshot(&self, names: &[String]) -> CaeResult<()> {
        let unique: BTreeSet<&String> = names.iter().collect();
        let known = self.descriptors()?;
        if unique.len() != names.len() || names.iter().any(|n| !known.iter().any(|d| d.package_id == *n)) {
            return Err(CaeError::contract("runtime_packages contains duplicate or unknown package ids"));
        }
        let _g = self.relock.lock();
        let original = self.selected();
        let attempt = || -> CaeResult<()> {
            for name in self.selected() {
                if !names.contains(&name) {
                    self.unload(&name)?;
                }
            }
            let current = self.selected();
            let wanted: Vec<&String> = names.iter().filter(|n| current.contains(n)).collect();
            if current.iter().collect::<Vec<_>>() != wanted {
                for name in self.selected() {
                    self.unload(&name)?;
                }
            }
            for name in names {
                self.load(name)?;
            }
            Ok(())
        };
        if let Err(e) = attempt() {
            for name in self.selected() {
                if !original.contains(&name) {
                    self.unload(&name)?;
                }
            }
            for name in self.selected() {
                if original.contains(&name) {
                    self.unload(&name)?;
                }
            }
            for name in &original {
                self.load(name)?;
            }
            return Err(e);
        }
        Ok(())
    }


    pub fn activate_snapshot_value(&self, names: &Value) -> CaeResult<()> {
        let Some(list) = crate::pyobj::text_list(names) else {
            return Err(CaeError::contract("runtime_packages must be a list of installed package ids"));
        };
        self.activate_snapshot(&list)
    }
}

static GLOBAL: LazyLock<PackageManager> =
    LazyLock::new(|| PackageManager::new(crate::registries::global(), crate::distributions::global(), true));

#[must_use]
pub fn global() -> &'static PackageManager {
    &GLOBAL
}
