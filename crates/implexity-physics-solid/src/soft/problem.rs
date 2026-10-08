// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Map, Value};

use implexity_core::CaeError;

use super::design::{FieldMap, Interpolation, MassInterpolation, Stiffness};
use super::laws::{Fibres, IsoLaw, Material, Prony, Volumetric};
use super::model::{Formulation, SoftModel};
use super::stepper::{FactorizationReuse, Loading, Measure, NewtonOptions, Scheme};
use crate::hyperelastic::kinematics::TetMesh;
use crate::util::contract;

pub const CASE_KEYS: [&str; 27] = [
    "points",
    "elements",
    "materials",
    "element_material",
    "formulation",
    "stabilization",
    "fibre_directions",
    "fibre_axis",
    "fixed_dofs",
    "prescribed_displacement_m",
    "prescribed_patterns",
    "nodal_force_N",
    "pressure_faces",
    "pressure_Pa",
    "load_steps",
    "times_s",
    "displacement_amplitude",
    "force_amplitude",
    "pressure_amplitude",
    "initial_velocity_m_s",
    "scheme",
    "rayleigh",
    "mass",
    "newton",
    "output_every",
    "tracking",
    "provenance",
];

pub(crate) fn object<'a>(v: &'a Value, what: &str) -> Result<&'a Map<String, Value>, CaeError> {
    v.as_object().ok_or_else(|| CaeError::contract(format!("{what} must be a JSON object")))
}

pub(crate) fn only(m: &Map<String, Value>, allowed: &[&str], what: &str) -> Result<(), CaeError> {
    if let Some(k) = m.keys().find(|k| !allowed.contains(&k.as_str())) {
        return contract(format!("unknown {what} key {k:?}; allowed: {}", allowed.join(", ")));
    }
    Ok(())
}

pub(crate) fn number(v: Option<&Value>, what: &str) -> Result<f64, CaeError> {
    v.and_then(Value::as_f64)
        .filter(|x| x.is_finite())
        .ok_or_else(|| CaeError::contract(format!("{what} must be a finite number")))
}

pub(crate) fn number_or(v: Option<&Value>, default: f64, what: &str) -> Result<f64, CaeError> {
    match v {
        None => Ok(default),
        some => number(some, what),
    }
}

pub(crate) fn numbers(v: Option<&Value>, what: &str) -> Result<Vec<f64>, CaeError> {
    let a = v
        .and_then(Value::as_array)
        .ok_or_else(|| CaeError::contract(format!("{what} must be an array of numbers")))?;
    a.iter().map(|x| number(Some(x), what)).collect()
}

fn triples(v: Option<&Value>, n: usize, what: &str) -> Result<Vec<f64>, CaeError> {
    let a = v
        .and_then(Value::as_array)
        .ok_or_else(|| CaeError::contract(format!("{what} must be a node-by-XYZ array")))?;
    if a.len() != n {
        return contract(format!("{what} must have one XYZ row per node"));
    }
    let mut out = Vec::with_capacity(3 * n);
    for row in a {
        let r = numbers(Some(row), what)?;
        if r.len() != 3 {
            return contract(format!("{what} must have one XYZ row per node"));
        }
        out.extend(r);
    }
    Ok(out)
}

fn index(v: &Value, n: usize, what: &str) -> Result<usize, CaeError> {
    v.as_u64()
        .and_then(|x| usize::try_from(x).ok())
        .filter(|x| *x < n)
        .ok_or_else(|| CaeError::contract(format!("{what} must hold integer indices below {n}")))
}

fn unit(v: &[f64], what: &str) -> Result<[f64; 3], CaeError> {
    let norm = v.iter().map(|x| x * x).sum::<f64>().sqrt();
    if v.len() != 3 || !(norm.is_finite() && norm > 0.0) {
        return contract(format!("{what} must be nonzero XYZ vectors"));
    }
    Ok([v[0] / norm, v[1] / norm, v[2] / norm])
}


