// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Map, Value};

use implexity_core::CaeError;
use implexity_core::orchestration::{
    AddInAdapter, AddInCategory, AddInContract, ContractInput, Fidelity, PortSpec,
};
use implexity_core::packages::InstallContext;
use implexity_physics_base::core_bridge::{
    ComponentOptions, PhysicsComponent, StrictComponentAdapter, register_strict_component,
};

use crate::history::{HistoryComponent, monitor};
use crate::inelastic::{CreepLaw, PlasticLaw};
use crate::material::MaterialLaw;
use crate::util::contract;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolidComponent {
    Material(MaterialLaw),
    Plastic(PlasticLaw),
    Creep(CreepLaw),
    History(HistoryComponent),
    Monitor,
    Maxwell,
    Fatigue,
    HistoryBlock,
    MovingMaterial,
}

impl SolidComponent {
    #[must_use]
    pub fn component_kind(self) -> &'static str {
        match self {
            Self::Material(_) => "material_properties",
            Self::Plastic(_) => "plastic_evolution",
            Self::Creep(_) => "creep_evolution",
            Self::History(_) => "material_state_evolution",
            Self::Monitor => monitor::COMPONENT_KIND,
            Self::Maxwell => crate::polymer::COMPONENT_KIND,
            Self::Fatigue => crate::fatigue::COMPONENT_KIND,
            Self::HistoryBlock => "solid_history_field",
            Self::MovingMaterial => crate::moving_interface::COMPONENT_KIND,
        }
    }

    #[must_use]
    pub fn implementation(self) -> &'static str {
        match self {
            Self::Material(m) => m.implementation(),
            Self::Plastic(p) => p.implementation(),
            Self::Creep(c) => c.implementation(),
            Self::History(h) => h.implementation(),
            Self::Monitor => monitor::IMPLEMENTATION,
            Self::Maxwell => crate::polymer::IMPLEMENTATION,
            Self::Fatigue => crate::fatigue::IMPLEMENTATION,
            Self::HistoryBlock => "implexity.physics_library.solid_history.SolidHistoryFactory",
            Self::MovingMaterial => crate::moving_interface::IMPLEMENTATION,
        }
    }

    #[must_use]
    pub fn runtime_support(self) -> Map<String, Value> {
        match self {
            Self::Material(m) => m.runtime_support(),
            Self::Plastic(p) => p.runtime_support(),
            Self::Creep(c) => c.runtime_support(),
            Self::History(h) => h.runtime_support(),
            Self::Monitor => monitor::runtime_support(),
            Self::Maxwell => crate::polymer::runtime_support(),
            Self::Fatigue => crate::fatigue::runtime_support(),
            Self::HistoryBlock => {
                crate::util::obj(serde_json::json!({"status": "field_component", "history": true,
                "data": "user_required", "limitations": crate::solid_history::LIMITATIONS}))
            }
            Self::MovingMaterial => crate::moving_interface::runtime_support(),
        }
    }

    #[must_use]
    pub fn authoring_contract(self) -> Option<Map<String, Value>> {
        match self {
            Self::Material(m) => m.authoring_contract(),
            Self::Plastic(p) => p.authoring_contract(),
            Self::History(h) => Some(h.authoring_contract()),
            Self::Maxwell => Some(crate::polymer::authoring_contract()),
            Self::Fatigue => Some(crate::fatigue::authoring_contract()),
            Self::Creep(_) | Self::Monitor | Self::HistoryBlock | Self::MovingMaterial => None,
        }
    }

    #[must_use]
    pub fn response_units(self) -> Option<BTreeMap<String, String>> {
        (self == Self::Monitor).then(monitor::response_units)
    }

    #[must_use]
    pub fn solid_study_templates(self, solid: &Value) -> Option<Vec<Value>> {
        match self {
            Self::Creep(c) => Some(c.solid_study_templates(solid)),
            Self::History(HistoryComponent::Species) => {
                Some(HistoryComponent::Species.solid_study_templates(solid))
            }
            _ => None,
        }
    }

    #[must_use]
    pub fn editor(self, settings: &Value, context: &Value) -> Option<(String, Value)> {
        match self {
            Self::History(h) => {
                h.editor_schema(settings, context).ok().map(|s| (h.editor_label().to_string(), s))
            }
            Self::Maxwell => {
                Some((crate::polymer::EDITOR_LABEL.into(), crate::polymer::editor_schema(settings, context)))
            }
            Self::Fatigue => {
                Some((crate::fatigue::EDITOR_LABEL.into(), crate::fatigue::editor_schema(settings, context)))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SolidComponentHandle(pub SolidComponent);

impl PhysicsComponent for SolidComponentHandle {
    fn implementation(&self) -> String {
        self.0.implementation().into()
    }
    fn component_kind(&self) -> Option<String> {
        Some(self.0.component_kind().into())
    }
    fn runtime_support(&self) -> Option<Map<String, Value>> {
        Some(self.0.runtime_support())
    }
    fn authoring_contract(&self) -> Option<Map<String, Value>> {
        self.0.authoring_contract()
    }
    fn response_units(&self) -> Option<BTreeMap<String, String>> {
        self.0.response_units()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SolidAddin(pub SolidComponent);

impl AddInAdapter for SolidAddin {
    fn implementation(&self) -> String {
        self.0.implementation().into()
    }
    fn runtime_support(&self) -> Option<Map<String, Value>> {
        Some(self.0.runtime_support())
    }
    fn component_kind(&self) -> Option<String> {
        Some(self.0.component_kind().into())
    }
    fn authoring_contract(&self) -> Option<Map<String, Value>> {
        self.0.authoring_contract()
    }
    fn response_units(&self) -> Option<BTreeMap<String, String>> {
        self.0.response_units()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[must_use]
pub fn component_of(adapter: &dyn AddInAdapter) -> Option<SolidComponent> {
    let any = adapter.as_any();
    if let Some(a) = any.downcast_ref::<SolidAddin>() {
        return Some(a.0);
    }
    let strict = any.downcast_ref::<StrictComponentAdapter>()?;
    strict.delegate.as_any().downcast_ref::<SolidComponentHandle>().map(|h| h.0)
}


pub fn registered(name: &str) -> Result<Option<SolidComponent>, CaeError> {
    let row = implexity_core::registries::global().addins.get(name)?;
    Ok(row.adapter.as_deref().and_then(component_of))
}


pub fn selected_component(name: &str, kind: &str) -> Result<SolidComponent, CaeError> {
    match registered(name)? {
        Some(c) if c.component_kind() == kind => Ok(c),
        _ => {
            contract(format!("{} is not an active {kind} component", implexity_core::py_repr::repr_str(name)))
        }
    }
}


pub fn selected_history(name: &str) -> Result<HistoryComponent, CaeError> {
    match registered(name)? {
        Some(SolidComponent::History(h)) => Ok(h),
        _ => contract(format!("{name}: not an active material-state evolution component")),
    }
}

#[must_use]
pub fn editor_schema_hook(
    name: &str,
    kind: &str,
    settings: &Value,
    context: &Value,
) -> Option<(String, Value)> {
    let component = registered(name).ok().flatten()?;
    if component.component_kind() != kind {
        return None;
    }
    component.editor(settings, context)
}

#[must_use]
pub fn study_template_components() -> Vec<(String, SolidComponent)> {
    let registry = &implexity_core::registries::global().addins;
    let mut out: Vec<(String, SolidComponent)> = registry
        .snapshot()
        .entries
        .iter()
        .filter_map(|row| {
            let c = row.adapter.as_deref().and_then(component_of)?;
            c.solid_study_templates(&Value::Null).map(|_| (row.contract.addin_id.clone(), c))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn notes(items: &[&str], extra: &[&str]) -> Vec<String> {
    items.iter().chain(extra).map(|s| (*s).to_string()).collect()
}


pub fn register_strict(
    ctx: &InstallContext<'_>,
    name: &str,
    component: SolidComponent,
    quantity: &str,
    category: AddInCategory,
    domain: &str,
    notes: Vec<String>,
) -> Result<AddInContract, CaeError> {
    let options =
        ComponentOptions { category, domain: domain.into(), fidelity: Fidelity::Intermediate, notes };
    register_strict_component(ctx, name, Arc::new(SolidComponentHandle(component)), quantity, &options)
}


pub fn register_legacy(
    ctx: &InstallContext<'_>,
    name: &str,
    component: SolidComponent,
    port: &str,
    direct_topology_dependence: Option<bool>,
    notes: Vec<String>,
) -> Result<AddInContract, CaeError> {
    let mut c = AddInContract::new(name);
    c.category = AddInCategory::Constitutive;
    c.provides = vec![PortSpec::new(port)];
    c.fidelity = Fidelity::Intermediate;
    c.direct_topology_dependence = direct_topology_dependence;
    c.notes = notes;
    ctx.register_addin(ContractInput::Typed(Box::new(c)), Some(Arc::new(SolidAddin(component))))
}


pub fn register_inelastic_components(ctx: &InstallContext<'_>) -> Result<(), CaeError> {
    let rows: [(&str, SolidComponent, &str); 5] = [
        (
            "phase_transition_caloric_solid",
            SolidComponent::Material(MaterialLaw::PhaseTransition),
            "material_property_bundle",
        ),
        (
            "temperature_linear_solid",
            SolidComponent::Material(MaterialLaw::TemperatureLinear),
            "material_property_bundle",
        ),
        (
            "j2_linear_hardening",
            SolidComponent::Plastic(PlasticLaw::J2LinearHardening),
            "plastic_state_update",
        ),
        ("norton_creep", SolidComponent::Creep(CreepLaw::Norton), "creep_state_update"),
        (
            "constant_strain_thermoelastic_solid",
            SolidComponent::Material(MaterialLaw::ConstantStrainThermoelastic),
            "material_property_bundle",
        ),
    ];
    for (name, component, quantity) in rows {
        let limitations: &[&str] = match component {
            SolidComponent::Material(m) => m.limitations(),
            SolidComponent::Plastic(p) => p.limitations(),
            SolidComponent::Creep(c) => c.limitations(),
            _ => &[],
        };
        let reversible = matches!(component, SolidComponent::Material(m) if m.reversible_thermoelastic());
        let caloric = if reversible {
            "Explicit Helmholtz entropy storage replaces enthalpy capacity."
        } else {
            "Native caloric contract revision54: add endpoint volumetric enthalpy for material volume fractions."
        };
        register_strict(
            ctx,
            name,
            component,
            quantity,
            AddInCategory::Constitutive,
            "solid",
            notes(limitations, &["Field-solver component, not an algebraic response.", caloric]),
        )?;
    }
    Ok(())
}


pub fn register_tabulated(ctx: &InstallContext<'_>) -> Result<(), CaeError> {
    let law = MaterialLaw::TemperatureTable;
    register_legacy(
        ctx,
        "temperature_tabulated_solid",
        SolidComponent::Material(law),
        "material_property_bundle",
        Some(false),
        notes(
            law.limitations(),
            &["Native caloric contract revision54: additive endmember volumetric enthalpy."],
        ),
    )?;
    Ok(())
}


pub fn register_chaboche(ctx: &InstallContext<'_>) -> Result<(), CaeError> {
    let extra =
        ["Native variable plastic state contract revision57; no legacy linear coefficient substitution."];
    let material = MaterialLaw::ChabocheTable;
    register_legacy(
        ctx,
        "temperature_tabulated_chaboche_solid",
        SolidComponent::Material(material),
        "material_property_bundle",
        Some(false),
        notes(material.limitations(), &extra),
    )?;
    let plastic = PlasticLaw::J2Chaboche;
    register_legacy(
        ctx,
        "j2_chaboche",
        SolidComponent::Plastic(plastic),
        "plastic_state_update",
        Some(false),
        notes(plastic.limitations(), &extra),
    )?;
    Ok(())
}


pub fn register_material_evolution(ctx: &InstallContext<'_>) -> Result<(), CaeError> {
    let history = |h: HistoryComponent, extra: &[&str]| {
        register_legacy(
            ctx,
            h.component_id(),
            SolidComponent::History(h),
            "material_state_evolution",
            None,
            notes(h.limitations(), extra),
        )
    };
    history(HistoryComponent::Defect, &["Native general constitutive-history revision55."])?;
    history(HistoryComponent::Ageing, &[])?;
    register_legacy(
        ctx,
        "material_history_monitor",
        SolidComponent::Monitor,
        "material_history_responses",
        None,
        notes(&monitor::LIMITATIONS, &[]),
    )?;
    history(HistoryComponent::Species, &[])?;
    history(HistoryComponent::Composite, &[])?;
    Ok(())
}


pub fn register_moving_material(ctx: &InstallContext<'_>) -> Result<(), CaeError> {
    register_strict(
        ctx,
        "moving_material_operators",
        SolidComponent::MovingMaterial,
        crate::moving_interface::COMPONENT_KIND,
        AddInCategory::Interface,
        "interface",
        notes(&crate::moving_interface::LIMITATIONS, &[]),
    )?;
    Ok(())
}
