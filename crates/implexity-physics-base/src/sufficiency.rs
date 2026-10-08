// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
