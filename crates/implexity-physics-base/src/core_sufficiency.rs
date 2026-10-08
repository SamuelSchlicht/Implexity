// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::CaeError;
use implexity_core::packages::InstallContext;
use implexity_core::sufficiency::SufficiencyRule;

type RuleRow = (
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
    &'static str,
);

const RULES: [RuleRow; 12] = [
    (
        "hydraulic_network",
        "pressure_drop",
        "screening",
        &[],
        &[],
        "Pressure loss requires a momentum/hydraulic relation with fluid properties and geometry.",
    ),
    (
        "hydraulic_network",
        "pump_power",
        "screening",
        &[],
        &["pressure_drop"],
        "Pump power derives from pressure loss and flow work.",
    ),
    (
        "archard_wear_evolution",
        "wear_depth",
        "high",
        &["evolution"],
        &[],
        "Engineering wear depth requires an explicit geometry-evolution law and verification-level calibration.",
    ),
    (
        "finite_strain_neo_hookean",
        "strain_energy",
        "intermediate",
        &["field"],
        &[],
        "Finite-strain energy requires a finite-deformation mechanics model.",
    ),
    (
        "finite_strain_neo_hookean",
        "stress",
        "intermediate",
        &[],
        &[],
        "Stress claims require an explicit constitutive/mechanics path, not geometry alone.",
    ),
    (
        "phase_change_enthalpy",
        "phase_fraction",
        "intermediate",
        &[],
        &[],
        "Phase fraction requires an explicit phase-transition model.",
    ),
    (
        "butler_volmer_interface",
        "current_density",
        "intermediate",
        &[],
        &[],
        "Current density requires an explicit charge-transfer relation.",
    ),
    (
        "eddy_current_induction",
        "current_density",
        "intermediate",
        &[],
        &[],
        "Current density requires an explicit electromagnetic constitutive relation.",
    ),
    (
        "moving_conductor_electromagnetics",
        "electromagnetic_mechanical_power",
        "intermediate",
        &[],
        &[],
        "Moving-conductor power requires collocated instantaneous laboratory-frame E, B, conductivity and material velocity; the local closure does not solve Maxwell fields.",
    ),
    (
        "nernst_planck_ionic_transport",
        "ionic_current",
        "intermediate",
        &[],
        &[],
        "Ionic current requires species and charge transport.",
    ),
    (
        "gray_surface_radiation",
        "radiative_heat_flux",
        "screening",
        &[],
        &[],
        "Radiative heat flux requires an explicit radiation model and absolute temperatures.",
    ),
    (
        "species_reaction_network",
        "reaction_heat",
        "intermediate",
        &[],
        &[],
        "Reaction heat requires a reaction network and an energy source term.",
    ),
];

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}


pub fn register_rules(ctx: &InstallContext<'_>) -> Result<Vec<SufficiencyRule>, CaeError> {
    let mut rows = Vec::with_capacity(RULES.len());
    for (owner, response, fidelity, categories, companions, rationale) in RULES {
        let contract = ctx.registries().addins.get(owner)?.contract.clone();
        let units: Vec<&String> =
            contract.responses.iter().filter(|c| c.response == response).map(|c| &c.unit).collect();
        if units.len() != 1 {
            return Err(CaeError::contract(format!(
                "{owner}:{response} has no unique registered response unit"
            )));
        }
        let mut rule = SufficiencyRule::new(response);
        rule.minimum_fidelity = fidelity.into();
        rule.required_categories = strings(categories);
        rule.required_companion_responses = strings(companions);
        rule.rationale = rationale.into();
        rule.module_id = "engineering_physics_library".into();
        rule.owner_addin_id = owner.into();
        rule.response_unit.clone_from(units[0]);
        rule.rule_version = 2;
        rows.push(rule);
    }
    for row in &rows {
        ctx.register_sufficiency_rule(row.clone())?;
    }
    Ok(rows)
}
