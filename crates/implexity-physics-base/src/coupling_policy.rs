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
