// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use crate::model_errors::PhysicsResult;
use crate::planner::{ObjectiveRequest, PhysicsPlan};
use crate::registry::{PhysicsAddinRegistry, ensure_default_addins};


pub fn build_physics_provenance(
    plan: &PhysicsPlan,
    objectives: &[ObjectiveRequest],
    registry: Option<&PhysicsAddinRegistry>,
    authoring: Option<&Map<String, Value>>,
) -> PhysicsResult<Value> {
    let reg = ensure_default_addins(registry)?;
    let selected = plan
        .selected_addins
        .iter()
        .map(|cid| reg.get(cid).map(|c| (cid.clone(), c)))
        .collect::<PhysicsResult<Vec<_>>>()?;
    let mut nodes = Map::new();
    for (cid, c) in &selected {
        nodes.insert(
            cid.clone(),
            json!({
                "addin_id": cid,
                "family": c.family,
                "fidelity": c.fidelity,
                "qualification_level": c.qualification_level,
                "produces": c.produces.iter().map(|p| p.quantity.clone()).collect::<Vec<_>>(),
                "consumes": c.consumes.iter().map(|p| p.quantity.clone()).collect::<Vec<_>>(),
                "design_coordinates": c.design_coordinates,
                "validity_notes": c.validity_notes,
            }),
        );
    }
    let mut records = Vec::new();
    for o in objectives {
        let q = &o.quantity;
        let providers: Vec<&String> = selected
            .iter()
            .filter(|(_, c)| {
                c.objective_aliases.contains(q) || c.produces.iter().any(|p| &p.quantity == q || &p.name == q)
            })
            .map(|(cid, _)| cid)
            .collect();
        let mut common: Option<BTreeSet<String>> = None;
        for cid in &providers {
            let coords: BTreeSet<String> = selected
                .iter()
                .find(|(id, _)| id == *cid)
                .map(|(_, c)| c.design_coordinates.iter().cloned().collect())
                .unwrap_or_default();
            common = Some(match common {
                None => coords,
                Some(prev) => prev.intersection(&coords).cloned().collect(),
            });
        }
        let common = common.unwrap_or_default();
        records.push(json!({
            "quantity": q,
            "providers": providers,
            "design_reachable": !providers.is_empty() && !common.is_empty(),
            "design_coordinates": common.into_iter().collect::<Vec<_>>(),
        }));
    }
    let mut auth = Map::new();
    for (k, v) in authoring.into_iter().flatten() {
        if selected.iter().any(|(cid, _)| cid == k) {
            auth.insert(k.clone(), v.clone());
        }
    }
    let mut assumptions = Map::new();
    for (cid, c) in &selected {
        assumptions.insert(cid.clone(), json!(c.validity_notes));
    }
    Ok(json!({
        "plan_status": plan.status,
        "objectives": records,
        "nodes": nodes,
        "edges": plan.edges.iter().map(|(a, b, q)| json!({"source": a, "target": b, "quantity": q})).collect::<Vec<_>>(),
        "authoring": auth,
        "assumptions": assumptions,
    }))
}
