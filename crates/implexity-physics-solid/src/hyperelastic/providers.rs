// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};

use implexity_core::CaeError;
use implexity_core::contracts::{
    CaeProvider, Evaluation, FieldValue, ProviderCapabilities, ProviderDescriptor, ProviderProblem,
    Sensitivity,
};
use implexity_core::coupling_graph::CouplingDeclaration;
use implexity_core::orchestration::{
    AddInCategory, AddInContract, DesignCoordinateRef, ExecutionKind, Fidelity, PublishedContract,
    ResponseCapability, RuntimeRoute,
};
use implexity_optim::design::{NamedArrays, design_identity};
use implexity_optim::optimizer::OptimizerLifecycleConfig;
use implexity_optim::provider_ops::{
    AdmissionReply, CandidateDesign, DesignOp, DesignOperations, DesignSensitivities, DesignSensitivity,
    LifecycleDeclaration,
};

use super::hydrogel::GelProblem;
use super::kinematics::{Mat3, TetMesh};
use super::poro::{PoroProblem, parse_mesh};
use super::statics::{
    StaticOptions, StaticProblem, StaticResult, ViscoelasticState, solve_viscoelastic_history,
};
use super::thermo::ThermoProblem;
use crate::util::{bool_array, contract, f64_shaped, int_array, obj, real_array};

pub const COORDINATE: &str = "model:material_control";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Static,
    Viscoelastic,
    Poro,
    PoroDesign,
    Thermo,
    Hydrogel,
    HydrogelDesign,
    ThermalDesign,
}

pub const ALL: [Kind; 8] = [
    Kind::Static,
    Kind::Viscoelastic,
    Kind::Poro,
    Kind::PoroDesign,
    Kind::Thermo,
    Kind::Hydrogel,
    Kind::HydrogelDesign,
    Kind::ThermalDesign,
];

const STATIC_LIMITS: [&str; 4] = [
    "Compressible neo-Hookean tetrahedra, fixed reference mesh, static dead loads.",
    "No contact, follower pressure, dynamics, mixed incompressibility, viscoelasticity or solvent transport.",
    "Dense solve limited to 256 nodes; convergence does not certify stability or material calibration.",
    "Evaluation only: no registered field-solve design adjoint.",
];
const VISCO_LIMITS: [&str; 4] = [
    "Quasistatic neo-Hookean solid plus reference Green-strain Maxwell branches; fixed reference mesh and dead loads.",
    "End-of-step histories use backward Euler; dissipation is not an automatically measured hysteresis-loop area.",
    "No solvent transport, swelling, contact, dynamics, mixed incompressibility or ageing-induced parameter evolution.",
    "Evaluation only; no field-history topology adjoint. Dense solve limited to 256 nodes; synthetic template is not calibrated.",
];
const PORO_LIMITS: [&str; 4] = [
    "Experimental saturated finite-volume poromechanics with reference Darcy transport and Green-strain Maxwell memory.",
    "Pressure drives deformation; J changes fluid storage. Constant reference mobility is not a deformation-updated spatial permeability.",
    "No chemical mixing/swelling, unsaturation, cavitation, inertia, contact or mixed incompressible limit.",
    "Evaluation only: no full-history topology adjoint. Use calibrated coefficients; test mesh/time convergence for each case.",
];
const PORO_DESIGN_NOTES: [&str; 4] = [
    "Direct-gradient spatial material design through the complete poroviscoelastic equilibrium and memory history.",
    "Fixed tetrahedral mesh, boundary masks and time schedule. This changes material fields, not a sharp CAD boundary.",
    "Mechanical work uses trapezoidal integration of total first Piola stress against deformation gradient; it is not automatically irreversible loop energy.",
    "Exact implicit derivatives refer to the admitted discrete branch, not bifurcations or physical calibration. Experimental dense solver.",
];
const THERMO_LIMITS: [&str; 5] = [
    "Fixed-reference quasistatic poroviscoelasticity coupled to heat conduction, convection and Arrhenius rates.",
    "Optional irreversible stiffness ageing and chemical-energy release; mechanical release uses endpoint time quadrature.",
    "Optional nonlocal energy-driven scalar damage degrades stiffness and releases heat; calibrated rate law, not qualified fracture or fatigue life.",
    "No reversible thermal expansion, chemical swelling, latent heat, contact or dynamics.",
    "History evaluation; use thermal_poroviscoelastic_material_design for coupled material derivatives and optimization. Calibrated temperature bounds are mandatory.",
];
const GEL_LIMITS: [&str; 4] = [
    "Neutral isothermal Flory mixing and Gaussian network coupled to solvent diffusion and Green-strain Maxwell memory.",
    "Dry reference mesh with a positive-solvent initial state. Chemical potential in Pa is not pore pressure.",
    "Finite constituent-volume penalty, reference mobility and convex good-solvent chi in [0,0.5].",
    "No ionic chemistry, phase separation, thermal coupling, contact or registered design adjoint. Experimental evaluation only.",
];
const GEL_DESIGN_NOTES: [&str; 3] = [
    "Fixed-reference neutral hydrogel material-field optimization through coupled displacement, solvent and Maxwell history.",
    "Implicit derivatives at admitted discrete states; fixed initial state, temperature, mesh, schedule and boundary masks.",
    "No phase separation, moving geometry, mixed incompressibility or physical qualification. Monitor finite volume-penalty error.",
];
const THERMAL_DESIGN_NOTES: [&str; 3] = [
    "Fixed-mesh material design through coupled equilibrium, heat, Darcy transport, Maxwell memory, optional ageing and scalar damage.",
    "Implicit discrete-history derivatives; initial states, calibrated temperature interval, mesh and loading schedule are fixed.",
    "No reversible thermoelastic expansion, chemical swelling, fracture or moving-boundary geometry. Experimental; validate mesh/time convergence.",
];

const PORO_MATERIALS: [&str; 7] = [
    "shear_Pa",
    "lame_Pa",
    "branch_moduli_Pa",
    "relaxation_times_s",
    "biot_coefficient",
    "biot_modulus_Pa",
    "reference_mobility_m2_Pa_s",
];
const GEL_MATERIALS: [&str; 7] = [
    "network_shear_Pa",
    "volume_penalty_Pa",
    "solvent_molar_volume_m3_mol",
    "chi",
    "reference_mobility_m2_Pa_s",
    "branch_moduli_Pa",
    "relaxation_times_s",
];

fn thermal_materials() -> Vec<String> {
    let mut out: Vec<String> = PORO_MATERIALS.iter().map(|k| format!("poromechanics.{k}")).collect();
    out.extend(super::thermo::THERMAL.iter().map(|k| format!("thermal.{k}")));
    out.extend(super::thermo::AGEING_KEYS.iter().map(|k| format!("thermal.ageing.{k}")));
    out.extend(super::thermo::DAMAGE_KEYS.iter().map(|k| format!("thermal.damage.{k}")));
    out
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

impl Kind {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Static => "finite_deformation_tetrahedra",
            Self::Viscoelastic => "finite_deformation_viscoelastic_history",
            Self::Poro => "finite_deformation_poroviscoelastic_history",
            Self::PoroDesign => "poroviscoelastic_material_design",
            Self::Thermo => "thermo_poroviscoelastic_history",
            Self::Hydrogel => "neutral_hydrogel_swelling_history",
            Self::HydrogelDesign => "hydrogel_material_design",
            Self::ThermalDesign => "thermal_poroviscoelastic_material_design",
        }
    }

    #[must_use]
    pub fn class(self) -> &'static str {
        match self {
            Self::Static => "HyperelasticFieldProvider",
            Self::Viscoelastic => "ViscoelasticFieldProvider",
            Self::Poro => "PoroViscoelasticFieldProvider",
            Self::PoroDesign => "PoroMaterialDesignProvider",
            Self::Thermo => "ThermoPoroFieldProvider",
            Self::Hydrogel => "HydrogelFieldProvider",
            Self::HydrogelDesign => "HydrogelMaterialDesignProvider",
            Self::ThermalDesign => "ThermalMaterialDesignProvider",
        }
    }

    fn design(self) -> bool {
        matches!(self, Self::PoroDesign | Self::HydrogelDesign | Self::ThermalDesign)
    }

    #[must_use]
    pub fn units(self) -> Vec<(&'static str, &'static str)> {
        match self {
            Self::Static => vec![
                ("stored_energy_J", "J"),
                ("peak_displacement_m", "m"),
                ("minimum_J", "1"),
                ("free_residual_N", "N"),
            ],
            Self::Viscoelastic => vec![
                ("dissipation_J", "J"),
                ("final_stored_energy_J", "J"),
                ("peak_displacement_m", "m"),
                ("minimum_J", "1"),
                ("free_residual_N", "N"),
            ],
            Self::Poro => vec![
                ("dissipation_J", "J"),
                ("final_stored_energy_J", "J"),
                ("peak_displacement_m", "m"),
                ("minimum_J", "1"),
                ("maximum_fluid_balance_error_m3", "m3"),
                ("net_reservoir_exchange_m3", "m3"),
            ],
            Self::PoroDesign => vec![
                ("dissipation_J", "J"),
                ("mechanical_work_J", "J"),
                ("final_stored_energy_J", "J"),
                ("final_displacement_squared_m2", "m2"),
                ("final_fluid_content_increment_m3", "m3"),
            ],
            Self::Thermo => vec![
                ("generated_heat_J", "J"),
                ("maximum_temperature_K", "K"),
                ("minimum_J", "1"),
                ("maximum_heat_balance_error_J", "J"),
                ("maximum_fluid_balance_error_m3", "m3"),
            ],
            Self::Hydrogel => vec![
                ("final_solvent_volume_m3", "m3"),
                ("final_free_energy_J", "J"),
                ("minimum_J", "1"),
                ("maximum_solvent_balance_error_m3", "m3"),
                ("maximum_volume_constraint_error", "1"),
            ],
            Self::HydrogelDesign => vec![
                ("final_solvent_volume_m3", "m3"),
                ("final_free_energy_J", "J"),
                ("dissipation_J", "J"),
                ("final_displacement_squared_m2", "m2"),
            ],
            Self::ThermalDesign => vec![
                ("final_mean_temperature_K", "K"),
                ("generated_heat_J", "J"),
                ("final_displacement_squared_m2", "m2"),
                ("final_mean_ageing_extent", "1"),
                ("final_mean_damage", "1"),
            ],
        }
    }

    #[must_use]
    pub fn notes(self) -> Vec<String> {
        match self {
            Self::Static => strings(&STATIC_LIMITS),
            Self::Viscoelastic => strings(&VISCO_LIMITS),
            Self::Poro => strings(&PORO_LIMITS),
            Self::PoroDesign => strings(&PORO_DESIGN_NOTES),
            Self::Thermo => strings(&THERMO_LIMITS),
            Self::Hydrogel => strings(&GEL_LIMITS),
            Self::HydrogelDesign => strings(&GEL_DESIGN_NOTES),
            Self::ThermalDesign => strings(&THERMAL_DESIGN_NOTES),
        }
    }

    fn materials(self) -> Vec<String> {
        match self {
            Self::HydrogelDesign => strings(&GEL_MATERIALS),
            Self::ThermalDesign => thermal_materials(),
            _ => strings(&PORO_MATERIALS),
        }
    }

    fn scope(self) -> Vec<String> {
        strings(match self {
            Self::Static | Self::Viscoelastic => &["mechanics"][..],
            Self::Poro | Self::PoroDesign => &["mechanics", "porous_flow"],
            Self::Thermo | Self::ThermalDesign => {
                &["mechanics", "porous_flow", "thermal", "ageing", "damage"]
            }
            Self::Hydrogel | Self::HydrogelDesign => &["mechanics", "solvent_transport"],
        })
    }
}

