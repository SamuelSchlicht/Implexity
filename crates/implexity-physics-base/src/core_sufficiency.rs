// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
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