pub fn material(v: &Value) -> Result<Material, CaeError> {
    let m = object(v, "material")?;
    let law_name = m.get("law").and_then(Value::as_str).unwrap_or_default();
    let (law, keys): (IsoLaw, &[&str]) = match law_name {
        "neo_hookean" => (IsoLaw::NeoHookean { mu: number(m.get("mu_Pa"), "mu_Pa")? }, &["mu_Pa"]),
        "mooney_rivlin" => (
            IsoLaw::MooneyRivlin {
                c10: number(m.get("c10_Pa"), "c10_Pa")?,
                c01: number(m.get("c01_Pa"), "c01_Pa")?,
            },
            &["c10_Pa", "c01_Pa"],
        ),
        "yeoh" => (IsoLaw::Yeoh { c: numbers(m.get("c_Pa"), "c_Pa")? }, &["c_Pa"]),
        "ogden" => (
            IsoLaw::Ogden { mu: numbers(m.get("mu_Pa"), "mu_Pa")?, alpha: numbers(m.get("alpha"), "alpha")? },
            &["mu_Pa", "alpha"],
        ),
        "gent" => (
            IsoLaw::Gent { mu: number(m.get("mu_Pa"), "mu_Pa")?, jm: number(m.get("jm"), "jm")? },
            &["mu_Pa", "jm"],
        ),
        "arruda_boyce" => (
            IsoLaw::ArrudaBoyce {
                mu: number(m.get("mu_Pa"), "mu_Pa")?,
                lambda_m: number(m.get("lambda_m"), "lambda_m")?,
            },
            &["mu_Pa", "lambda_m"],
        ),
        "neo_hookean_coupled" => (
            IsoLaw::CoupledNeoHookean {
                mu: number(m.get("mu_Pa"), "mu_Pa")?,
                lambda: number(m.get("lambda_Pa"), "lambda_Pa")?,
            },
            &["mu_Pa", "lambda_Pa"],
        ),
        "st_venant_kirchhoff" => (
            IsoLaw::StVenantKirchhoff {
                mu: number(m.get("mu_Pa"), "mu_Pa")?,
                lambda: number(m.get("lambda_Pa"), "lambda_Pa")?,
            },
            &["mu_Pa", "lambda_Pa"],
        ),
        _ => {
            return contract(
                "material law must be one of neo_hookean, mooney_rivlin, yeoh, ogden, gent, arruda_boyce, neo_hookean_coupled, st_venant_kirchhoff",
            );
        }
    };
    let mut allowed: Vec<&str> = vec!["law", "volumetric", "fibres", "prony", "density_kg_m3", "label"];
    allowed.extend_from_slice(keys);
    only(m, &allowed, "material")?;
    let coupled = matches!(law, IsoLaw::CoupledNeoHookean { .. } | IsoLaw::StVenantKirchhoff { .. });
    let (volumetric, bulk) = match m.get("volumetric") {
        None if coupled => (Volumetric::Coupled, 0.0),
        None => return contract("decoupled laws require a volumetric object {function, bulk_Pa}"),
        Some(v) => {
            let o = object(v, "volumetric")?;
            only(o, &["function", "bulk_Pa", "beta"], "volumetric")?;
            let f = match o.get("function").and_then(Value::as_str).unwrap_or_default() {
                "quadratic" => Volumetric::Quadratic,
                "logarithmic" => Volumetric::Logarithmic,
                "simo_taylor" => Volumetric::SimoTaylor,
                "miehe" => Volumetric::Miehe,
                "ogden_beta" => Volumetric::OgdenBeta(number(o.get("beta"), "volumetric beta")?),
                "incompressible" => Volumetric::Incompressible,
                "coupled" => Volumetric::Coupled,
                _ => {
                    return contract(
                        "volumetric function must be one of quadratic, logarithmic, simo_taylor, miehe, ogden_beta, incompressible, coupled",
                    );
                }
            };
            let bulk = if matches!(f, Volumetric::Incompressible | Volumetric::Coupled) {
                0.0
            } else {
                number(o.get("bulk_Pa"), "bulk_Pa")?
            };
            (f, bulk)
        }
    };
    let fibres = match m.get("fibres") {
        None => None,
        Some(v) => {
            let o = object(v, "fibres")?;
            only(o, &["k1_Pa", "k2", "kappa"], "fibres")?;
            Some(Fibres {
                k1: number(o.get("k1_Pa"), "k1_Pa")?,
                k2: number(o.get("k2"), "k2")?,
                kappa: number_or(o.get("kappa"), 0.0, "kappa")?,
            })
        }
    };
    let prony = match m.get("prony") {
        None => None,
        Some(v) => {
            let o = object(v, "prony")?;
            only(o, &["beta", "tau_s"], "prony")?;
            Some(Prony {
                beta: numbers(o.get("beta"), "prony beta")?,
                tau: numbers(o.get("tau_s"), "prony tau_s")?,
            })
        }
    };
    let density = number_or(m.get("density_kg_m3"), 1000.0, "density_kg_m3")?;
    Material::new(law, bulk, volumetric, fibres, prony, density)
}