fn arr(values: Vec<f64>, shape: &[usize]) -> Result<ArrayD<f64>, CaeError> {
    ArrayD::from_shape_vec(IxDyn(shape), values)
        .map_err(|e| CaeError::contract(format!("internal array shape error: {e}")))
}

fn field(values: Vec<f64>, shape: &[usize]) -> Result<FieldValue, CaeError> {
    Ok(FieldValue::Array(arr(values, shape)?))
}

fn mats(v: &[Mat3<f64>]) -> Vec<f64> {
    v.iter().flat_map(|m| m.iter().flatten().copied()).collect()
}

fn fmax(v: impl Iterator<Item = f64>) -> f64 {
    v.fold(f64::NEG_INFINITY, f64::max)
}

fn fmin(v: impl Iterator<Item = f64>) -> f64 {
    v.fold(f64::INFINITY, f64::min)
}

fn peak(u: &[f64]) -> f64 {
    fmax(u.chunks(3).map(|c| (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt()))
}

fn cumsum(v: &[f64]) -> Vec<f64> {
    let mut acc = 0.0;
    v.iter()
        .map(|x| {
            acc += x;
            acc
        })
        .collect()
}

fn without_provenance(p: &Value) -> Value {
    let mut m = p.as_object().cloned().unwrap_or_default();
    m.remove("provenance");
    Value::Object(m)
}

fn nonblank(v: Option<&Value>) -> bool {
    v.and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty())
}

struct StaticInputs {
    mesh: TetMesh,
    mu: Vec<f64>,
    lam: Vec<f64>,
    fixed: Vec<bool>,
}

fn static_inputs(p: &Value) -> Result<StaticInputs, CaeError> {
    let n = match real_array(&p["points"]) {
        Some((s, _)) if s.len() == 2 && s[1] == 3 && (4..=256).contains(&s[0]) => s[0],
        _ => return contract("dense hyperelastic path requires 4..256 XYZ nodes"),
    };
    let ne = p["elements"].as_array().map_or(0, Vec::len);
    let (mu, lam) = (f64_shaped(&p["shear_Pa"], &[ne]), f64_shaped(&p["lame_Pa"], &[ne]));
    let (Some(mu), Some(lam)) = (mu, lam) else {
        return contract("one positive shear modulus and nonnegative Lame lambda per element required");
    };
    if mu.iter().chain(&lam).any(|v| !v.is_finite())
        || mu.iter().any(|v| *v <= 0.0)
        || lam.iter().any(|v| *v < 0.0)
    {
        return contract("one positive shear modulus and nonnegative Lame lambda per element required");
    }
    if int_array(&p["elements"]).is_none_or(|(s, _)| s.len() != 2 || s[1] != 4) {
        return contract("elements require integer [element,4] node indices");
    }
    let mesh = parse_mesh(p)?;
    let fixed = bool_array(&p["fixed_dofs"]).filter(|(s, _)| s == &[n, 3]).map(|x| x.1).unwrap_or_default();
    Ok(StaticInputs { mesh, mu, lam, fixed })
}

fn node_array(v: &Value, n: usize) -> Vec<f64> {
    f64_shaped(v, &[n, 3]).unwrap_or_default()
}

fn solve_static(p: &Value, validate_only: bool) -> Result<Option<StaticResult>, CaeError> {
    let s = static_inputs(p)?;
    let n = s.mesh.node_count();
    let prescribed = node_array(&p["prescribed_displacement_m"], n);
    let force = node_array(&p["nodal_force_N"], n);
    let problem = StaticProblem {
        mesh: &s.mesh,
        mu: &s.mu,
        lam: &s.lam,
        fixed: &s.fixed,
        prescribed: &prescribed,
        force: &force,
        initial: None,
        viscoelastic: None,
        options: StaticOptions::default(),
    };
    problem.validate()?;
    if validate_only {
        return Ok(None);
    }
    problem.solve().map(Some)
}

struct ViscoInputs {
    base: StaticInputs,
    prescribed: Vec<Vec<f64>>,
    force: Vec<Vec<f64>>,
    steps: Vec<f64>,
    moduli: Vec<Vec<f64>>,
    times: Vec<Vec<f64>>,
    memory: Vec<Vec<Mat3<f64>>>,
    initial: Vec<f64>,
}

fn visco_inputs(p: &Value) -> Result<ViscoInputs, CaeError> {
    let steps = match real_array(&p["steps_s"]) {
        Some((s, v))
            if s.len() == 1
                && (2..=1000).contains(&v.len())
                && v.iter().all(|x| x.is_finite() && *x > 0.0) =>
        {
            v
        }
        _ => return contract("2..1000 finite positive time increments required"),
    };
    let nt = steps.len();
    let pshape = real_array(&p["points"]).map(|x| x.0).unwrap_or_default();
    let mut hshape = vec![nt];
    hshape.extend(&pshape);
    let (u, f) = (
        f64_shaped(&p["prescribed_displacement_history_m"], &hshape),
        f64_shaped(&p["nodal_force_history_N"], &hshape),
    );
    let (Some(u), Some(f)) = (u, f) else {
        return contract("Matching finite step-by-node-by-XYZ histories required");
    };
    if u.iter().chain(&f).any(|v| !v.is_finite()) {
        return contract("Matching finite step-by-node-by-XYZ histories required");
    }
    let base = static_inputs(p)?;
    let (n, ne) = (base.mesh.node_count(), base.mesh.elements.len());
    let branch_error =
        "branch arrays require element-by-branch materials and element-by-branch-by-3-by-3 memory";
    let (Some((ms, moduli)), Some((ts, times)), Some((mems, memory))) = (
        real_array(&p["branch_moduli_Pa"]),
        real_array(&p["relaxation_times_s"]),
        real_array(&p["initial_branch_strain"]),
    ) else {
        return contract(branch_error);
    };
    if ms.len() != 2 || ms[0] != ne || ms[1] < 1 || ts != ms || mems != [ne, ms[1], 3, 3] {
        return contract(branch_error);
    }
    let nb = ms[1];
    let initial = f64_shaped(&p["initial_displacement_m"], &[n, 3]).unwrap_or_default();
    let memory: Vec<Mat3<f64>> =
        memory.chunks(9).map(|c| std::array::from_fn(|r| std::array::from_fn(|k| c[3 * r + k]))).collect();
    Ok(ViscoInputs {
        base,
        prescribed: u.chunks(3 * n).map(<[f64]>::to_vec).collect(),
        force: f.chunks(3 * n).map(<[f64]>::to_vec).collect(),
        steps,
        moduli: moduli.chunks(nb).map(<[f64]>::to_vec).collect(),
        times: times.chunks(nb).map(<[f64]>::to_vec).collect(),
        memory: memory.chunks(nb).map(<[Mat3<f64>]>::to_vec).collect(),
        initial,
    })
}

fn metadata(kind: Kind) -> Map<String, Value> {
    let d = kind.design();
    kind.units()
        .into_iter()
        .map(|(k, u)| (k.to_string(), json!({"unit": u, "differentiable": d, "design_reachable": d})))
        .collect()
}

fn descriptor(kind: Kind, analyses: &[&str], fields: &[&str], sensitivities: bool) -> ProviderDescriptor {
    let mut d = ProviderDescriptor::new(
        kind.name(),
        strings(analyses),
        kind.units().iter().map(|(k, _)| (*k).to_string()).collect(),
    );
    d.fields = strings(fields);
    d.sensitivities = sensitivities;
    d.nonlinear = true;
    d.notes = kind.notes();
    d.response_metadata = metadata(kind);
    d
}

fn presentation(d: &ProviderDescriptor, editor: Value) -> Result<ProviderCapabilities, CaeError> {
    Ok(ProviderCapabilities::Descriptor(Box::new(d.clone().with_presentation(editor, "array")?)))
}

