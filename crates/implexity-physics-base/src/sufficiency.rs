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


use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

use implexity_core::py_repr::repr_str;

use crate::contracts::ValidationIssue;
use crate::model_errors::PhysicsResult;
use crate::planner::{ObjectiveRequest, PhysicsPlan};
use crate::registry::{PhysicsAddinRegistry, ensure_default_addins};

fn rank(f: &str) -> u8 {
    match f {
        "intermediate" => 1,
        "verification" => 2,
        "production" => 3,
        _ => 0,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SufficiencyRequirement {
    pub quantity: String,
    pub minimum_fidelity: String,
    pub required_families: Vec<String>,
    pub required_capabilities: Vec<String>,
    pub description: String,
}

fn req(
    q: &str,
    fidelity: &str,
    families: &[&str],
    capabilities: &[&str],
    description: &str,
) -> SufficiencyRequirement {
    SufficiencyRequirement {
        quantity: q.into(),
        minimum_fidelity: fidelity.into(),
        required_families: families.iter().map(|s| (*s).to_string()).collect(),
        required_capabilities: capabilities.iter().map(|s| (*s).to_string()).collect(),
        description: description.into(),
    }
}

#[must_use]
pub fn default_requirements() -> BTreeMap<String, SufficiencyRequirement> {
    [
        req(
            "fatigue_life",
            "verification",
            &["evolution", "constitutive"],
            &[],
            "Life prediction requires stress/strain history plus a validated damage/fatigue law.",
        ),
        req(
            "fracture_risk",
            "verification",
            &["evolution"],
            &[],
            "Fracture assessment requires an explicit damage/fracture evolution model.",
        ),
        req(
            "max_temperature",
            "screening",
            &[],
            &["temperature"],
            "Temperature requires an energy balance and thermal boundary conditions.",
        ),
        req(
            "pressure_drop",
            "screening",
            &[],
            &["pressure_drop"],
            "Pressure loss requires a momentum/hydraulic model.",
        ),
        req(
            "eigenfrequency",
            "intermediate",
            &[],
            &["eigenfrequency"],
            "Eigenfrequency requires mass and stiffness dynamics.",
        ),
        req(
            "current_density",
            "intermediate",
            &[],
            &["current_density"],
            "Current density requires charge-transport/electromagnetic physics.",
        ),
        req(
            "phase_fraction",
            "intermediate",
            &[],
            &["phase_fraction"],
            "Phase fraction requires a phase-transition/evolution law.",
        ),
        req(
            "wear_depth",
            "verification",
            &["evolution"],
            &[],
            "Wear depth requires an explicit geometry-evolution law.",
        ),
    ]
    .into_iter()
    .map(|r| (r.quantity.clone(), r))
    .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct SufficiencyResult {
    pub sufficient: bool,
    pub issues: Vec<ValidationIssue>,
    pub evidence: Map<String, Value>,
}

pub struct PhysicsSufficiencyAuditor<'a> {
    registry: &'a PhysicsAddinRegistry,
    rules: BTreeMap<String, SufficiencyRequirement>,
}

impl<'a> PhysicsSufficiencyAuditor<'a> {

    pub fn new(
        registry: Option<&'a PhysicsAddinRegistry>,
        rules: Option<BTreeMap<String, SufficiencyRequirement>>,
    ) -> PhysicsResult<Self> {
        let mut all = default_requirements();
        all.extend(rules.unwrap_or_default());
        Ok(Self { registry: ensure_default_addins(registry)?, rules: all })
    }


    pub fn audit(
        &self,
        plan: &PhysicsPlan,
        objectives: &[ObjectiveRequest],
        requested_fidelity: &str,
    ) -> PhysicsResult<SufficiencyResult> {
        let selected =
            plan.selected_addins.iter().map(|x| self.registry.get(x)).collect::<PhysicsResult<Vec<_>>>()?;
        let mut outputs: BTreeSet<&str> = BTreeSet::new();
        for c in &selected {
            outputs.extend(c.produces.iter().map(|p| p.quantity.as_str()));
            outputs.extend(c.objective_aliases.iter().map(String::as_str));
        }
        let families: BTreeSet<&str> = selected.iter().map(|c| c.family.as_str()).collect();
        let has_family = |fam: &str| families.iter().any(|f| *f == fam || f.starts_with(&format!("{fam}_")));
        let mut issues = Vec::new();
        let mut evidence = Map::new();
        let requested_rank = rank(requested_fidelity);
        for obj in objectives {
            let q = &obj.quantity;
            let Some(rule) = self.rules.get(q) else {
                evidence.insert(q.clone(), json!({"rule": "none", "status": "not_asserted"}));
                continue;
            };
            let min_rank = requested_rank.max(rank(&rule.minimum_fidelity));
            let candidates: Vec<_> = selected
                .iter()
                .filter(|c| c.objective_aliases.contains(q) || c.produces.iter().any(|p| &p.quantity == q))
                .collect();
            let capability_ok = rule.required_capabilities.iter().all(|cap| outputs.contains(cap.as_str()));
            let family_ok = rule.required_families.iter().all(|f| has_family(f));
            let best_rank = candidates.iter().map(|c| rank(&c.fidelity)).max();
            let fidelity_ok = best_rank.is_some_and(|r| r >= min_rank);
            if candidates.is_empty() {
                issues.push(ValidationIssue::new(
                    "PHYSICS_INSUFFICIENT_RESPONSE",
                    format!("No selected add-in can substantiate engineering quantity {}.", repr_str(q)),
                ));
            }
            if !capability_ok {
                let missing: Vec<String> = rule
                    .required_capabilities
                    .iter()
                    .filter(|x| !outputs.contains(x.as_str()))
                    .map(|x| repr_str(x))
                    .collect();
                issues.push(ValidationIssue::new(
                    "PHYSICS_INSUFFICIENT_CAPABILITY",
                    format!("{} lacks required physical capabilities [{}].", repr_str(q), missing.join(", ")),
                ));
            }
            if !family_ok {
                let missing: Vec<String> =
                    rule.required_families.iter().filter(|x| !has_family(x)).map(|x| repr_str(x)).collect();
                issues.push(ValidationIssue::new(
                    "PHYSICS_INSUFFICIENT_FAMILY",
                    format!("{} lacks required model families [{}].", repr_str(q), missing.join(", ")),
                ));
            }
            if !candidates.is_empty() && !fidelity_ok {
                let best = candidates
                    .iter()
                    .map(|c| (rank(&c.fidelity), c.fidelity.as_str(), c.addin_id.as_str()))
                    .max()
                    .unwrap_or_default();
                issues.push(ValidationIssue::new(
                    "PHYSICS_INSUFFICIENT_FIDELITY",
                    format!(
                        "{} requires at least rank {min_rank}; best selected provider is {} ({}).",
                        repr_str(q),
                        best.2,
                        best.1
                    ),
                ));
            }
            evidence.insert(
                q.clone(),
                json!({
                    "candidates": candidates.iter().map(|c| c.addin_id.clone()).collect::<Vec<_>>(),
                    "minimum_fidelity": rule.minimum_fidelity,
                    "requested_fidelity": requested_fidelity,
                    "description": rule.description,
                }),
            );
        }
        Ok(SufficiencyResult { sufficient: !issues.iter().any(|i| i.blocking), issues, evidence })
    }
}