#[derive(Debug, Clone)]
pub struct Case {
    pub model: SoftModel,
    pub loading: Loading,
    pub scheme: Scheme,
    pub rayleigh: (f64, f64),
    pub newton: NewtonOptions,
    pub factorization_reuse: Option<FactorizationReuse>,
    pub prescribed_patterns: Vec<(Vec<f64>, Vec<f64>)>,
    pub output_every: usize,
    pub tracking: Option<Measure>,
}


#[allow(clippy::too_many_lines)]
pub fn case(v: &Value, history: bool, interpolation: Interpolation) -> Result<Case, CaeError> {
    let m = object(v, "problem")?;
    only(m, &CASE_KEYS, "problem")?;
    let static_only = ["load_steps"];
    let history_only = [
        "times_s",
        "prescribed_patterns",
        "displacement_amplitude",
        "force_amplitude",
        "pressure_amplitude",
        "initial_velocity_m_s",
        "scheme",
        "rayleigh",
        "mass",
        "output_every",
    ];
    if let Some(k) = m.keys().find(|k| {
        if history { static_only.contains(&k.as_str()) } else { history_only.contains(&k.as_str()) }
    }) {
        return contract(format!(
            "key {k:?} belongs to the {} provider",
            if history { "static" } else { "history" }
        ));
    }
    let pts = m
        .get("points")
        .and_then(Value::as_array)
        .ok_or_else(|| CaeError::contract("points must be a node-by-XYZ array"))?;
    let n = pts.len();
    if !(4..=2_000_000).contains(&n) {
        return contract("points must hold 4..2000000 nodes");
    }
    let flat = triples(m.get("points"), n, "points")?;
    let points: Vec<[f64; 3]> = flat.chunks(3).map(|c| [c[0], c[1], c[2]]).collect();
    let els = m
        .get("elements")
        .and_then(Value::as_array)
        .ok_or_else(|| CaeError::contract("elements must be tetrahedral connectivity"))?;
    let mut elements = Vec::with_capacity(els.len());
    for row in els {
        let r = row
            .as_array()
            .filter(|r| r.len() == 4)
            .ok_or_else(|| CaeError::contract("elements must be [element,4] node indices"))?;
        elements.push([
            index(&r[0], n, "elements")?,
            index(&r[1], n, "elements")?,
            index(&r[2], n, "elements")?,
            index(&r[3], n, "elements")?,
        ]);
    }
    let ne = elements.len();
    let mesh = TetMesh::new(points, elements)?;
    let mats = m.get("materials").and_then(Value::as_array).filter(|a| (1..=64).contains(&a.len()));
    let Some(mats) = mats else { return contract("materials must list 1..64 material objects") };
    let materials: Vec<Material> = mats.iter().map(material).collect::<Result<_, _>>()?;
    let element_material = match m.get("element_material") {
        None => vec![0; ne],
        Some(v) => {
            let a = v
                .as_array()
                .filter(|a| a.len() == ne)
                .ok_or_else(|| CaeError::contract("element_material needs one index per element"))?;
            a.iter().map(|x| index(x, materials.len(), "element_material")).collect::<Result<_, _>>()?
        }
    };
    let formulation = match m.get("formulation").and_then(Value::as_str).unwrap_or("displacement") {
        "displacement" => {
            if m.contains_key("stabilization") {
                return contract("stabilization applies to the mixed_up formulation only");
            }
            Formulation::Displacement
        }
        "mixed_up" => {
            Formulation::Mixed { stabilization: number_or(m.get("stabilization"), 1.0, "stabilization")? }
        }
        _ => return contract("formulation must be displacement or mixed_up"),
    };
    let needs_fibres: Vec<bool> = element_material.iter().map(|k| materials[*k].fibres.is_some()).collect();
    let (fibres, fibre_axis) = if needs_fibres.iter().any(|b| *b) {
        let dirs = m
            .get("fibre_directions")
            .ok_or_else(|| CaeError::contract("fibre-reinforced materials require fibre_directions"))?;
        let per_element: Vec<Vec<[f64; 3]>> = match dirs {
            Value::Object(o) => {
                only(o, &["uniform"], "fibre_directions")?;
                let u = o
                    .get("uniform")
                    .and_then(Value::as_array)
                    .ok_or_else(|| CaeError::contract("fibre_directions.uniform must list XYZ vectors"))?;
                let list: Vec<[f64; 3]> = u
                    .iter()
                    .map(|x| unit(&numbers(Some(x), "fibre direction")?, "fibre directions"))
                    .collect::<Result<_, _>>()?;
                vec![list; ne]
            }
            Value::Array(a) if a.len() == ne => a
                .iter()
                .map(|row| {
                    row.as_array()
                        .ok_or_else(|| {
                            CaeError::contract("fibre_directions must be element-by-family-by-XYZ")
                        })?
                        .iter()
                        .map(|x| unit(&numbers(Some(x), "fibre direction")?, "fibre directions"))
                        .collect::<Result<Vec<_>, _>>()
                })
                .collect::<Result<_, _>>()?,
            _ => return contract("fibre_directions must be {uniform: [...]} or element-by-family-by-XYZ"),
        };
        let fibres: Vec<Vec<[f64; 3]>> = per_element
            .into_iter()
            .zip(&needs_fibres)
            .map(|(d, need)| if *need { d } else { Vec::new() })
            .collect();
        let axis: Vec<[f64; 3]> = match m.get("fibre_axis") {
            None => vec![[0.0, 0.0, 1.0]; ne],
            Some(Value::Array(a)) if a.len() == 3 && a.iter().all(Value::is_number) => {
                vec![unit(&numbers(Some(&Value::Array(a.clone())), "fibre_axis")?, "fibre_axis")?; ne]
            }
            Some(Value::Array(a)) if a.len() == ne => a
                .iter()
                .map(|x| unit(&numbers(Some(x), "fibre_axis")?, "fibre_axis"))
                .collect::<Result<_, _>>()?,
            _ => return contract("fibre_axis must be one XYZ vector or one per element"),
        };
        (fibres, axis)
    } else {
        if m.contains_key("fibre_directions") || m.contains_key("fibre_axis") {
            return contract("fibre_directions require a material with fibres");
        }
        (vec![Vec::new(); ne], vec![[0.0, 0.0, 1.0]; ne])
    };
    let fixed_v = m.get("fixed_dofs").and_then(Value::as_array).filter(|a| a.len() == n);
    let Some(fixed_v) = fixed_v else { return contract("fixed_dofs must be a node-by-XYZ Boolean array") };
    let mut fixed = Vec::with_capacity(3 * n);
    for row in fixed_v {
        let r = row.as_array().filter(|r| r.len() == 3 && r.iter().all(Value::is_boolean));
        let Some(r) = r else { return contract("fixed_dofs must be a node-by-XYZ Boolean array") };
        fixed.extend(r.iter().map(|b| b.as_bool().unwrap_or(false)));
    }
    let zeros = vec![0.0; 3 * n];
    let prescribed = if m.contains_key("prescribed_displacement_m") {
        triples(m.get("prescribed_displacement_m"), n, "prescribed_displacement_m")?
    } else {
        zeros.clone()
    };

    let mut prescribed_patterns = Vec::new();
    if let Some(v) = m.get("prescribed_patterns") {
        let rows = v
            .as_array()
            .filter(|a| a.len() <= 16)
            .ok_or_else(|| CaeError::contract("prescribed_patterns must be a list of at most 16 patterns"))?;
        for row in rows {
            let o = object(row, "prescribed_patterns entry")?;
            only(o, &["displacement_m", "amplitude"], "prescribed_patterns entry")?;
            prescribed_patterns.push((
                triples(o.get("displacement_m"), n, "prescribed_patterns.displacement_m")?,
                numbers(o.get("amplitude"), "prescribed_patterns.amplitude")?,
            ));
        }
    }
    let force = if m.contains_key("nodal_force_N") {
        triples(m.get("nodal_force_N"), n, "nodal_force_N")?
    } else {
        zeros.clone()
    };
    let faces: Vec<[usize; 3]> = match m.get("pressure_faces") {
        None => Vec::new(),
        Some(v) => v
            .as_array()
            .ok_or_else(|| CaeError::contract("pressure_faces must be [face,3] node indices"))?
            .iter()
            .map(|row| {
                let r = row
                    .as_array()
                    .filter(|r| r.len() == 3)
                    .ok_or_else(|| CaeError::contract("pressure_faces must be [face,3] node indices"))?;
                Ok([
                    index(&r[0], n, "pressure_faces")?,
                    index(&r[1], n, "pressure_faces")?,
                    index(&r[2], n, "pressure_faces")?,
                ])
            })
            .collect::<Result<_, CaeError>>()?,
    };
    let pressure = number_or(m.get("pressure_Pa"), 0.0, "pressure_Pa")?;
    if pressure != 0.0 && faces.is_empty() {
        return contract("pressure_Pa requires pressure_faces");
    }
    let lumped = match m.get("mass").and_then(Value::as_str).unwrap_or("consistent") {
        "consistent" => false,
        "lumped" => true,
        _ => return contract("mass must be consistent or lumped"),
    };
    let model = SoftModel::new(
        mesh,
        materials,
        element_material,
        formulation,
        fibres,
        fibre_axis,
        interpolation,
        fixed,
        faces,
        lumped,
    )?;
    let factorization_reuse = match m.get("newton").and_then(|v| v.get("factorization_reuse")) {
        None | Some(Value::Bool(false)) => None,
        Some(Value::Bool(true)) => Some(FactorizationReuse::default()),
        Some(v) => {
            let o = object(v, "newton.factorization_reuse")?;
            only(o, &["contraction", "max_reuse"], "newton.factorization_reuse")?;
            let d = FactorizationReuse::default();
            let reuse = FactorizationReuse {
                contraction: number_or(
                    o.get("contraction"),
                    d.contraction,
                    "newton.factorization_reuse.contraction",
                )?,
                max_reuse: o
                    .get("max_reuse")
                    .map_or(Some(d.max_reuse), |x| x.as_u64().and_then(|x| usize::try_from(x).ok()))
                    .filter(|x| (1..=100).contains(x))
                    .ok_or_else(|| {
                        CaeError::contract(
                            "newton.factorization_reuse.max_reuse must be an integer in 1..100",
                        )
                    })?,
            };
            reuse.validate()?;
            Some(reuse)
        }
    };
    let newton = match m.get("newton") {
        None => NewtonOptions::default(),
        Some(v) => {
            let o = object(v, "newton")?;
            only(o, &["max_iterations", "relative_tolerance", "factorization_reuse"], "newton")?;
            let d = NewtonOptions::default();
            NewtonOptions {
                max_iterations: o
                    .get("max_iterations")
                    .map_or(Some(d.max_iterations), |x| x.as_u64().and_then(|x| usize::try_from(x).ok()))
                    .filter(|x| (1..=500).contains(x))
                    .ok_or_else(|| {
                        CaeError::contract("newton.max_iterations must be an integer in 1..500")
                    })?,
                relative_tolerance: number_or(
                    o.get("relative_tolerance"),
                    d.relative_tolerance,
                    "newton.relative_tolerance",
                )?,
            }
        }
    };
    let (loading, scheme, rayleigh, output_every) = if history {
        let times = numbers(m.get("times_s"), "times_s")?;
        let steps = times.len().saturating_sub(1);
        let amp = |key: &str| -> Result<Vec<f64>, CaeError> {
            match m.get(key) {
                None => Ok(vec![1.0; steps]),
                v => numbers(v, key),
            }
        };
        let initial_velocity = if m.contains_key("initial_velocity_m_s") {
            triples(m.get("initial_velocity_m_s"), n, "initial_velocity_m_s")?
        } else {
            zeros
        };
        let scheme = match m.get("scheme") {
            None => Scheme::GeneralizedAlpha { rho_inf: 0.8 },
            Some(v) => {
                let o = object(v, "scheme")?;
                match o.get("kind").and_then(Value::as_str).unwrap_or_default() {
                    "quasistatic" => {
                        only(o, &["kind"], "scheme")?;
                        Scheme::Quasistatic
                    }
                    "newmark" => {
                        only(o, &["kind", "beta", "gamma"], "scheme")?;
                        Scheme::Newmark {
                            beta: number_or(o.get("beta"), 0.25, "scheme.beta")?,
                            gamma: number_or(o.get("gamma"), 0.5, "scheme.gamma")?,
                        }
                    }
                    "generalized_alpha" => {
                        only(o, &["kind", "rho_inf"], "scheme")?;
                        Scheme::GeneralizedAlpha {
                            rho_inf: number_or(o.get("rho_inf"), 0.8, "scheme.rho_inf")?,
                        }
                    }
                    "avf_midpoint" => {
                        only(o, &["kind", "gauss_points"], "scheme")?;
                        let g = number_or(o.get("gauss_points"), 3.0, "scheme.gauss_points")?;
                        if g.fract() != 0.0 || !(1.0..=8.0).contains(&g) {
                            return contract("scheme.gauss_points must be an integer in 1..8");
                        }
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        Scheme::AvfMidpoint { gauss_points: g as usize }
                    }
                    _ => {
                        return contract(
                            "scheme.kind must be quasistatic, newmark, generalized_alpha or avf_midpoint",
                        );
                    }
                }
            }
        };
        let rayleigh = match m.get("rayleigh") {
            None => (0.0, 0.0),
            Some(v) => {
                let o = object(v, "rayleigh")?;
                only(o, &["alpha_mass_s_inv", "beta_stiffness_s"], "rayleigh")?;
                (
                    number_or(o.get("alpha_mass_s_inv"), 0.0, "alpha_mass_s_inv")?,
                    number_or(o.get("beta_stiffness_s"), 0.0, "beta_stiffness_s")?,
                )
            }
        };
        let output_every = match m.get("output_every") {
            None => 1,
            Some(v) => v
                .as_u64()
                .and_then(|x| usize::try_from(x).ok())
                .filter(|x| *x >= 1)
                .ok_or_else(|| CaeError::contract("output_every must be a positive integer"))?,
        };
        (
            Loading {
                times,
                prescribed,
                displacement_amplitude: amp("displacement_amplitude")?,
                force,
                force_amplitude: amp("force_amplitude")?,
                pressure,
                pressure_amplitude: amp("pressure_amplitude")?,
                initial_velocity,
            },
            scheme,
            rayleigh,
            output_every,
        )
    } else {
        let steps = match m.get("load_steps") {
            None => 1,
            Some(v) => v
                .as_u64()
                .and_then(|x| usize::try_from(x).ok())
                .filter(|x| (1..=1000).contains(x))
                .ok_or_else(|| CaeError::contract("load_steps must be an integer in 1..1000"))?,
        };
        #[allow(clippy::cast_precision_loss)]
        let ramp: Vec<f64> = (1..=steps).map(|k| k as f64 / steps as f64).collect();
        #[allow(clippy::cast_precision_loss)]
        let times: Vec<f64> = (0..=steps).map(|k| k as f64).collect();
        (
            Loading {
                times,
                prescribed,
                displacement_amplitude: ramp.clone(),
                force,
                force_amplitude: ramp.clone(),
                pressure,
                pressure_amplitude: ramp,
                initial_velocity: zeros,
            },
            Scheme::Quasistatic,
            (0.0, 0.0),
            1,
        )
    };
    let tracking = match m.get("tracking") {
        None => None,
        Some(v) => {
            let o = object(v, "tracking")?;
            only(o, &["targets", "amplitude"], "tracking")?;
            let rows =
                o.get("targets").and_then(Value::as_array).filter(|a| !a.is_empty()).ok_or_else(|| {
                    CaeError::contract("tracking.targets must list [node, component, target_m, weight] rows")
                })?;
            let mut targets = Vec::with_capacity(rows.len());
            for row in rows {
                let r = row.as_array().filter(|r| r.len() == 4).ok_or_else(|| {
                    CaeError::contract("tracking.targets must list [node, component, target_m, weight] rows")
                })?;
                let node = index(&r[0], n, "tracking node")?;
                let comp = index(&r[1], 3, "tracking component")?;
                let weight = number(Some(&r[3]), "tracking weight")?;
                if weight < 0.0 {
                    return contract("tracking weights must be nonnegative");
                }
                targets.push((3 * node + comp, number(Some(&r[2]), "tracking target_m")?, weight));
            }
            let steps = loading.steps();
            let amplitude = match o.get("amplitude") {
                None => vec![1.0; steps],
                v => numbers(v, "tracking.amplitude")?,
            };
            if amplitude.len() != steps {
                return contract("tracking.amplitude needs one value per step");
            }
            Some(Measure::Tracking { targets, amplitude })
        }
    };
    if prescribed_patterns.iter().any(|(_, a)| a.len() != loading.steps()) {
        return contract("prescribed_patterns.amplitude needs one value per step");
    }
    Ok(Case {
        model,
        loading,
        scheme,
        rayleigh,
        newton,
        factorization_reuse,
        prescribed_patterns,
        output_every,
        tracking,
    })
}

