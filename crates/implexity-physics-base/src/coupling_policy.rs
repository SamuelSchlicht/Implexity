// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeSet;

use implexity_core::CaeError;
use implexity_core::coupling_graph::CouplingEdge;
use implexity_core::extensions::CouplingRule;
use implexity_core::packages::InstallContext;

type EdgeRow = (&'static str, &'static str, &'static str, &'static str, bool, &'static str);

const POLICY: [(&[&str], &[EdgeRow]); 7] = [
    (
        &["flow", "thermal"],
        &[
            ("flow", "thermal", "wall_heat_flux", "iterative", true, "fluid state sets wall energy transfer"),
            (
                "thermal",
                "flow",
                "wall_temperature",
                "iterative",
                true,
                "wall temperature can change fluid energy/properties",
            ),
        ],
    ),
    (
        &["thermal", "structure"],
        &[
            (
                "thermal",
                "structure",
                "temperature_field",
                "iterative",
                true,
                "thermal strain and temperature-dependent properties",
            ),
            (
                "structure",
                "thermal",
                "deformed_geometry_or_contact",
                "iterative",
                false,
                "required when deformation/contact materially changes heat transfer",
            ),
        ],
    ),
    (
        &["flow", "structure"],
        &[
            (
                "flow",
                "structure",
                "pressure_and_shear_load",
                "iterative",
                true,
                "fluid traction loads structure",
            ),
            (
                "structure",
                "flow",
                "deformed_flow_domain",
                "iterative",
                false,
                "required when deformation materially changes flow",
            ),
        ],
    ),
    (
        &["radiation", "thermal"],
        &[
            (
                "radiation",
                "thermal",
                "radiative_heat_flux",
                "iterative",
                true,
                "radiation participates in energy balance",
            ),
            (
                "thermal",
                "radiation",
                "surface_temperature",
                "iterative",
                true,
                "emission depends on temperature",
            ),
        ],
    ),
    (
        &["ageing", "structure"],
        &[
            (
                "structure",
                "ageing",
                "stress_strain_history",
                "iterative",
                true,
                "ageing depends on mechanical history",
            ),
            (
                "ageing",
                "structure",
                "degraded_material_state",
                "iterative",
                true,
                "ageing changes constitutive response",
            ),
        ],
    ),
    (
        &["life_damage", "structure"],
        &[(
            "structure",
            "life_damage",
            "stress_strain_temperature_history",
            "one_way",
            true,
            "life post-processing consumes solved history",
        )],
    ),
    (
        &["erosion", "flow"],
        &[
            (
                "flow",
                "erosion",
                "thermal_shear_particle_loading",
                "iterative",
                true,
                "flow drives erosion/ablation",
            ),
            ("erosion", "flow", "evolved_geometry", "iterative", true, "erosion changes flow domain"),
        ],
    ),
];


pub fn physical_couplings() -> Result<Vec<CouplingRule>, CaeError> {
    POLICY
        .iter()
        .map(|(trigger, edges)| {
            let edges = edges
                .iter()
                .map(|(s, t, q, m, r, why)| {
                    CouplingEdge::new(s, t, q, m, *r, why).map_err(|e| CaeError::contract(e.0))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CouplingRule {
                trigger: trigger.iter().map(|x| (*x).to_string()).collect::<BTreeSet<_>>(),
                edges,
            })
        })
        .collect()
}


pub fn install(ctx: &InstallContext<'_>, owner: &str) -> Result<(), CaeError> {
    ctx.register_coupling_rules(&format!("{owner}.physical_coupling_policy"), physical_couplings()?)
}