fn static_template() -> Result<Value, CaeError> {
    let m = crate::solid_history::mesh([2, 2, 2])?;
    let scale = [0.2, 0.3, 0.4];
    let points: Vec<[f64; 3]> =
        m.ijk.iter().map(|p| std::array::from_fn(|a| p[a] as f64 * scale[a])).collect();
    let (lo, hi) = (0..3).fold(([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]), |(mut lo, mut hi), a| {
        for p in &points {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
        (lo, hi)
    });
    #[allow(clippy::float_cmp)]
    let boundary: Vec<bool> = points.iter().map(|p| (0..3).any(|a| p[a] == lo[a] || p[a] == hi[a])).collect();
    let displacement: Vec<[f64; 3]> =
        points.iter().map(|p| [p[0] * 1.05 - p[0], p[1] * 1.0 - p[1], p[2] * 1.0 - p[2]]).collect();
    let ne = m.tets.len();
    Ok(
        json!({"points": points, "elements": m.tets, "shear_Pa": vec![3000.0; ne], "lame_Pa": vec![5000.0; ne],
        "fixed_dofs": boundary.iter().map(|b| [*b; 3]).collect::<Vec<_>>(),
        "prescribed_displacement_m": displacement, "nodal_force_N": vec![[0.0; 3]; points.len()],
        "provenance": "Synthetic affine patch; not calibrated material data"}),
    )
}

fn static_properties() -> Map<String, Value> {
    [
        ("points", "Reference mesh nodes (m)", "Node-by-XYZ coordinates in metres."),
        ("elements", "Tetrahedral connectivity", "Zero-based node indices; positive orientation required."),
        ("shear_Pa", "Shear modulus (Pa)", "One positive value per tetrahedron."),
        ("lame_Pa", "Lame lambda (Pa)", "One nonnegative value per tetrahedron. Not bulk modulus."),
        (
            "fixed_dofs",
            "Prescribed displacement components",
            "Node-by-XYZ booleans: true fixes that displacement component.",
        ),
        (
            "prescribed_displacement_m",
            "Prescribed displacement (m)",
            "Only components marked true are imposed.",
        ),
        (
            "nodal_force_N",
            "Applied nodal dead loads (N)",
            "Reference-direction nodal forces; not follower pressure loads.",
        ),
    ]
    .iter()
    .map(|(k, title, description)| {
        ((*k).to_string(), json!({"title": title, "format": "json", "description": description}))
    })
    .collect()
}

fn history_series(rows: &[(&str, &str, &str)]) -> Value {
    json!({"kind": "scalar_time_histories", "time_field": "time_s",
        "series": rows.iter().map(|(k, l, u)| json!({"key": k, "label": l, "unit": u})).collect::<Vec<_>>()})
}

fn visco_template() -> Result<(Value, Map<String, Value>), CaeError> {
    let mut t = static_template()?;
    let m = t.as_object_mut().ok_or_else(|| CaeError::contract("template"))?;
    let u = m.remove("prescribed_displacement_m").unwrap_or(Value::Null);
    let f = m.remove("nodal_force_N").unwrap_or(Value::Null);
    let ne = m["elements"].as_array().map_or(0, Vec::len);
    let n = m["points"].as_array().map_or(0, Vec::len);
    let zeros = json!(vec![[0.0; 3]; n]);
    m.insert("prescribed_displacement_history_m".into(), json!([u, u, u, zeros]));
    m.insert("nodal_force_history_N".into(), json!([f, f, f, f]));
    m.insert("steps_s".into(), json!(([0.05; 4])));
    m.insert("branch_moduli_Pa".into(), json!(vec![[2000.0, 1000.0]; ne]));
    m.insert("relaxation_times_s".into(), json!(vec![[0.1, 1.0]; ne]));
    m.insert("initial_branch_strain".into(), json!(vec![[[[0.0; 3]; 3]; 2]; ne]));
    m.insert("initial_displacement_m".into(), zeros);
    let mut props = static_properties();
    props.remove("prescribed_displacement_m");
    props.remove("nodal_force_N");
    for (key, title, description) in [
        (
            "prescribed_displacement_history_m",
            "Displacement loading history (m)",
            "Step-by-node-by-XYZ end values; only fixed components are imposed. Include unloading steps to inspect recovery.",
        ),
        (
            "nodal_force_history_N",
            "Nodal force history (N)",
            "Step-by-node-by-XYZ dead loads; same number of steps as displacement.",
        ),
        (
            "steps_s",
            "Time increments (s)",
            "2..1000 strictly positive increments, one for each end-of-step state.",
        ),
        (
            "branch_moduli_Pa",
            "Relaxation branch moduli (Pa)",
            "Element-by-branch nonnegative coefficients of the full Green-strain tensor norm, not engineering shear moduli.",
        ),
        (
            "relaxation_times_s",
            "Relaxation times (s)",
            "Element-by-branch positive times; requires calibrated material data.",
        ),
        (
            "initial_branch_strain",
            "Initial material memory",
            "Element-by-branch-by-3-by-3 symmetric tensors. Zero means initially unrelaxed internal strain, not arbitrary stress-free initialization.",
        ),
        (
            "initial_displacement_m",
            "Initial displacement estimate (m)",
            "Node-by-XYZ starting estimate; fixed components follow the first prescribed step.",
        ),
        (
            "provenance",
            "Material and loading provenance",
            "Describe calibration, initial memory and prescribed loading history.",
        ),
    ] {
        props.insert(key.into(), json!({"title": title, "description": description, "format": if key == "provenance" { "text" } else { "json" }}));
    }
    Ok((t, props))
}

fn poro_template() -> Result<(Value, Map<String, Value>), CaeError> {
    let (mut t, mut props) = visco_template()?;
    let m = t.as_object_mut().ok_or_else(|| CaeError::contract("template"))?;
    let xs: Vec<f64> = m["points"]
        .as_array()
        .map(|a| a.iter().map(|p| p[0].as_f64().unwrap_or(0.0)).collect())
        .unwrap_or_default();
    let (n, ne) = (xs.len(), m["elements"].as_array().map_or(0, Vec::len));
    let nt = m["steps_s"].as_array().map_or(0, Vec::len);
    let negated: Vec<Value> = m["prescribed_displacement_history_m"]
        .as_array()
        .map(|steps| {
            steps
                .iter()
                .map(|s| {
                    json!(s.as_array().map(|rows| {
                        rows.iter()
                            .map(|r| [0, 1, 2].map(|k| -r[k].as_f64().unwrap_or(0.0)))
                            .collect::<Vec<_>>()
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    m.insert("prescribed_displacement_history_m".into(), json!(negated));
    let xmax = fmax(xs.iter().copied());
    #[allow(clippy::float_cmp)]
    let reservoir: Vec<bool> = xs.iter().map(|x| *x == xmax).collect();
    for (k, v) in [
        ("biot_coefficient", json!(vec![0.8; ne])),
        ("biot_modulus_Pa", json!(vec![10000.0; ne])),
        ("reference_mobility_m2_Pa_s", json!(vec![1e-6; ne])),
        ("initial_pressure_Pa", json!(vec![0.0; n])),
        ("fixed_pressure_nodes", json!(reservoir)),
        ("pressure_history_Pa", json!(vec![vec![0.0; n]; nt])),
        ("fluid_source_history_m3_s", json!(vec![vec![0.0; n]; nt])),
    ] {
        m.insert(k.into(), v);
    }
    for (key, title, description) in [
        (
            "biot_coefficient",
            "Biot coupling coefficient",
            "One value in [0,1] per tetrahedron. Fluid-volume increment is alpha*(J-1)+p/M.",
        ),
        (
            "biot_modulus_Pa",
            "Biot storage modulus (Pa)",
            "One strictly positive finite storage modulus per tetrahedron; not the drained solid bulk modulus.",
        ),
        (
            "reference_mobility_m2_Pa_s",
            "Reference Darcy mobility (m²/(Pa·s))",
            "One nonnegative value per tetrahedron. Defined in the fixed reference configuration; not intrinsic permeability alone.",
        ),
        (
            "initial_pressure_Pa",
            "Initial pore pressure (Pa)",
            "One signed gauge pressure per node, combined with initial displacement to establish initial fluid content.",
        ),
        (
            "fixed_pressure_nodes",
            "Reservoir pressure nodes",
            "Boolean node mask. True imposes pressure; false uses fluid balance. Unspecified exterior flux is zero.",
        ),
        (
            "pressure_history_Pa",
            "Reservoir pressure history (Pa)",
            "Step-by-node end values; only reservoir nodes are imposed.",
        ),
        (
            "fluid_source_history_m3_s",
            "Nodal fluid injection rates (m³/s)",
            "Step-by-node integrated nodal rates; positive injects fluid. These are not flux densities.",
        ),
    ] {
        props.insert(key.into(), json!({"title": title, "description": description, "format": "json"}));
    }
    Ok((t, props))
}

fn thermo_template() -> Result<Value, CaeError> {
    let (mut p, _) = poro_template()?;
    let m = p.as_object_mut().ok_or_else(|| CaeError::contract("template"))?;
    let provenance = m.remove("provenance").unwrap_or(Value::Null);
    let n = m["points"].as_array().map_or(0, Vec::len);
    let ne = m["elements"].as_array().map_or(0, Vec::len);
    let nt = m["steps_s"].as_array().map_or(0, Vec::len);
    let nb = m["branch_moduli_Pa"][0].as_array().map_or(0, Vec::len);
    let t = json!({"heat_capacity_J_m3_K": vec![1e6; ne], "reference_conductivity_W_m_K": vec![0.5; ne],
        "initial_temperature_K": vec![300.0; n], "fixed_temperature_nodes": vec![false; n],
        "temperature_history_K": vec![vec![300.0; n]; nt], "heat_source_history_W": vec![vec![0.0; n]; nt],
        "nodal_convection_W_K": vec![0.0; n], "ambient_temperature_history_K": vec![vec![300.0; n]; nt],
        "reference_temperature_K": vec![300.0; ne], "relaxation_activation_J_mol": vec![vec![10000.0; nb]; ne],
        "mobility_activation_J_mol": vec![10000.0; ne], "temperature_min_K": 250.0, "temperature_max_K": 400.0});
    Ok(json!({"poromechanics": p, "thermal": t, "provenance": provenance}))
}

fn gel_template() -> Value {
    let x = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let fixed = [[true, true, true], [false, true, true], [false, false, true], [false, false, false]];
    json!({"points": x, "elements": [[0, 1, 2, 3]], "network_shear_Pa": [40000.0], "volume_penalty_Pa": [1e8],
        "temperature_K": [293.15], "solvent_molar_volume_m3_mol": [super::hydrogel::R_GAS * 293.15 / 4e7], "chi": [0.1],
        "fixed_dofs": fixed, "prescribed_displacement_history_m": ([[[0.0; 3]; 4]; 2]),
        "nodal_force_history_N": ([[[0.0; 3]; 4]; 2]), "steps_s": [0.1, 0.1],
        "branch_moduli_Pa": [[0.0]], "relaxation_times_s": [[1.0]], "initial_branch_strain": ([[[[0.0; 3]; 3]]]),
        "initial_displacement_m": x, "reference_mobility_m2_Pa_s": [1e-6],
        "initial_solvent_volume": ([7.0; 4]), "initial_chemical_potential_Pa": ([-263_760.0; 4]),
        "fixed_chemical_nodes": ([true; 4]), "chemical_potential_history_Pa": ([[-200_000.0; 4], [0.0; 4]]),
        "solvent_source_history_m3_s": ([[0.0; 4]; 2]),
        "provenance": "Synthetic homogeneous swelling benchmark; not calibrated hydrogel data"})
}

fn design_capabilities(
    kind: Kind,
    history: &Value,
    material: &str,
    control: &Value,
    lower: f64,
    upper: f64,
) -> Value {
    let ones = ones_like(control);
    json!({"kind": "native_json",
        "design_template": {COORDINATE: {"value": control, "lower": lower, "upper": upper, "designable": ones}},
        "problem_template": {"history": history, "material": material, "filter_radius_m": 0.0,
            "design_region": ones, "fixed_design": control},
        "schema": {"type": "object", "properties": {
            "history": {"title": "Coupled loading and material history", "format": "json", "description": "Complete poroviscoelastic authored case, including calibration provenance."},
            "material": {"title": "Material field to optimize", "type": "string", "enum": kind.materials()},
            "filter_radius_m": {"title": "Spatial averaging radius", "type": "number", "minimum": 0, "unit": "m"},
            "design_region": {"title": "Designable material entries", "format": "json", "description": "Boolean array matching the selected elementwise material field; branches form the second axis where applicable."},
            "fixed_design": {"title": "Protected material values", "format": "json", "description": "Used wherever design_region is false. Bounds apply to raw control values; positive-radius filtering averages neighboring elements."}}}})
}

fn ones_like(v: &Value) -> Value {
    match v {
        Value::Array(a) => Value::Array(a.iter().map(ones_like).collect()),
        _ => Value::Bool(true),
    }
}

fn capabilities(kind: Kind) -> Result<ProviderCapabilities, CaeError> {
    let static_fields = ["displacement_m", "cauchy_stress_Pa", "jacobian", "support_reactions_N"];
    let visco_fields = [
        "time_s",
        "displacement_history_m",
        "cauchy_stress_history_Pa",
        "branch_strain_history",
        "support_reactions_history_N",
        "stored_energy_history_J",
        "dissipation_cumulative_J",
        "peak_displacement_history_m",
        "jacobian_history",
    ];
    let evaluation_traits = |preview: Option<Value>| {
        let mut t = obj(json!({"experimental": true, "evaluation_only": true}));
        if let Some(p) = preview {
            t.insert("history_preview".into(), p);
        }
        t
    };
    let visco_series = [
        ("stored_energy_history_J", "Stored energy", "J"),
        ("dissipation_cumulative_J", "Cumulative dissipation", "J"),
        ("peak_displacement_history_m", "Peak displacement", "m"),
    ];
    match kind {
        Kind::Static => {
            let mut d = descriptor(kind, &["finite_deformation_static_mechanics"], &static_fields, false);
            d.traits = evaluation_traits(None);
            presentation(
                &d,
                json!({"kind": "native_json", "title": "Finite-deformation solid, static neo-Hookean",
                    "problem_template": static_template()?, "schema": {"type": "object", "properties": static_properties()}}),
            )
        }
        Kind::Viscoelastic => {
            let (t, props) = visco_template()?;
            let mut d = descriptor(kind, &["finite_deformation_viscoelastic_history"], &visco_fields, false);
            d.traits = evaluation_traits(Some(history_series(&visco_series)));
            presentation(
                &d,
                json!({"kind": "native_json", "title": "Viscoelastic solid, loading, hold and unloading",
                    "problem_template": t, "schema": {"type": "object", "properties": props}}),
            )
        }
        Kind::Poro => {
            let (t, props) = poro_template()?;
            let mut fields: Vec<&str> = visco_fields.to_vec();
            fields.extend([
                "pressure_history_Pa",
                "fluid_content_history_m3",
                "reservoir_exchange_history_m3",
                "darcy_flux_history_m_s",
            ]);
            let mut d = descriptor(kind, &["finite_deformation_poroviscoelastic_history"], &fields, false);
            let mut series = visco_series.to_vec();
            series.push(("fluid_content_history_m3", "Fluid-volume increment", "m³"));
            d.traits = evaluation_traits(Some(history_series(&series)));
            presentation(
                &d,
                json!({"kind": "native_json", "title": "Poroviscoelastic solid, pressure, drainage and relaxation",
                    "problem_template": t, "schema": {"type": "object", "properties": props}}),
            )
        }
        Kind::Thermo => {
            let fields = [
                "time_s",
                "temperature_history_K",
                "displacement_history_m",
                "pressure_history_Pa",
                "ageing_extent_history",
                "damage_history",
                "damage_heat_power_history_W",
                "nonthermal_damage_energy_history_J",
                "maximum_temperature_history_K",
                "generated_heat_cumulative_J",
            ];
            let mut d = descriptor(kind, &[kind.name()], &fields, false);
            d.traits = evaluation_traits(Some(history_series(&[
                ("maximum_temperature_history_K", "Maximum temperature", "K"),
                ("generated_heat_cumulative_J", "Generated heat", "J"),
            ])));
            presentation(
                &d,
                json!({"kind": "native_json", "title": "Thermal poroviscoelasticity, heat, drainage and ageing",
                    "problem_template": thermo_template()?,
                    "schema": {"type": "object", "properties": {
                        "poromechanics": {"title": "Mechanical and drainage history", "format": "json"},
                        "thermal": {"title": "Thermal history and calibrated rate laws", "format": "json", "description": "Integrated nodal heat sources in W and convection in W/K. Optional ageing: element arrays initial_extent, rate_ref_s_inv, activation_J_mol, residual_stiffness, chemical_energy_J_m3, plus provenance. Optional damage: element arrays initial_damage, rate_s_inv, threshold_J_m3, residual_stiffness, integer material_region; positive scalar length_scale_m and provenance. Optional element heat_fraction in [0,1] defaults to 1; the remainder is tracked as nonthermal released energy, not qualified fracture energy. All rates use seconds, including years-long histories. Different region labels prevent nonlocal averaging between disconnected or distinct materials. Damage is evaluation-only."},
                        "provenance": {"title": "Calibration provenance", "format": "text"}}}}),
            )
        }
        Kind::Hydrogel => {
            let p = gel_template();
            let mut props: Map<String, Value> = p
                .as_object()
                .map(|m| {
                    m.keys()
                        .map(|k| (k.clone(), json!({"title": k.replace('_', " "), "format": if k == "provenance" { "text" } else { "json" }})))
                        .collect()
                })
                .unwrap_or_default();
            for (k, description) in [
                (
                    "initial_solvent_volume",
                    "Solvent volume per dry reference volume at each node; strictly positive and dimensionless.",
                ),
                (
                    "fixed_chemical_nodes",
                    "Reservoir node mask. Other nodes conserve solvent; unlisted exterior flux is zero.",
                ),
                (
                    "solvent_source_history_m3_s",
                    "Integrated nodal injection rates, positive inward; not flux density.",
                ),
            ] {
                if let Some(Value::Object(row)) = props.get_mut(k) {
                    row.insert("description".into(), json!(description));
                }
            }
            let fields = [
                "time_s",
                "displacement_history_m",
                "chemical_potential_history_Pa",
                "solvent_volume_history_m3",
                "free_energy_history_J",
                "jacobian_history",
                "branch_strain_history",
                "cauchy_stress_history_Pa",
            ];
            let mut d = descriptor(kind, &[kind.name()], &fields, false);
            d.traits = evaluation_traits(Some(history_series(&[
                ("solvent_volume_history_m3", "Solvent volume", "m³"),
                ("free_energy_history_J", "Free energy", "J"),
            ])));
            presentation(
                &d,
                json!({"kind": "native_json", "title": "Neutral hydrogel, chemical swelling and relaxation",
                    "problem_template": p, "schema": {"type": "object", "properties": props}}),
            )
        }
        Kind::PoroDesign | Kind::HydrogelDesign | Kind::ThermalDesign => {
            let (history, material, lower, upper, title, fields) = match kind {
                Kind::PoroDesign => (
                    poro_template()?.0,
                    "branch_moduli_Pa",
                    0.0,
                    10000.0,
                    "Poroviscoelastic material-layout optimization",
                    vec!["displacement_history_m", "pressure_history_Pa", "material_field"],
                ),
                Kind::HydrogelDesign => (
                    gel_template(),
                    "network_shear_Pa",
                    1000.0,
                    100_000.0,
                    "Hydrogel material-layout optimization",
                    vec!["displacement_history_m", "chemical_potential_history_Pa", "material_field"],
                ),
                _ => (
                    thermo_template()?,
                    "thermal.heat_capacity_J_m3_K",
                    1e5,
                    2e6,
                    "Thermal poroviscoelastic material optimization",
                    vec![
                        "temperature_history_K",
                        "displacement_history_m",
                        "ageing_extent_history",
                        "damage_history",
                        "material_field",
                    ],
                ),
            };
            let control = if kind == Kind::ThermalDesign {
                history["thermal"]["heat_capacity_J_m3_K"].clone()
            } else {
                history[material].clone()
            };
            let mut editor = design_capabilities(kind, &history, material, &control, lower, upper);
            editor["title"] = json!(title);
            if kind == Kind::ThermalDesign {
                editor["schema"]["properties"]["history"]["description"] = json!(
                    "Nested poromechanics and thermal settings, optional thermal.ageing and thermal.damage, and calibration provenance."
                );
            }
            let mut d = descriptor(kind, &["coupled_material_history_optimization"], &fields, true);
            d.design_coordinates = vec![COORDINATE.to_string()];
            d.traits = obj(json!({"experimental": true, "requires_explicit_design": true}));
            presentation(&d, editor)
        }
    }
}

fn orchestration_contract(kind: Kind) -> Result<AddInContract, CaeError> {
    let name = kind.name();
    let mut c = AddInContract::new(name);
    c.category = AddInCategory::Field;
    let design = kind.design();
    let port = format!("{name}.design.0");
    c.responses = kind
        .units()
        .into_iter()
        .map(|(r, u)| {
            let mut cap = ResponseCapability::new(r);
            cap.unit = u.into();
            cap.differentiable = Some(design);
            cap.design_reachable = Some(design);
            cap.depends_on = if design { vec![port.clone()] } else { Vec::new() };
            cap
        })
        .collect();
    c.scope = kind.scope();
    c.fidelity = Fidelity::Screening;
    c.runtime_route = RuntimeRoute::Array;
    c.exact_design_derivatives = Some(design);
    c.exact_state_transpose = Some(design);
    c.notes = kind.notes();
    c.contract_version = 2;
    c.compatibility_mode = false;
    c.owner_id = format!("provider:{name}");
    c.execution_kind = Some(ExecutionKind::Provider);
    c.supported_operations = if design {
        strings(&["preflight", "preflight_design", "evaluate", "sensitivity", "sensitivities", "optimize"])
    } else {
        strings(&["preflight", "evaluate"])
    };
    c.no_op_operations = Vec::new();
    if design {
        let mut d = DesignCoordinateRef::new(COORDINATE, port);
        d.addin_id = name.into();
        c.design_inputs = vec![d];
    }
    c.checked()
}

fn declaration(kind: Kind, problem: &Value) -> CouplingDeclaration {
    let mut active = strings(match kind {
        Kind::Static | Kind::Viscoelastic => &["mechanics"][..],
        Kind::Poro | Kind::PoroDesign => &["mechanics", "porous_flow"],
        Kind::Hydrogel | Kind::HydrogelDesign => &["mechanics", "solvent_transport"],
        Kind::Thermo | Kind::ThermalDesign => &["mechanics", "porous_flow", "thermal", "ageing"],
    });
    let thermal =
        if kind == Kind::ThermalDesign { &problem["history"]["thermal"] } else { &problem["thermal"] };
    if matches!(kind, Kind::Thermo | Kind::ThermalDesign) && thermal.get("damage").is_some() {
        active.push("damage".into());
    }
    CouplingDeclaration {
        provider: kind.name().into(),
        active_physics: active,
        ports: Vec::new(),
        edges: Vec::new(),
        closed_loops: Vec::new(),
        intentionally_frozen: Vec::new(),
        notes: kind.notes(),
    }
}

fn normalise(kind: Kind, problem: &Value) -> Result<Value, CaeError> {
    match kind {
        Kind::Static => {
            const REQUIRED: [&str; 8] = [
                "points",
                "elements",
                "shear_Pa",
                "lame_Pa",
                "fixed_dofs",
                "prescribed_displacement_m",
                "nodal_force_N",
                "provenance",
            ];
            let exact = problem
                .as_object()
                .is_some_and(|m| m.len() == REQUIRED.len() && REQUIRED.iter().all(|k| m.contains_key(*k)));
            if !exact || !nonblank(problem.get("provenance")) {
                return contract(
                    "Complete explicit mesh, material, boundary, load arrays and provenance required",
                );
            }
            solve_static(problem, true)?;
        }
        Kind::Viscoelastic => {
            const REQUIRED: [&str; 13] = [
                "points",
                "elements",
                "shear_Pa",
                "lame_Pa",
                "fixed_dofs",
                "prescribed_displacement_history_m",
                "nodal_force_history_N",
                "steps_s",
                "branch_moduli_Pa",
                "relaxation_times_s",
                "initial_branch_strain",
                "initial_displacement_m",
                "provenance",
            ];
            let exact = problem
                .as_object()
                .is_some_and(|m| m.len() == REQUIRED.len() && REQUIRED.iter().all(|k| m.contains_key(*k)));
            if !exact || !nonblank(problem.get("provenance")) {
                return contract("Complete explicit viscoelastic history and provenance required");
            }
            let v = visco_inputs(problem)?;
            let vs = ViscoelasticState {
                memory: v.memory.clone(),
                moduli: v.moduli.clone(),
                times: v.times.clone(),
                step: v.steps[0],
            };
            let initial = f64_shaped(&problem["initial_displacement_m"], &[v.base.mesh.node_count(), 3]);
            StaticProblem {
                mesh: &v.base.mesh,
                mu: &v.base.mu,
                lam: &v.base.lam,
                fixed: &v.base.fixed,
                prescribed: &v.prescribed[0],
                force: &v.force[0],
                initial: Some(initial.as_deref().unwrap_or(&[])),
                viscoelastic: Some(&vs),
                options: StaticOptions::default(),
            }
            .validate()?;
        }
        Kind::Poro => {
            if !problem.is_object() || !nonblank(problem.get("provenance")) {
                return contract("Explicit material/loading provenance required");
            }
            if problem.get("steps_s").and_then(Value::as_array).map_or(0, Vec::len) < 2 {
                return contract("At least two history samples required");
            }
            PoroProblem::parse(&without_provenance(problem))?;
        }
        Kind::Thermo => {
            let exact = problem.as_object().is_some_and(|m| {
                m.len() == 3
                    && m.contains_key("poromechanics")
                    && m.contains_key("thermal")
                    && m.contains_key("provenance")
            });
            if !exact || !nonblank(problem.get("provenance")) {
                return contract("Explicit coupled thermal history and provenance required");
            }
            ThermoProblem::parse(&without_provenance(problem))?;
        }
        Kind::Hydrogel => {
            if !problem.is_object() || !nonblank(problem.get("provenance")) {
                return contract("Explicit hydrogel calibration provenance required");
            }
            GelProblem::parse(&without_provenance(problem))?;
        }
        Kind::PoroDesign | Kind::HydrogelDesign | Kind::ThermalDesign => {
            let keys = ["history", "material", "filter_radius_m", "design_region", "fixed_design"];
            let exact = problem
                .as_object()
                .is_some_and(|m| m.len() == keys.len() && keys.iter().all(|k| m.contains_key(*k)));
            let material = problem.get("material").and_then(Value::as_str).unwrap_or_default();
            let (first, second) = match kind {
                Kind::PoroDesign => (
                    "Explicit history and supported material design map required",
                    "Invalid material-region mask, protected values or spatial radius",
                ),
                Kind::HydrogelDesign => (
                    "Explicit hydrogel history and supported material design map required",
                    "Invalid hydrogel material region, protected values or filter radius",
                ),
                _ => (
                    "Explicit thermal history and supported material design map required",
                    "Invalid thermal material region, protected values or filter radius",
                ),
            };
            if !exact || !kind.materials().iter().any(|m| m == material) {
                return contract(first);
            }
            let base = match kind {
                Kind::PoroDesign => Kind::Poro,
                Kind::HydrogelDesign => Kind::Hydrogel,
                _ => Kind::Thermo,
            };
            normalise(base, &problem["history"])?;
            let leaf = if kind == Kind::ThermalDesign {
                let (parent, key) = leaf(&problem["history"], material)?;
                match parent.get(key) {
                    Some(v) => v.clone(),
                    None => return contract("Selected material coefficient must be explicitly authored"),
                }
            } else {
                problem["history"][material].clone()
            };
            let shape = real_array(&leaf).map(|x| x.0).unwrap_or_default();
            let region = bool_array(&problem["design_region"]).map(|x| x.0);
            let fixed = real_array(&problem["fixed_design"])
                .filter(|(_, d)| d.iter().all(|v| v.is_finite()))
                .map(|x| x.0);
            let radius = problem["filter_radius_m"].as_f64().filter(|r| r.is_finite() && *r >= 0.0);
            if region.as_deref() != Some(&shape[..])
                || fixed.as_deref() != Some(&shape[..])
                || radius.is_none()
            {
                return contract(second);
            }
        }
    }
    Ok(problem.clone())
}

fn leaf<'a>(history: &'a Value, path: &'a str) -> Result<(&'a Value, &'a str), CaeError> {
    let keys: Vec<&str> = path.split('.').collect();
    let mut parent = history;
    for key in &keys[..keys.len() - 1] {
        match parent.get(*key) {
            Some(v) => parent = v,
            None => return contract("Selected optional material law is absent from history"),
        }
    }
    Ok((parent, keys[keys.len() - 1]))
}

fn evaluation(
    kind: Kind,
    responses: Vec<(&str, f64)>,
    diagnostics: Value,
    fields: Vec<(&str, FieldValue)>,
) -> Evaluation {
    Evaluation {
        provider: kind.name().into(),
        responses: responses.into_iter().map(|(k, v)| (k.to_string(), v)).collect::<BTreeMap<_, _>>(),
        diagnostics: obj(diagnostics),
        fields: fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    }
}

fn stack(rows: &[Vec<f64>], tail: &[usize]) -> Result<FieldValue, CaeError> {
    let mut shape = vec![rows.len()];
    shape.extend(tail);
    field(rows.iter().flatten().copied().collect(), &shape)
}

#[allow(clippy::too_many_lines)]
fn evaluate(kind: Kind, p: &Value) -> Result<Evaluation, CaeError> {
    let provenance = p.get("provenance").cloned().unwrap_or(Value::Null);
    match kind {
        Kind::Static => {
            let out = solve_static(p, false)?.ok_or_else(|| CaeError::contract("static solve"))?;
            let (n, ne) = (out.displacement.len(), out.jacobian.len());
            let u: Vec<f64> = out.displacement.iter().flatten().copied().collect();
            Ok(evaluation(
                kind,
                vec![
                    ("stored_energy_J", out.stored_energy),
                    ("peak_displacement_m", peak(&u)),
                    ("minimum_J", fmin(out.jacobian.iter().copied())),
                    ("free_residual_N", out.free_residual),
                ],
                json!({"iterations": out.iterations, "limitations": STATIC_LIMITS, "physical_qualification": false, "provenance": provenance}),
                vec![
                    ("displacement_m", field(u, &[n, 3])?),
                    ("cauchy_stress_Pa", field(mats(&out.cauchy_stress), &[ne, 3, 3])?),
                    ("jacobian", field(out.jacobian.clone(), &[ne])?),
                    (
                        "support_reactions_N",
                        field(out.support_reactions.iter().flatten().copied().collect(), &[n, 3])?,
                    ),
                ],
            ))
        }
        Kind::Viscoelastic => {
            let v = visco_inputs(p)?;
            let records = solve_viscoelastic_history(
                &v.base.mesh,
                &v.base.mu,
                &v.base.lam,
                &v.base.fixed,
                &v.prescribed,
                &v.force,
                &v.moduli,
                &v.times,
                &v.steps,
                v.memory.clone(),
                Some(v.initial.clone()),
            )?;
            let (n, ne) = (v.base.mesh.node_count(), v.base.mesh.elements.len());
            let nb = v.moduli.first().map_or(0, Vec::len);
            let disp: Vec<Vec<f64>> =
                records.iter().map(|r| r.displacement.iter().flatten().copied().collect()).collect();
            let peaks: Vec<f64> = disp.iter().map(|u| peak(u)).collect();
            let dissipation: Vec<f64> =
                records.iter().map(|r| r.dissipation_increment.unwrap_or(0.0)).collect();
            let jac: Vec<Vec<f64>> = records.iter().map(|r| r.jacobian.clone()).collect();
            let times = cumsum(&v.steps);
            Ok(evaluation(
                kind,
                vec![
                    ("dissipation_J", dissipation.iter().sum()),
                    ("final_stored_energy_J", records.last().map_or(f64::NAN, |r| r.stored_energy)),
                    ("peak_displacement_m", fmax(peaks.iter().copied())),
                    ("minimum_J", fmin(jac.iter().flatten().copied())),
                    ("free_residual_N", fmax(records.iter().map(|r| r.free_residual))),
                ],
                json!({"limitations": VISCO_LIMITS, "physical_qualification": false, "provenance": provenance,
                    "iterations_by_step": records.iter().map(|r| r.iterations.clone()).collect::<Vec<_>>(),
                    "optimization_supported": false}),
                vec![
                    ("time_s", field(times, &[v.steps.len()])?),
                    ("displacement_history_m", stack(&disp, &[n, 3])?),
                    (
                        "cauchy_stress_history_Pa",
                        stack(
                            &records.iter().map(|r| mats(&r.cauchy_stress)).collect::<Vec<_>>(),
                            &[ne, 3, 3],
                        )?,
                    ),
                    (
                        "support_reactions_history_N",
                        stack(
                            &records
                                .iter()
                                .map(|r| r.support_reactions.iter().flatten().copied().collect())
                                .collect::<Vec<_>>(),
                            &[n, 3],
                        )?,
                    ),
                    (
                        "branch_strain_history",
                        stack(
                            &records
                                .iter()
                                .map(|r| {
                                    r.branch_strain
                                        .as_ref()
                                        .map(|b| b.iter().flat_map(|e| mats(e)).collect())
                                        .unwrap_or_default()
                                })
                                .collect::<Vec<_>>(),
                            &[ne, nb, 3, 3],
                        )?,
                    ),
                    ("jacobian_history", stack(&jac, &[ne])?),
                    (
                        "stored_energy_history_J",
                        field(records.iter().map(|r| r.stored_energy).collect(), &[records.len()])?,
                    ),
                    ("dissipation_cumulative_J", field(cumsum(&dissipation), &[records.len()])?),
                    ("peak_displacement_history_m", field(peaks, &[records.len()])?),
                ],
            ))
        }
        Kind::Poro => {
            let problem = PoroProblem::parse(&without_provenance(p))?;
            let out = problem.solve_history()?;
            let r = &out.steps;
            let (n, ne) = (problem.mesh.node_count(), problem.mesh.elements.len());
            let nb = problem.moduli.first().map_or(0, Vec::len);
            let nt = r.len();
            let disp: Vec<Vec<f64>> = r.iter().map(|s| s.displacement.clone()).collect();
            let peaks: Vec<f64> = disp.iter().map(|u| peak(u)).collect();
            let dissipation = cumsum(
                &r.iter().map(|s| s.darcy_dissipation + s.viscoelastic_dissipation).collect::<Vec<_>>(),
            );
            let stored: Vec<f64> = r.iter().map(|s| s.stored_energy).collect();
            let exchange: Vec<Vec<f64>> = r.iter().map(|s| s.reservoir_exchange.clone()).collect();
            Ok(evaluation(
                kind,
                vec![
                    ("dissipation_J", dissipation[nt - 1]),
                    ("final_stored_energy_J", stored[nt - 1]),
                    ("peak_displacement_m", fmax(peaks.iter().copied())),
                    ("minimum_J", fmin(r.iter().flat_map(|s| s.jacobian.iter().copied()))),
                    (
                        "maximum_fluid_balance_error_m3",
                        fmax(r.iter().map(|s| s.free_fluid_balance.max(s.global_fluid_balance.abs()))),
                    ),
                    ("net_reservoir_exchange_m3", exchange.iter().flatten().sum()),
                ],
                json!({"limitations": PORO_LIMITS, "physical_qualification": false, "provenance": provenance,
                    "scaled_free_residual_history": r.iter().map(|s| s.scaled_free_residual).collect::<Vec<_>>(),
                    "initial_fluid_content_increment_m3": out.initial_content, "optimization_supported": false}),
                vec![
                    ("time_s", field(out.times.clone(), &[nt])?),
                    ("displacement_history_m", stack(&disp, &[n, 3])?),
                    (
                        "cauchy_stress_history_Pa",
                        stack(&r.iter().map(|s| mats(&s.cauchy_stress)).collect::<Vec<_>>(), &[ne, 3, 3])?,
                    ),
                    (
                        "support_reactions_history_N",
                        stack(&r.iter().map(|s| s.support_reactions.clone()).collect::<Vec<_>>(), &[n, 3])?,
                    ),
                    (
                        "branch_strain_history",
                        stack(
                            &r.iter()
                                .map(|s| s.branch_strain.iter().flat_map(|e| mats(e)).collect())
                                .collect::<Vec<_>>(),
                            &[ne, nb, 3, 3],
                        )?,
                    ),
                    (
                        "jacobian_history",
                        stack(&r.iter().map(|s| s.jacobian.clone()).collect::<Vec<_>>(), &[ne])?,
                    ),
                    ("stored_energy_history_J", field(stored, &[nt])?),
                    (
                        "pressure_history_Pa",
                        stack(&r.iter().map(|s| s.pressure.clone()).collect::<Vec<_>>(), &[n])?,
                    ),
                    ("fluid_content_history_m3", field(r.iter().map(|s| s.fluid_content).collect(), &[nt])?),
                    ("reservoir_exchange_history_m3", stack(&exchange, &[n])?),
                    (
                        "darcy_flux_history_m_s",
                        stack(
                            &r.iter()
                                .map(|s| s.darcy_flux.iter().flatten().copied().collect())
                                .collect::<Vec<_>>(),
                            &[ne, 3],
                        )?,
                    ),
                    ("peak_displacement_history_m", field(peaks, &[nt])?),
                    ("dissipation_cumulative_J", field(dissipation, &[nt])?),
                ],
            ))
        }
        Kind::Thermo => {
            let problem = ThermoProblem::parse(&without_provenance(p))?;
            let out = problem.solve_history()?;
            let r = &out.steps;
            let (n, ne, nt) = (problem.poro.mesh.node_count(), problem.poro.mesh.elements.len(), r.len());
            let temperature: Vec<Vec<f64>> = r.iter().map(|s| s.temperature.clone()).collect();
            let generated = cumsum(&r.iter().map(|s| s.generated_heat).collect::<Vec<_>>());
            Ok(evaluation(
                kind,
                vec![
                    ("generated_heat_J", generated[nt - 1]),
                    ("maximum_temperature_K", fmax(temperature.iter().flatten().copied())),
                    ("minimum_J", fmin(r.iter().flat_map(|s| s.poro.jacobian.iter().copied()))),
                    (
                        "maximum_heat_balance_error_J",
                        fmax(r.iter().map(|s| s.heat_balance_error.abs().max(s.free_heat_balance_error))),
                    ),
                    (
                        "maximum_fluid_balance_error_m3",
                        fmax(
                            r.iter()
                                .map(|s| s.poro.free_fluid_balance.max(s.poro.global_fluid_balance.abs())),
                        ),
                    ),
                ],
                json!({"limitations": THERMO_LIMITS, "physical_qualification": false, "optimization_supported": false,
                    "provenance": provenance}),
                vec![
                    ("time_s", field(out.times.clone(), &[nt])?),
                    (
                        "maximum_temperature_history_K",
                        field(temperature.iter().map(|t| fmax(t.iter().copied())).collect(), &[nt])?,
                    ),
                    ("temperature_history_K", stack(&temperature, &[n])?),
                    (
                        "displacement_history_m",
                        stack(&r.iter().map(|s| s.poro.displacement.clone()).collect::<Vec<_>>(), &[n, 3])?,
                    ),
                    (
                        "pressure_history_Pa",
                        stack(&r.iter().map(|s| s.poro.pressure.clone()).collect::<Vec<_>>(), &[n])?,
                    ),
                    (
                        "ageing_extent_history",
                        stack(&r.iter().map(|s| s.ageing_extent.clone()).collect::<Vec<_>>(), &[ne])?,
                    ),
                    (
                        "damage_history",
                        stack(&r.iter().map(|s| s.damage.clone()).collect::<Vec<_>>(), &[ne])?,
                    ),
                    ("generated_heat_cumulative_J", field(generated, &[nt])?),
                    (
                        "damage_heat_power_history_W",
                        field(r.iter().map(|s| s.damage_heat_power).collect(), &[nt])?,
                    ),
                    (
                        "nonthermal_damage_energy_history_J",
                        field(r.iter().map(|s| s.nonthermal_damage_energy).collect(), &[nt])?,
                    ),
                ],
            ))
        }
        Kind::Hydrogel => {
            let problem = GelProblem::parse(&without_provenance(p))?;
            let out = problem.solve_history()?;
            let r = &out.steps;
            let (n, ne, nt) = (problem.mesh.node_count(), problem.mesh.elements.len(), r.len());
            let nb = problem.moduli.first().map_or(0, Vec::len);
            let jac: Vec<Vec<f64>> = r.iter().map(|s| s.jacobian.clone()).collect();
            Ok(evaluation(
                kind,
                vec![
                    ("final_solvent_volume_m3", r[nt - 1].solvent_volume),
                    ("final_free_energy_J", r[nt - 1].free_energy),
                    ("minimum_J", fmin(jac.iter().flatten().copied())),
                    (
                        "maximum_solvent_balance_error_m3",
                        fmax(r.iter().map(|s| s.free_solvent_balance.max(s.global_solvent_balance.abs()))),
                    ),
                    ("maximum_volume_constraint_error", fmax(r.iter().map(|s| s.volume_constraint_error))),
                ],
                json!({"limitations": GEL_LIMITS, "physical_qualification": false, "optimization_supported": false,
                    "provenance": provenance}),
                vec![
                    ("time_s", field(out.times.clone(), &[nt])?),
                    (
                        "displacement_history_m",
                        stack(&r.iter().map(|s| s.displacement.clone()).collect::<Vec<_>>(), &[n, 3])?,
                    ),
                    (
                        "chemical_potential_history_Pa",
                        stack(&r.iter().map(|s| s.chemical.clone()).collect::<Vec<_>>(), &[n])?,
                    ),
                    (
                        "solvent_volume_history_m3",
                        field(r.iter().map(|s| s.solvent_volume).collect(), &[nt])?,
                    ),
                    ("free_energy_history_J", field(r.iter().map(|s| s.free_energy).collect(), &[nt])?),
                    ("jacobian_history", stack(&jac, &[ne])?),
                    (
                        "branch_strain_history",
                        stack(
                            &r.iter()
                                .map(|s| s.branch_strain.iter().flat_map(|e| mats(e)).collect())
                                .collect::<Vec<_>>(),
                            &[ne, nb, 3, 3],
                        )?,
                    ),
                    (
                        "cauchy_stress_history_Pa",
                        stack(&r.iter().map(|s| mats(&s.cauchy_stress)).collect::<Vec<_>>(), &[ne, 3, 3])?,
                    ),
                ],
            ))
        }
        _ => Err(missing(kind, "evaluate")),
    }
}

fn missing(kind: Kind, method: &str) -> CaeError {
    CaeError::contract(format!("'{}' object has no attribute '{method}'", kind.class()))
}

struct Mapped {
    problem: Value,
    history: Value,
    material: String,
    field: Vec<f64>,
    shape: Vec<usize>,
    weights: Vec<Vec<f64>>,
    region: Vec<bool>,
}

fn mapped(kind: Kind, problem: &Value, design: &NamedArrays) -> Result<Mapped, CaeError> {
    let p = normalise(kind, problem)?;
    let message = if kind == Kind::ThermalDesign {
        "Explicit thermal material controls required"
    } else {
        "Explicit material control array required"
    };
    let raw = match design.first() {
        Some((name, v)) if design.len() == 1 && name == COORDINATE => v,
        _ => return contract(message),
    };
    let (shape, fixed) = real_array(&p["fixed_design"]).unwrap_or_default();
    let (_, region) = bool_array(&p["design_region"]).unwrap_or_default();
    if raw.shape() != shape.as_slice() || raw.iter().any(|v| !v.is_finite()) {
        return contract("Finite shape-matched material controls required");
    }
    let raw: Vec<f64> = raw.iter().copied().collect();
    let history_mesh =
        if kind == Kind::ThermalDesign { &p["history"]["poromechanics"] } else { &p["history"] };
    let points: Vec<[f64; 3]> = real_array(&history_mesh["points"])
        .map(|(_, d)| d.chunks(3).map(|c| [c[0], c[1], c[2]]).collect())
        .unwrap_or_default();
    let elements: Vec<[usize; 4]> = int_array(&history_mesh["elements"])
        .map(|(_, d)| {
            d.chunks(4).map(|c| std::array::from_fn(|i| usize::try_from(c[i]).unwrap_or(0))).collect()
        })
        .unwrap_or_default();
    let material = p["material"].as_str().unwrap_or_default().to_string();
    let centres: Vec<[f64; 3]> = if material == "thermal.nodal_convection_W_K" {
        points.clone()
    } else {
        elements
            .iter()
            .map(|t| {
                std::array::from_fn(|a| {
                    (points[t[0]][a] + points[t[1]][a] + points[t[2]][a] + points[t[3]][a]) / 4.0
                })
            })
            .collect()
    };
    let radius = p["filter_radius_m"].as_f64().unwrap_or(0.0);
    let m = centres.len();
    let weights: Vec<Vec<f64>> = (0..m)
        .map(|i| {
            let row: Vec<f64> = (0..m)
                .map(|j| {
                    #[allow(clippy::float_cmp)]
                    if radius == 0.0 {
                        if i == j { 1.0 } else { 0.0 }
                    } else {
                        let d = (0..3).map(|a| (centres[i][a] - centres[j][a]).powi(2)).sum::<f64>().sqrt();
                        (radius - d).max(0.0)
                    }
                })
                .collect();
            let total: f64 = row.iter().sum();
            row.iter().map(|w| w / total).collect()
        })
        .collect();
    let width = fixed.len().checked_div(m).unwrap_or(0);
    let source: Vec<f64> = (0..fixed.len()).map(|k| if region[k] { raw[k] } else { fixed[k] }).collect();
    let field: Vec<f64> = (0..fixed.len())
        .map(|k| {
            if region[k] {
                let (i, b) = (k / width, k % width);
                (0..m).map(|j| weights[i][j] * source[j * width + b]).sum()
            } else {
                fixed[k]
            }
        })
        .collect();
    let mut history = p["history"].clone();
    let value = nested(&field, &shape);
    if kind == Kind::ThermalDesign {
        let keys: Vec<&str> = material.split('.').collect();
        let mut parent = &mut history;
        for key in &keys[..keys.len() - 1] {
            parent = &mut parent[*key];
        }
        parent[keys[keys.len() - 1]] = value;
    } else {
        history[material.as_str()] = value;
    }
    Ok(Mapped { problem: p, history, material, field, shape, weights, region })
}

fn nested(values: &[f64], shape: &[usize]) -> Value {
    crate::util::nested(shape, values)
}

impl Mapped {
    fn pullback(&self, gradient: &[f64]) -> Vec<f64> {
        let m = self.weights.len();
        let width = gradient.len().checked_div(m).unwrap_or(0);
        let masked: Vec<f64> =
            gradient.iter().zip(&self.region).map(|(g, r)| if *r { *g } else { 0.0 }).collect();
        (0..gradient.len())
            .map(|k| {
                if self.region[k] {
                    let (j, b) = (k / width, k % width);
                    (0..m).map(|i| self.weights[i][j] * masked[i * width + b]).sum()
                } else {
                    0.0
                }
            })
            .collect()
    }
}

fn sensitivity_value(
    kind: Kind,
    history: &Value,
    material: &str,
    response: &str,
) -> Result<(f64, Vec<f64>), CaeError> {
    let bare = without_provenance(history);
    match kind {
        Kind::PoroDesign => {
            super::poro::material_sensitivity(&PoroProblem::parse(&bare)?, material, response)
        }
        Kind::HydrogelDesign => {
            super::hydrogel::material_sensitivity(&GelProblem::parse(&bare)?, material, response)
        }
        _ => {
            let r = super::thermo::material_sensitivity(
                &ThermoProblem::parse(&bare)?,
                &bare,
                material,
                response,
            )?;
            Ok((r.value, r.gradient))
        }
    }
}

#[allow(clippy::too_many_lines)]
fn evaluate_design(kind: Kind, problem: &Value, design: &NamedArrays) -> Result<Evaluation, CaeError> {
    let m = mapped(kind, problem, design)?;
    let h = &m.history;
    let provenance = h.get("provenance").cloned().unwrap_or(Value::Null);
    let material_field = field(m.field.clone(), &m.shape)?;
    match kind {
        Kind::PoroDesign => {
            let problem = PoroProblem::parse(&without_provenance(h))?;
            let out = problem.solve_history()?;
            let r = &out.steps;
            let last = &r[r.len() - 1];
            let n = problem.mesh.node_count();
            Ok(evaluation(
                kind,
                vec![
                    (
                        "dissipation_J",
                        r.iter().map(|s| s.darcy_dissipation + s.viscoelastic_dissipation).sum(),
                    ),
                    ("mechanical_work_J", last.mechanical_work),
                    ("final_stored_energy_J", last.stored_energy),
                    ("final_displacement_squared_m2", last.displacement.iter().map(|u| u * u).sum()),
                    ("final_fluid_content_increment_m3", last.fluid_content),
                ],
                json!({"notes": PORO_DESIGN_NOTES, "physical_qualification": false,
                    "kinematic_cycle_closure": last.kinematic_cycle_closure, "provenance": provenance}),
                vec![
                    (
                        "displacement_history_m",
                        stack(&r.iter().map(|s| s.displacement.clone()).collect::<Vec<_>>(), &[n, 3])?,
                    ),
                    (
                        "pressure_history_Pa",
                        stack(&r.iter().map(|s| s.pressure.clone()).collect::<Vec<_>>(), &[n])?,
                    ),
                    ("material_field", material_field),
                ],
            ))
        }
        Kind::HydrogelDesign => {
            let problem = GelProblem::parse(&without_provenance(h))?;
            let out = problem.solve_history()?;
            let r = &out.steps;
            let last = &r[r.len() - 1];
            let n = problem.mesh.node_count();
            Ok(evaluation(
                kind,
                vec![
                    ("final_solvent_volume_m3", last.solvent_volume),
                    ("final_free_energy_J", last.free_energy),
                    (
                        "dissipation_J",
                        r.iter().map(|s| s.diffusion_dissipation + s.viscoelastic_dissipation).sum(),
                    ),
                    ("final_displacement_squared_m2", last.displacement.iter().map(|u| u * u).sum()),
                ],
                json!({"notes": GEL_DESIGN_NOTES, "physical_qualification": false, "provenance": provenance,
                    "maximum_volume_constraint_error": fmax(r.iter().map(|s| s.volume_constraint_error))}),
                vec![
                    (
                        "displacement_history_m",
                        stack(&r.iter().map(|s| s.displacement.clone()).collect::<Vec<_>>(), &[n, 3])?,
                    ),
                    (
                        "chemical_potential_history_Pa",
                        stack(&r.iter().map(|s| s.chemical.clone()).collect::<Vec<_>>(), &[n])?,
                    ),
                    ("material_field", material_field),
                ],
            ))
        }
        _ => {
            let problem = ThermoProblem::parse(&without_provenance(h))?;
            let out = problem.solve_history()?;
            let r = &out.steps;
            let last = &r[r.len() - 1];
            let mesh = &problem.poro.mesh;
            let (n, ne) = (mesh.node_count(), mesh.elements.len());
            let volume = &mesh.volumes;
            let cv = &problem.capacity;
            let (mut num, mut den) = (0.0, 0.0);
            for (e, t) in mesh.elements.iter().enumerate() {
                let mean = (last.temperature[t[0]]
                    + last.temperature[t[1]]
                    + last.temperature[t[2]]
                    + last.temperature[t[3]])
                    / 4.0;
                num += volume[e] * cv[e] * mean;
                den += volume[e] * cv[e];
            }
            let vsum: f64 = volume.iter().sum();
            let dotv = |x: &[f64]| volume.iter().zip(x).map(|(a, b)| a * b).sum::<f64>();
            Ok(evaluation(
                kind,
                vec![
                    ("final_mean_temperature_K", num / den),
                    ("generated_heat_J", r.iter().map(|s| s.generated_heat).sum()),
                    ("final_displacement_squared_m2", last.poro.displacement.iter().map(|u| u * u).sum()),
                    ("final_mean_ageing_extent", dotv(&last.ageing_extent) / vsum),
                    ("final_mean_damage", dotv(&last.damage) / vsum),
                ],
                json!({"notes": THERMAL_DESIGN_NOTES, "physical_qualification": false, "provenance": provenance}),
                vec![
                    (
                        "temperature_history_K",
                        stack(&r.iter().map(|s| s.temperature.clone()).collect::<Vec<_>>(), &[n])?,
                    ),
                    (
                        "displacement_history_m",
                        stack(&r.iter().map(|s| s.poro.displacement.clone()).collect::<Vec<_>>(), &[n, 3])?,
                    ),
                    (
                        "ageing_extent_history",
                        stack(&r.iter().map(|s| s.ageing_extent.clone()).collect::<Vec<_>>(), &[ne])?,
                    ),
                    (
                        "damage_history",
                        stack(&r.iter().map(|s| s.damage.clone()).collect::<Vec<_>>(), &[ne])?,
                    ),
                    ("material_field", material_field),
                ],
            ))
        }
    }
}

fn sensitivities_design(
    kind: Kind,
    problem: &Value,
    design: &NamedArrays,
    responses: &[String],
) -> Result<DesignSensitivities, CaeError> {
    let units = kind.units();
    let mut unique = responses.to_vec();
    unique.sort();
    unique.dedup();
    let message = match kind {
        Kind::PoroDesign => "Unique supported history responses required",
        Kind::HydrogelDesign => "Unique supported hydrogel responses required",
        _ => "Unique supported thermal responses required",
    };
    if responses.is_empty()
        || unique.len() != responses.len()
        || responses.iter().any(|r| !units.iter().any(|(k, _)| k == r))
    {
        return contract(message);
    }
    let m = mapped(kind, problem, design)?;
    let mut values = BTreeMap::new();
    let mut gradients = BTreeMap::new();
    for response in responses {
        let (value, gradient) = sensitivity_value(kind, &m.history, &m.material, response)?;
        let pulled = m.pullback(&gradient);
        values.insert(response.clone(), value);
        gradients.insert(response.clone(), NamedArrays::single(COORDINATE, arr(pulled, &m.shape)?));
    }
    let _ = &m.problem;
    Ok(DesignSensitivities {
        responses: values,
        gradients,
        diagnostics: obj(json!({"notes": kind.notes(), "physical_qualification": false})),
    })
}

#[derive(Debug, Clone, Copy)]
pub struct HyperelasticProvider(pub Kind);

fn problem_value(problem: &ProviderProblem) -> Result<&Value, CaeError> {
    problem
        .downcast_ref::<Value>()
        .ok_or_else(|| CaeError::contract("hyperelastic field providers require their own problem mapping"))
}

fn no_design(topology: Option<&ArrayD<f64>>) -> Result<(), CaeError> {
    if topology.is_some_and(|t| !t.is_empty()) {
        return contract("This field provider is evaluation-only");
    }
    Ok(())
}

impl CaeProvider for HyperelasticProvider {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn implementation(&self) -> &str {
        match self.0 {
            Kind::Static => "implexity.addins.hyperelastic_field.HyperelasticFieldProvider",
            Kind::Viscoelastic => "implexity.addins.hyperelastic_field.ViscoelasticFieldProvider",
            Kind::Poro => "implexity.addins.hyperelastic_field.PoroViscoelasticFieldProvider",
            Kind::PoroDesign => "implexity.addins.hyperelastic_field.PoroMaterialDesignProvider",
            Kind::Thermo => "implexity.addins.hyperelastic_field.ThermoPoroFieldProvider",
            Kind::Hydrogel => "implexity.addins.hyperelastic_field.HydrogelFieldProvider",
            Kind::HydrogelDesign => "implexity.addins.hyperelastic_field.HydrogelMaterialDesignProvider",
            Kind::ThermalDesign => "implexity.addins.hyperelastic_field.ThermalMaterialDesignProvider",
        }
    }

    fn capabilities(&self) -> Result<ProviderCapabilities, CaeError> {
        capabilities(self.0)
    }

    fn orchestration_contract(&self) -> Option<Result<PublishedContract, CaeError>> {
        Some(orchestration_contract(self.0).map(|c| PublishedContract::Contract(Box::new(c))))
    }

    fn normalise_problem(&self, problem: &Value) -> Result<ProviderProblem, CaeError> {
        Ok(Arc::new(normalise(self.0, problem)?))
    }

    fn preflight(
        &self,
        problem: &ProviderProblem,
        topology: Option<&ArrayD<f64>>,
    ) -> Result<Map<String, Value>, CaeError> {
        let p = problem_value(problem)?;
        if self.0.design() {
            normalise(self.0, p)?;
            return Ok(obj(
                json!({"ok": true, "requires_complete_design": true, "physical_qualification": false}),
            ));
        }
        no_design(topology)?;
        normalise(self.0, p)?;
        Ok(obj(
            json!({"ok": true, "equilibrium_solved": false, "optimization_supported": false, "limitations": self.0.notes()}),
        ))
    }

    fn evaluate(&self, problem: &ProviderProblem, topology: &ArrayD<f64>) -> Result<Evaluation, CaeError> {
        if self.0.design() {
            return Err(missing(self.0, "evaluate"));
        }
        no_design(Some(topology))?;
        let p = normalise(self.0, problem_value(problem)?)?;
        evaluate(self.0, &p)
    }

    fn sensitivity(
        &self,
        _problem: &ProviderProblem,
        _topology: &ArrayD<f64>,
        _response: &str,
    ) -> Result<Sensitivity, CaeError> {
        Err(missing(self.0, "sensitivity"))
    }

    fn coupling_declaration(&self, problem: Option<&ProviderProblem>) -> Option<Result<Value, CaeError>> {
        let p = problem.and_then(|p| p.downcast_ref::<Value>()).cloned().unwrap_or_else(|| json!({}));
        Some(Ok(declaration(self.0, &p).to_value()))
    }

    fn interface(&self, name: &str) -> Option<&(dyn Any + Send + Sync)> {
        implexity_optim::provider_ops::design_interface::<Self>(name)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl DesignOperations for HyperelasticProvider {
    fn provides(&self, op: DesignOp) -> bool {
        if self.0.design() {
            matches!(
                op,
                DesignOp::EvaluateDesign
                    | DesignOp::PreflightDesign
                    | DesignOp::SensitivityDesign
                    | DesignOp::SensitivitiesDesign
                    | DesignOp::CandidateDesignAdmission
                    | DesignOp::OptimizerLifecycle
            )
        } else {
            matches!(op, DesignOp::Evaluate | DesignOp::EvaluateWithoutDesign)
        }
    }

    fn evaluate_without_design(&self, problem: &ProviderProblem) -> Result<Evaluation, CaeError> {
        let p = normalise(self.0, problem_value(problem)?)?;
        evaluate(self.0, &p)
    }

    fn evaluate_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        _operating_point: usize,
    ) -> Result<Evaluation, CaeError> {
        evaluate_design(self.0, problem_value(problem)?, design)
    }

    fn preflight_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
    ) -> Result<Map<String, Value>, CaeError> {
        let p = problem_value(problem)?;
        if self.0 == Kind::PoroDesign {
            let m = mapped(self.0, p, design)?;
            evaluate(Kind::Poro, &m.history)?;
        } else {
            evaluate_design(self.0, p, design)?;
        }
        Ok(obj(json!({"ok": true, "physical_qualification": false})))
    }

    fn sensitivity_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        response: &str,
        _operating_point: usize,
    ) -> Result<DesignSensitivity, CaeError> {
        let mut out = sensitivities_design(self.0, problem_value(problem)?, design, &[response.to_string()])?;
        Ok(DesignSensitivity {
            value: out.responses.get(response).copied().unwrap_or(f64::NAN),
            gradients: out.gradients.remove(response).unwrap_or_default(),
            diagnostics: out.diagnostics,
        })
    }

    fn sensitivities_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        responses: &[String],
        _operating_point: usize,
    ) -> Result<DesignSensitivities, CaeError> {
        sensitivities_design(self.0, problem_value(problem)?, design, responses)
    }

    fn candidate_admission(
        &self,
        op: DesignOp,
        problem: &ProviderProblem,
        current: &CandidateDesign,
        trial: &CandidateDesign,
    ) -> Result<AdmissionReply, CaeError> {
        if op != DesignOp::CandidateDesignAdmission {
            return Err(CaeError::contract(format!("provider operation {:?} is unavailable", op.name())));
        }
        let named = |c: &CandidateDesign| match c {
            CandidateDesign::Named(n) => Ok(n.clone()),
            CandidateDesign::Array(_) => {
                contract("candidate named designs require nonempty text coordinate ids")
            }
        };
        let (current, trial) = (named(current)?, named(trial)?);
        let evidence = (design_identity(&current)?, design_identity(&trial)?);
        let reply = match evaluate_design(self.0, problem_value(problem)?, &trial) {
            Err(e) => json!({"current_design_state_id": evidence.0, "candidate_design_state_id": evidence.1,
                "allow": false, "reason": e.message(), "diagnostics": {"history_admitted": false}}),
            Ok(_) => json!({"current_design_state_id": evidence.0, "candidate_design_state_id": evidence.1,
                "allow": true, "reason": "Complete coupled history admitted", "diagnostics": {"history_admitted": true}}),
        };
        Ok(AdmissionReply::Record(reply))
    }

    fn optimizer_lifecycle(
        &self,
        _problem: Option<&ProviderProblem>,
    ) -> Result<LifecycleDeclaration, CaeError> {
        Ok(LifecycleDeclaration::Typed(OptimizerLifecycleConfig::new(
            vec![COORDINATE.to_string()],
            "sensitivity_design",
            "evaluate_design",
            Some("candidate_design_admission"),
            None,
            false,
            false,
        )?))
    }
}