#[derive(Debug, Clone)]
pub struct DesignSettings {
    pub history: bool,
    pub case: Case,
    pub density: FieldMap,
    pub angle: Option<FieldMap>,
    pub weights: Vec<f64>,
    pub checkpoint: Option<usize>,
    pub periodic: Option<crate::soft_fsi::periodic::PeriodicSolid>,
}

pub const DESIGN_KEYS: [&str; 11] = [
    "analysis",
    "case",
    "interpolation",
    "filter_radius_m",
    "projection",
    "design_region",
    "fixed_density",
    "fibre_angle",
    "response_weights",
    "checkpointing",
    "periodic",
];


#[allow(clippy::too_many_lines)]
pub fn design(v: &Value) -> Result<DesignSettings, CaeError> {
    let m = object(v, "design problem")?;
    only(m, &DESIGN_KEYS, "design problem")?;
    let history = match m.get("analysis").and_then(Value::as_str).unwrap_or("static") {
        "static" => false,
        "history" => true,
        _ => return contract("analysis must be static or history"),
    };
    let mut interpolation = Interpolation::default();
    if let Some(iv) = m.get("interpolation") {
        let o = object(iv, "interpolation")?;
        only(
            o,
            &["stiffness", "penalty", "q", "e_min", "wang", "wang_beta", "wang_eta", "mass"],
            "interpolation",
        )?;
        let e_min = number_or(o.get("e_min"), 1e-6, "e_min")?;
        interpolation.stiffness = match o.get("stiffness").and_then(Value::as_str).unwrap_or("simp") {
            "simp" => Stiffness::Simp { penalty: number_or(o.get("penalty"), 3.0, "penalty")?, e_min },
            "ramp" => Stiffness::Ramp { q: number_or(o.get("q"), 8.0, "q")?, e_min },
            _ => return contract("interpolation.stiffness must be simp or ramp"),
        };
        let wang = match o.get("wang") {
            None => true,
            Some(b) => {
                b.as_bool().ok_or_else(|| CaeError::contract("interpolation.wang must be a Boolean"))?
            }
        };
        interpolation.wang = if wang {
            Some((
                number_or(o.get("wang_beta"), 500.0, "wang_beta")?,
                number_or(o.get("wang_eta"), 0.01, "wang_eta")?,
            ))
        } else {
            None
        };
        interpolation.mass = match o.get("mass").and_then(Value::as_str).unwrap_or("pedersen") {
            "pedersen" => MassInterpolation::Pedersen,
            "linear" => MassInterpolation::Linear,
            "constant" => MassInterpolation::Constant,
            _ => return contract("interpolation.mass must be pedersen, linear or constant"),
        };
    }
    let case = case(
        m.get("case").ok_or_else(|| CaeError::contract("design problems require a case"))?,
        history,
        interpolation,
    )?;
    let model = &case.model;
    let ne = model.ne();
    let centroids: Vec<[f64; 3]> = model
        .mesh
        .elements
        .iter()
        .map(|t| core::array::from_fn(|a| t.iter().map(|n| model.mesh.points[*n][a]).sum::<f64>() / 4.0))
        .collect();
    let bools = |key: &str| -> Result<Vec<bool>, CaeError> {
        match m.get(key) {
            None => Ok(vec![true; ne]),
            Some(v) => v
                .as_array()
                .filter(|a| a.len() == ne && a.iter().all(Value::is_boolean))
                .map(|a| a.iter().map(|b| b.as_bool().unwrap_or(false)).collect())
                .ok_or_else(|| CaeError::contract(format!("{key} must hold one Boolean per element"))),
        }
    };
    let region = bools("design_region")?;
    let fixed = match m.get("fixed_density") {
        None => vec![1.0; ne],
        v => numbers(v, "fixed_density")?,
    };
    if fixed.len() != ne || fixed.iter().any(|x| !(0.0..=1.0).contains(x)) {
        return contract("fixed_density must hold one value in [0, 1] per element");
    }
    let radius = number_or(m.get("filter_radius_m"), 0.0, "filter_radius_m")?;
    let projection = match m.get("projection") {
        None => None,
        Some(pv) => {
            let o = object(pv, "projection")?;
            only(o, &["beta", "eta"], "projection")?;
            Some((number(o.get("beta"), "projection.beta")?, number_or(o.get("eta"), 0.5, "projection.eta")?))
        }
    };
    let density = FieldMap::new(&centroids, &model.mesh.volumes, radius, region, fixed, projection)?;
    let angle = match m.get("fibre_angle") {
        None => None,
        Some(av) => {
            if model.fibres.iter().all(Vec::is_empty) {
                return contract("fibre_angle design requires fibre-reinforced materials");
            }
            let o = object(av, "fibre_angle")?;
            only(o, &["design_region", "fixed_rad", "filter_radius_m"], "fibre_angle")?;
            let region: Vec<bool> = match o.get("design_region") {
                None => model.fibres.iter().map(|f| !f.is_empty()).collect(),
                Some(v) => v
                    .as_array()
                    .filter(|a| a.len() == ne && a.iter().all(Value::is_boolean))
                    .map(|a| a.iter().map(|b| b.as_bool().unwrap_or(false)).collect())
                    .ok_or_else(|| {
                        CaeError::contract("fibre_angle.design_region must hold one Boolean per element")
                    })?,
            };
            let fixed = match o.get("fixed_rad") {
                None => vec![0.0; ne],
                v => numbers(v, "fibre_angle.fixed_rad")?,
            };
            let r = number_or(o.get("filter_radius_m"), 0.0, "fibre_angle.filter_radius_m")?;
            Some(FieldMap::new(&centroids, &model.mesh.volumes, r, region, fixed, None)?)
        }
    };
    let steps = case.loading.steps();
    let weights = match m.get("response_weights") {
        None => case.loading.times.windows(2).map(|w| w[1] - w[0]).collect(),
        v => numbers(v, "response_weights")?,
    };
    if weights.len() != steps
        || weights.iter().any(|w| *w < 0.0)
        || weights.iter().sum::<f64>().partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater)
    {
        return contract("response_weights need one nonnegative value per step and a positive sum");
    }
    let checkpoint = match m.get("checkpointing") {
        None => None,
        Some(Value::String(s)) if s == "all" => None,
        Some(Value::String(s)) if s == "sqrt" => {
            let mut k = 1;
            while k * k < steps {
                k += 1;
            }
            Some(k)
        }
        Some(v) => {
            Some(v.as_u64().and_then(|x| usize::try_from(x).ok()).filter(|x| *x >= 1).ok_or_else(|| {
                CaeError::contract("checkpointing must be all, sqrt or a positive interval")
            })?)
        }
    };
    let periodic = match m.get("periodic") {
        None => None,
        Some(pv) => {
            if !history {
                return contract("periodic responses require the history analysis");
            }
            Some(crate::soft_fsi::periodic::PeriodicSolid::parse(pv, &case)?)
        }
    };
    Ok(DesignSettings { history, case, density, angle, weights, checkpoint, periodic })
}
