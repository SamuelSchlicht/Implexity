// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::any::Any;
use std::collections::BTreeMap;
use std::f64::consts::PI;
use std::sync::{Arc, OnceLock};

use serde_json::{Map, Value, json};

use implexity_ad::Scalar;
use implexity_core::contracts::FieldValue;
use implexity_core::history_field_sources::HistorySourceComponent;
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_physics_solid::solid_elements::SolidModel;
use implexity_physics_solid::solid_history::SolidKernel;
use implexity_solve::coupled_history::{
    CoupledHistoryAssembly, HistoryBlock, HistoryBlockCallbacks, HistoryInterface,
};
use implexity_solve::local_assembly::{AssemblyOptions, Kind, LocalResidual, LocalResidualAssembly};
use implexity_solve::matrix::Jacobian;

use crate::common::{design_incidence, err, extend_coupling, incidence, py_int, zero_csr};
use crate::host::{
    BoundFieldSource, BoundSource, FieldSourceAuthoring, ResponseVjp, SourceEnergy, block_start, host_of,
};
use crate::prescribed_joule::{array, strings};
use crate::prescribed_volumetric_heating::py_shape;

pub const NAME: &str = "neutral_transport_deposition";
pub const BLOCK: &str = "neutral_transport";
const FOUR_PI: f64 = 4.0 * PI;
const DATA: &str = include_str!("data/neutral_deposition.json");
const ENERGY_CONVENTION: &str = "authored_heat_excludes_transport_deposition_which_includes_defect_storage";

fn data() -> Value {
    serde_json::from_str(DATA).unwrap_or(Value::Null)
}

#[must_use]
pub fn limitations() -> Vec<String> {
    strings(&data()["transport_limitations"])
}

#[must_use]
pub fn units() -> Value {
    json!({"energy_edges_J": "J", "group_energy_J": "J", "absorption_m_inv": "1/m",
        "scattering_from_to_m_inv": "1/m", "reaction_release_J_m": "J/m",
        "local_deposition_J_m": "J/m", "damage_response_m2": "dpa*m^2",
        "incoming_angular_flux": "1/(m^2*s*sr)", "isotropic_volume_source": "1/(m^3*s)"})
}

#[must_use]
pub fn leggauss(n: usize) -> (Vec<f64>, Vec<f64>) {
    let mut x = vec![0.0; n];
    let mut w = vec![0.0; n];
    for i in 0..n {

        let mut z = (PI * (i as f64 + 0.75) / (n as f64 + 0.5)).cos();
        let mut dp = 0.0;
        for _ in 0..100 {
            let (mut p0, mut p1) = (1.0, z);
            for k in 2..=n {
                let p2 = ((2 * k - 1) as f64 * z * p1 - (k - 1) as f64 * p0) / k as f64;
                p0 = p1;
                p1 = p2;
            }
            let pn = if n == 0 { 1.0 } else { p1 };
            let pm = if n == 1 { 1.0 } else { p0 };
            dp = n as f64 * (z * pn - pm) / (z * z - 1.0);
            let dz = pn / dp;
            z -= dz;
            if dz.abs() < 1e-16 {
                break;
            }
        }
        x[n - 1 - i] = z;
        w[n - 1 - i] = 2.0 / ((1.0 - z * z) * dp * dp);
    }

    let xs: Vec<f64> = (0..n).map(|i| (x[i] - x[n - 1 - i]) / 2.0).collect();
    let ws: Vec<f64> = (0..n).map(|i| f64::midpoint(w[i], w[n - 1 - i])).collect();
    let total: f64 = ws.iter().sum();
    (xs, ws.iter().map(|v| v * 2.0 / total).collect())
}


pub fn quadrature(n_mu: &Value, n_phi: &Value) -> CaeResult<(Vec<[f64; 3]>, Vec<f64>)> {
    let (Some(m), Some(p)) = (py_int(n_mu), py_int(n_phi)) else {
        return Err(err("quadrature requires even n_mu>=2, n_phi a multiple of 4, <=128 directions"));
    };
    if m < 2 || m % 2 != 0 || p < 4 || p % 4 != 0 || m * p > 128 {
        return Err(err("quadrature requires even n_mu>=2, n_phi a multiple of 4, <=128 directions"));
    }
    let (m, p) = (usize::try_from(m).unwrap_or(0), usize::try_from(p).unwrap_or(0));
    let (mu, w) = leggauss(m);
    let mut directions = Vec::with_capacity(m * p);
    let mut weights = Vec::with_capacity(m * p);
    for (v, wv) in mu.iter().zip(&w) {
        for k in 0..p {
            let a = (k as f64 + 0.5) * (2.0 * PI / p as f64);
            let s = (1.0 - v * v).sqrt();
            directions.push([s * a.cos(), s * a.sin(), *v]);
            weights.push(wv * 2.0 * PI / p as f64);
        }
    }
    Ok((directions, weights))
}


pub fn checked_array(value: &Value, shape: &[usize], name: &str, nonnegative: bool) -> CaeResult<Vec<f64>> {
    let Some((s, v)) = implexity_physics_solid::util::real_array(value) else {
        return Err(err(format!("{name}: real numeric array required")));
    };
    if s != shape || !v.iter().all(|x| x.is_finite()) || (nonnegative && v.iter().any(|x| *x < 0.0)) {
        let neg = if nonnegative { "nonnegative " } else { "" };
        return Err(err(format!("{name}: finite {neg}shape {} required", py_shape(shape))));
    }
    Ok(v)
}

#[derive(Clone, Debug)]
pub struct Card {
    pub interval: [f64; 2],
    pub absorption: Vec<f64>,
    pub scattering: Vec<Vec<f64>>,
    pub release: Vec<f64>,
    pub deposition: Vec<f64>,
    pub damage: Vec<f64>,
    pub provenance: String,
}


#[allow(clippy::too_many_lines)]
pub fn normalise(settings: &Value, context: &Value) -> CaeResult<Value> {
    let required = [
        "name",
        "provenance",
        "energy_edges_J",
        "group_energy_J",
        "quadrature",
        "materials",
        "units",
        "incoming_angular_flux",
        "isotropic_volume_source",
        "numerics",
        "source_convention",
        "data_convention",
    ];
    let ok = settings
        .as_object()
        .is_some_and(|m| m.len() == required.len() && required.iter().all(|k| m.contains_key(*k)));
    let Some(s) = settings.as_object().filter(|_| ok) else {
        let mut sorted = required.to_vec();
        sorted.sort_unstable();
        return Err(err(format!("neutral transport requires {}", implexity_core::pyobj::list_repr(&sorted))));
    };
    let mut p = s.clone();
    if ["name", "provenance"].iter().any(|k| !p[*k].as_str().is_some_and(|v| !v.trim().is_empty())) {
        return Err(err("transport identity and data provenance required"));
    }
    if p["units"] != units() {
        return Err(err("explicit SI group/source/dose units required"));
    }
    if p["source_convention"] != json!("absolute_rates_right_endpoint_zero_initial") {
        return Err(err("absolute source rates and zero-initial right-endpoint history required"));
    }
    if p["data_convention"] != json!("nonmultiplying_isotropic_fixed_group_data_from_to") {
        return Err(err("explicit nonmultiplying fixed-group data convention required"));
    }
    let Some(ng) = p["group_energy_J"].as_array().map(Vec::len).filter(|n| (1..=16).contains(n)) else {
        return Err(err("1..16 declared groups required"));
    };
    let energy = checked_array(&p["group_energy_J"], &[ng], "group energies", true)?;
    let edges = checked_array(&p["energy_edges_J"], &[ng + 1], "group edges", true)?;
    if edges.windows(2).any(|w| w[1] - w[0] >= 0.0)
        || (0..ng).any(|g| energy[g] <= edges[g + 1] || energy[g] >= edges[g])
    {
        return Err(err("descending energy edges with strictly internal representative energies required"));
    }
    let q = &p["quadrature"];
    if !q.as_object().is_some_and(|m| m.len() == 2 && m.contains_key("n_mu") && m.contains_key("n_phi")) {
        return Err(err("explicit product quadrature required"));
    }
    let (directions, _) = quadrature(&q["n_mu"], &q["n_phi"])?;
    let nd = directions.len();
    let Some(mats) = p["materials"].as_array().filter(|m| m.len() == 3).cloned() else {
        return Err(err("two ordered solid endpoints and one fluid data card required"));
    };
    let solid = &context["solid"];
    let mut expected: Vec<String> = solid["materials"]
        .as_array()
        .map(|a| a.iter().map(implexity_core::wire::fingerprint_value).collect())
        .unwrap_or_default();
    expected.push(implexity_core::wire::fingerprint_value(&context["fluid"]["material"]));
    let keys = [
        "name",
        "host_material_fingerprint",
        "provenance",
        "temperature_interval_K",
        "absorption_m_inv",
        "scattering_from_to_m_inv",
        "reaction_release_J_m",
        "local_deposition_J_m",
        "damage_response_m2",
    ];
    for (i, m) in mats.iter().enumerate() {
        let ok =
            m.as_object().is_some_and(|o| o.len() == keys.len() && keys.iter().all(|k| o.contains_key(*k)))
                && ["name", "host_material_fingerprint", "provenance"]
                    .iter()
                    .all(|k| m[*k].as_str().is_some_and(|v| !v.trim().is_empty()));
        if !ok {
            return Err(err("complete nuclear data cards with provenance and host identity required"));
        }
        if expected.get(i).is_none_or(|e| m["host_material_fingerprint"].as_str() != Some(e.as_str())) {
            return Err(err("transport material order/identity disagrees with native solid/fluid endpoints"));
        }
        let ti =
            checked_array(&m["temperature_interval_K"], &[2], "nuclear-data temperature interval", true)?;
        let t0 = solid["temperature_initial_K"].as_f64().unwrap_or(f64::NAN);
        if !(0.0 < ti[0] && ti[0] < ti[1]) || !(ti[0] <= t0 && t0 <= ti[1]) {
            return Err(err("fixed group-data validity interval must contain the initial temperature"));
        }
        let a = checked_array(&m["absorption_m_inv"], &[ng], "absorption", true)?;
        let sc = checked_array(&m["scattering_from_to_m_inv"], &[ng, ng], "scattering", true)?;
        let q = checked_array(&m["reaction_release_J_m"], &[ng], "reaction release", true)?;
        let k = checked_array(&m["local_deposition_J_m"], &[ng], "deposition", true)?;
        let damage = checked_array(&m["damage_response_m2"], &[ng], "damage response", true)?;
        for g in 0..ng {
            let loss = a[g] * energy[g]
                + (0..ng).map(|h| sc[g * ng + h] * (energy[g] - energy[h])).sum::<f64>()
                + q[g];
            if !loss.is_finite() {
                return Err(err("nonfinite collision energy removal from authored group data"));
            }
        }
        for g in 0..ng {
            let loss = a[g] * energy[g]
                + (0..ng).map(|h| sc[g * ng + h] * (energy[g] - energy[h])).sum::<f64>()
                + q[g];
            let tol = 1e-12 * loss.abs().max(1e-300);
            if loss < 0.0 || k[g] > loss + tol {
                return Err(err(
                    "local deposition exceeds declared collision energy removal plus reaction release",
                ));
            }
        }
        for g in 0..ng {
            let collisions = a[g] + (0..ng).map(|h| sc[g * ng + h]).sum::<f64>();
            if collisions == 0.0 && (q[g] != 0.0 || k[g] != 0.0 || damage[g] != 0.0) {
                return Err(err("reaction/deposition/damage response without collisions is forbidden"));
            }
        }
    }
    let nt = solid["times_s"].as_array().map_or(0, Vec::len);
    let nc: usize = solid["grid"]
        .as_array()
        .map_or(0, |g| g.iter().filter_map(Value::as_u64).map(|v| usize::try_from(v).unwrap_or(0)).product());
    let bc = checked_array(&p["incoming_angular_flux"], &[nt, 6, ng, nd], "incoming angular flux", true)?;
    let src = checked_array(&p["isotropic_volume_source"], &[nt, nc, ng], "volume source", true)?;
    if bc[..6 * ng * nd].iter().any(|v| *v != 0.0) || src[..nc * ng].iter().any(|v| *v != 0.0) {
        return Err(err("zero initial source required for this design-independent initialized history"));
    }
    for n in 0..nt {
        for ax in 0..3 {
            for side in 0..2 {
                for g in 0..ng {
                    for d in 0..nd {
                        let outgoing =
                            if side == 0 { directions[d][ax] < 0.0 } else { directions[d][ax] > 0.0 };
                        if outgoing && bc[((n * 6 + 2 * ax + side) * ng + g) * nd + d] != 0.0 {
                            return Err(err("boundary data may prescribe incoming ordinates only"));
                        }
                    }
                }
            }
        }
    }
    let nkeys = [
        "angular_flux_scale",
        "length_scale_m",
        "batch_size",
        "max_estimated_bytes",
        "balance_relative_tolerance",
    ];
    let numerics = p["numerics"].clone();
    let Some(n) =
        numerics.as_object().filter(|m| m.len() == nkeys.len() && nkeys.iter().all(|k| m.contains_key(*k)))
    else {
        return Err(err("positive explicit transport scales/resource/balance bounds required"));
    };
    for (key, value) in n {
        let v = checked_array(&json!([value]), &[1], &format!("transport numerics {key}"), true)?;
        if v[0] <= 0.0 {
            return Err(err("positive explicit transport scales/resource/balance bounds required"));
        }
    }
    let is_int = |v: &Value| matches!(v, Value::Number(x) if x.is_i64() || x.is_u64());
    if !is_int(&n["batch_size"])
        || !is_int(&n["max_estimated_bytes"])
        || n["batch_size"].as_f64().unwrap_or(0.0) > 256.0
        || n["balance_relative_tolerance"].as_f64().unwrap_or(1.0) > 1e-5
    {
        return Err(err(
            "integer bounded transport assembly resources and balance tolerance <=1e-5 required",
        ));
    }
    let mut nn = n.clone();
    for key in ["angular_flux_scale", "length_scale_m", "balance_relative_tolerance"] {
        nn.insert(key.into(), json!(n[key].as_f64().unwrap_or(0.0)));
    }
    let scale = (nn["angular_flux_scale"].as_f64().unwrap_or(0.0)
        * nn["length_scale_m"].as_f64().unwrap_or(0.0))
        * nn["length_scale_m"].as_f64().unwrap_or(0.0);
    if !scale.is_finite() || scale < f64::MIN_POSITIVE {
        return Err(err(
            "combined transport residual scale must be finite and at least the smallest normal float",
        ));
    }
    let estimate = (nc * (ng * nd).pow(2) * 4 * 24 + nt * nc * ng * nd * 8 * 8) as f64;
    if estimate > n["max_estimated_bytes"].as_f64().unwrap_or(0.0) {
        return Err(err("declared neutral transport sparse workspace budget exceeded"));
    }
    p.insert("numerics".into(), Value::Object(nn));
    Ok(Value::Object(p))
}

pub struct NeutralKernel {
    pub p: Value,
    pub grid: [usize; 3],
    pub nc: usize,
    pub nt: usize,
    pub energy: Vec<f64>,
    pub ng: usize,
    pub directions: Vec<[f64; 3]>,
    pub weights: Vec<f64>,
    pub nd: usize,
    pub nloc: usize,
    pub state_size: usize,
    pub design_size: usize,
    pub scale: f64,
    pub length_scale: f64,
    pub rescale: f64,
    pub cards: Vec<Card>,
    pub total: Vec<Vec<f64>>,
    pub bc: Vec<f64>,
    pub source: Vec<f64>,
    boundary_faces: Vec<i64>,
    local: Arc<LocalResidualAssembly<TransportCell>>,
}

pub type CellQuantities = (Vec<Vec<f64>>, Vec<f64>, Vec<[f64; 2]>, Vec<f64>);

pub struct TransportCell {
    ng: usize,
    nd: usize,
    scale: f64,
    rescale: f64,
    weights: Vec<f64>,
    om: Vec<[f64; 3]>,
    total: Vec<Vec<f64>>,
    scatter: Vec<Vec<Vec<f64>>>,
}

impl LocalResidual for TransportCell {
    fn residual<S: Scalar>(&self, _item: usize, current: &[S], _previous: &[S], design: &[S], out: &mut [S]) {
        let (ng, nd) = (self.ng, self.nd);
        let nloc = ng * nd;
        let sc = S::from_f64(self.scale);
        let psi = |g: usize, d: usize| current[g * nd + d] * sc;
        let up = |a: usize, g: usize, d: usize| current[nloc + (a * ng + g) * nd + d] * sc;
        let h: [S; 3] = std::array::from_fn(|a| design[1 + a] * S::from_f64(1e-3));
        let v = h[0] * h[1] * h[2];
        let area: [S; 3] = std::array::from_fn(|a| v / h[a]);
        let fr = [design[0] * (S::one() - design[4]), design[0] * design[4], S::one() - design[0]];
        let total: Vec<S> = (0..ng)
            .map(|g| (0..3).fold(S::zero(), |acc, m| acc + fr[m] * S::from_f64(self.total[m][g])))
            .collect();
        let scalar: Vec<S> = (0..ng)
            .map(|g| (0..nd).fold(S::zero(), |acc, d| acc + psi(g, d) * S::from_f64(self.weights[d])))
            .collect();
        for g in 0..ng {
            let inscatter = (0..ng).fold(S::zero(), |acc, f| {
                let s = (0..3).fold(S::zero(), |a2, m| a2 + fr[m] * S::from_f64(self.scatter[m][f][g]));
                acc + s * scalar[f]
            });
            let src = current[4 * nloc + g];
            for d in 0..nd {
                let coll = total[g] * psi(g, d) - inscatter / S::from_f64(FOUR_PI);
                let streaming = (0..3).fold(S::zero(), |acc, a| {
                    acc + area[a] * S::from_f64(self.om[d][a]) * (psi(g, d) - up(a, g, d))
                });
                out[g * nd + d] =
                    (streaming + v * (coll - src / S::from_f64(FOUR_PI))) / S::from_f64(self.rescale);
            }
        }
    }
}

fn f64_rows(v: &Value) -> Vec<f64> {
    implexity_physics_solid::util::real_array(v).map(|(_, d)| d).unwrap_or_default()
}

impl NeutralKernel {

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]
    pub fn new(settings: &Value, context: &Value) -> CaeResult<Self> {
        let p = normalise(settings, context)?;
        let solid = &context["solid"];
        let g: Vec<usize> = solid["grid"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_u64).map(|v| v as usize).collect())
            .unwrap_or_default();
        let grid = [g[0], g[1], g[2]];
        let nc = grid.iter().product::<usize>();
        let nt = solid["times_s"].as_array().map_or(0, Vec::len);
        let energy = f64_rows(&p["group_energy_J"]);
        let ng = energy.len();
        let (directions, weights) = quadrature(&p["quadrature"]["n_mu"], &p["quadrature"]["n_phi"])?;
        let nd = weights.len();
        let nloc = ng * nd;
        let scale = p["numerics"]["angular_flux_scale"].as_f64().unwrap_or(1.0);
        let length_scale = p["numerics"]["length_scale_m"].as_f64().unwrap_or(1.0);
        let rescale = (scale * length_scale) * length_scale;
        let cards: Vec<Card> = p["materials"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|m| {
                        let sc = f64_rows(&m["scattering_from_to_m_inv"]);
                        let ti = f64_rows(&m["temperature_interval_K"]);
                        Card {
                            interval: [ti[0], ti[1]],
                            absorption: f64_rows(&m["absorption_m_inv"]),
                            scattering: sc.chunks(ng).map(<[f64]>::to_vec).collect(),
                            release: f64_rows(&m["reaction_release_J_m"]),
                            deposition: f64_rows(&m["local_deposition_J_m"]),
                            damage: f64_rows(&m["damage_response_m2"]),
                            provenance: m["provenance"].as_str().unwrap_or_default().to_string(),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let total: Vec<Vec<f64>> = cards
            .iter()
            .map(|c| (0..ng).map(|g| c.absorption[g] + c.scattering[g].iter().sum::<f64>()).collect())
            .collect();
        let bc = f64_rows(&p["incoming_angular_flux"]);
        let source = f64_rows(&p["isotropic_volume_source"]);
        let id = |c: usize, g: usize, d: usize| ((c * ng + g) * nd + d) as i64;
        let mut rows = Vec::with_capacity(nc * nloc);
        let mut current = Vec::with_capacity(nc * (4 * nloc + ng));
        let mut xmap = Vec::with_capacity(nc * 5);
        let mut boundary_faces = Vec::with_capacity(nc * 3 * nd);
        let nci = nc as i64;
        for ci in 0..nc {
            let cell = [ci / (grid[1] * grid[2]), (ci / grid[2]) % grid[1], ci % grid[2]];
            let mut up = vec![-1_i64; 3 * nloc];
            let mut bf = vec![-1_i64; 3 * nd];
            for ax in 0..3 {
                for d in 0..nd {
                    let shift: i64 = if directions[d][ax] > 0.0 { -1 } else { 1 };
                    let other = cell[ax] as i64 + shift;
                    if other >= 0 && other < grid[ax] as i64 {
                        let mut o = cell;
                        o[ax] = other as usize;
                        let oc = (o[0] * grid[1] + o[1]) * grid[2] + o[2];
                        for g in 0..ng {
                            up[(ax * ng + g) * nd + d] = id(oc, g, d);
                        }
                    } else {
                        bf[ax * nd + d] = 2 * ax as i64 + i64::from(shift > 0);
                    }
                }
            }
            let own: Vec<i64> = (0..nloc).map(|k| ci as i64 * nloc as i64 + k as i64).collect();
            rows.extend(&own);
            current.extend(&own);
            current.extend(&up);
            current.extend(std::iter::repeat_n(-1, ng));
            boundary_faces.extend(bf);
            xmap.extend([ci as i64, nci, nci + 1, nci + 2, nci + 3 + ci as i64]);
        }
        let kernel = TransportCell {
            ng,
            nd,
            scale,
            rescale,
            weights: weights.clone(),
            om: directions.iter().map(|d| d.map(f64::abs)).collect(),
            total: total.clone(),
            scatter: cards.iter().map(|c| c.scattering.clone()).collect(),
        };
        let batch = usize::try_from(p["numerics"]["batch_size"].as_u64().unwrap_or(64)).unwrap_or(64);
        let state_size = nc * nloc;
        let design_size = 2 * nc + 3;
        let local = LocalResidualAssembly::new(
            kernel,
            incidence(nc, nloc, rows)?,
            incidence(nc, 4 * nloc + ng, current.clone())?,
            incidence(nc, 4 * nloc + ng, current)?,
            incidence(nc, 5, xmap)?,
            state_size,
            design_size,
            AssemblyOptions { batch_size: batch, ..AssemblyOptions::default() },
        )?;
        Ok(Self {
            p,
            grid,
            nc,
            nt,
            energy,
            ng,
            directions,
            weights,
            nd,
            nloc,
            state_size,
            design_size,
            scale,
            length_scale,
            rescale,
            cards,
            total,
            bc,
            source,
            boundary_faces,
            local: Arc::new(local),
        })
    }

    fn data(&self, n: usize) -> Vec<f64> {
        let (ng, nd, nloc) = (self.ng, self.nd, self.nloc);
        let width = 4 * nloc + ng;
        let mut out = vec![0.0; self.nc * width];
        for ci in 0..self.nc {
            for ax in 0..3 {
                for d in 0..nd {
                    let face = self.boundary_faces[(ci * 3 + ax) * nd + d];
                    if let Ok(f) = usize::try_from(face) {
                        for g in 0..ng {
                            out[ci * width + nloc + (ax * ng + g) * nd + d] =
                                self.bc[((n * 6 + f) * ng + g) * nd + d] / self.scale;
                        }
                    }
                }
            }
            for g in 0..ng {
                out[ci * width + 4 * nloc + g] = self.source[(n * self.nc + ci) * ng + g];
            }
        }
        out
    }


    pub fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        let d = self.data(n);
        self.local.residual(z, old, x, &d, &d)
    }


    pub fn jacobian(&self, kind: Kind, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        if kind == Kind::Previous {
            return Ok(Jacobian::Csr(zero_csr(self.state_size, self.state_size)));
        }
        let d = self.data(n);
        Ok(Jacobian::Csr(self.local.jacobian(kind, z, old, x, &d, &d)?))
    }

    #[must_use]
    pub fn scalar_flux(&self, z: &[f64]) -> Vec<Vec<f64>> {
        (0..self.nc)
            .map(|c| {
                (0..self.ng)
                    .map(|g| {
                        (0..self.nd)
                            .map(|d| z[(c * self.ng + g) * self.nd + d] * self.scale * self.weights[d])
                            .sum()
                    })
                    .collect()
            })
            .collect()
    }

    fn fractions(&self, x: &[f64], c: usize) -> [f64; 3] {
        let (rho, ph) = (x[c], x[self.nc + 3 + c]);
        [rho * (1.0 - ph), rho * ph, 1.0 - rho]
    }

    #[must_use]
    pub fn cell_quantities(&self, z: &[f64], x: &[f64]) -> CellQuantities {
        let phi = self.scalar_flux(z);
        let mut deposition = Vec::with_capacity(self.nc);
        let mut rates = Vec::with_capacity(self.nc);
        let mut release = Vec::with_capacity(self.nc);
        for c in 0..self.nc {
            let fr = self.fractions(x, c);
            let mix = |f: &dyn Fn(&Card) -> &Vec<f64>, g: usize| {
                (0..3).map(|m| fr[m] * f(&self.cards[m])[g]).sum::<f64>()
            };
            deposition.push((0..self.ng).map(|g| mix(&|k| &k.deposition, g) * phi[c][g]).sum());
            release.push((0..self.ng).map(|g| mix(&|k| &k.release, g) * phi[c][g]).sum());
            rates.push(std::array::from_fn(|k| {
                (0..self.ng).map(|g| phi[c][g] * self.cards[k].damage[g]).sum()
            }));
        }
        (phi, deposition, rates, release)
    }

    fn cell_ids(&self, ax: usize, side: usize) -> Vec<usize> {
        let target = if side == 0 { 0 } else { self.grid[ax] - 1 };
        (0..self.nc)
            .filter(|c| {
                let ijk =
                    [c / (self.grid[1] * self.grid[2]), (c / self.grid[2]) % self.grid[1], c % self.grid[2]];
                ijk[ax] == target
            })
            .collect()
    }

    #[must_use]
    pub fn ledger(&self, n: usize, z: &[f64], x: &[f64]) -> Vec<(&'static str, f64)> {
        let nc = self.nc;
        let h: [f64; 3] = std::array::from_fn(|a| x[nc + a] * 1e-3);
        let v = h[0] * h[1] * h[2];
        let area: [f64; 3] = std::array::from_fn(|a| v / h[a]);
        let (phi, deposition, _, release) = self.cell_quantities(z, x);
        let mut absorb = 0.0;
        for c in 0..nc {
            let fr = self.fractions(x, c);
            for g in 0..self.ng {
                absorb += v * (0..3).map(|m| fr[m] * self.cards[m].absorption[g]).sum::<f64>() * phi[c][g];
            }
        }
        let mut inward = vec![0.0; self.ng];
        let mut outward = vec![0.0; self.ng];
        for ax in 0..3 {
            for side in 0..2 {
                let ix = self.cell_ids(ax, side);
                for g in 0..self.ng {
                    let mut inc = 0.0;
                    for d in 0..self.nd {
                        let mask = if side == 0 {
                            self.directions[d][ax] > 0.0
                        } else {
                            self.directions[d][ax] < 0.0
                        };
                        let wg = self.weights[d] * self.directions[d][ax].abs();
                        if mask {
                            inc += self.bc[((n * 6 + 2 * ax + side) * self.ng + g) * self.nd + d] * wg;
                        } else {
                            outward[g] += area[ax]
                                * ix.iter()
                                    .map(|c| z[(c * self.ng + g) * self.nd + d] * self.scale)
                                    .sum::<f64>()
                                * wg;
                        }
                    }
                    inward[g] += ix.len() as f64 * area[ax] * inc;
                }
            }
        }
        let source: Vec<f64> = (0..self.ng)
            .map(|g| v * (0..nc).map(|c| self.source[(n * nc + c) * self.ng + g]).sum::<f64>())
            .collect();
        let heat = v * deposition.iter().sum::<f64>();
        let reaction = v * release.iter().sum::<f64>();
        let mut escape = 0.0;
        for c in 0..nc {
            let fr = self.fractions(x, c);
            for g in 0..self.ng {
                let loss = |m: usize| {
                    let k = &self.cards[m];
                    k.absorption[g] * self.energy[g]
                        + (0..self.ng)
                            .map(|t| k.scattering[g][t] * (self.energy[g] - self.energy[t]))
                            .sum::<f64>()
                        + k.release[g]
                };
                escape += v
                    * (0..3).map(|m| fr[m] * (loss(m) - self.cards[m].deposition[g])).sum::<f64>()
                    * phi[c][g];
            }
        }
        let dot = |a: &[f64]| a.iter().zip(&self.energy).map(|(x, e)| x * e).sum::<f64>();
        let net: Vec<f64> = (0..self.ng).map(|g| inward[g] + source[g] - outward[g]).collect();
        vec![
            ("incoming_per_s", inward.iter().sum()),
            ("outgoing_per_s", outward.iter().sum()),
            ("volume_source_per_s", source.iter().sum()),
            ("absorption_per_s", absorb),
            ("particle_balance_per_s", net.iter().sum::<f64>() - absorb),
            ("incoming_energy_W", dot(&inward)),
            ("outgoing_energy_W", dot(&outward)),
            ("source_energy_W", dot(&source)),
            ("reaction_release_W", reaction),
            ("local_deposition_W", heat),
            ("escaped_secondary_energy_W", escape),
            ("energy_balance_W", dot(&net) + reaction - heat - escape),
        ]
    }


    pub fn validate_solution(&self, n: usize, z: &[f64], x: &[f64]) -> CaeResult<Vec<(&'static str, f64)>> {
        let tol = self.p["numerics"]["balance_relative_tolerance"].as_f64().unwrap_or(0.0);
        let peak = z.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
        let min = z.iter().copied().fold(f64::INFINITY, f64::min);
        if !z.iter().all(|v| v.is_finite()) || min < -1e-10 * peak.max(1e-12) {
            return Err(CaeError::convergence("negative or nonfinite angular flux; never clipped"));
        }
        let d = self.ledger(n, z, x);
        if !d.iter().all(|(_, v)| v.is_finite()) {
            return Err(CaeError::convergence("nonfinite neutral particle/energy ledger"));
        }
        let get = |k: &str| d.iter().find(|(n, _)| *n == k).map_or(0.0, |(_, v)| *v);
        let pscale = (get("incoming_per_s") + get("volume_source_per_s")).max(1e-300);
        let escale =
            (get("incoming_energy_W") + get("source_energy_W") + get("reaction_release_W")).max(1e-300);
        if !pscale.is_finite() || !escale.is_finite() {
            return Err(CaeError::convergence("nonfinite neutral balance normalization"));
        }
        if get("particle_balance_per_s").abs() > tol * pscale || get("energy_balance_W").abs() > tol * escale
        {
            return Err(CaeError::convergence("neutral particle/energy balance outside declared tolerance"));
        }
        Ok(d)
    }
}

struct TransportBlock(Arc<NeutralKernel>);

impl HistoryBlockCallbacks for TransportBlock {
    fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        self.0.residual(n, z, old, x)
    }
    fn current_jacobian(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        self.0.jacobian(Kind::Current, n, z, old, x)
    }
    fn previous_jacobian(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        self.0.jacobian(Kind::Previous, n, z, old, x)
    }
    fn design_jacobian(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        self.0.jacobian(Kind::Design, n, z, old, x)
    }
}

struct Drive {
    model: Arc<SolidModel>,
    grad0: Arc<Vec<[[f64; 3]; 4]>>,
    damage: [Vec<f64>; 2],
    width: usize,
}

struct DepositionElement {
    ng: usize,
    nd: usize,
    scale: f64,
    weights: Vec<f64>,
    deposition: Vec<Vec<f64>>,
    thermal_scale: f64,
    drive: Option<Drive>,
}

impl LocalResidual for DepositionElement {
    fn residual<S: Scalar>(&self, item: usize, current: &[S], previous: &[S], design: &[S], out: &mut [S]) {
        let offset = self.drive.as_ref().map_or(0, |d| d.width);
        let psi = &current[offset..];
        let fr = [design[0] * (S::one() - design[4]), design[0] * design[4], S::one() - design[0]];
        let mut heat = S::zero();
        let mut phi = Vec::with_capacity(self.ng);
        for g in 0..self.ng {
            let value = (0..self.nd).fold(S::zero(), |acc, d| {
                acc + psi[g * self.nd + d] * S::from_f64(self.scale * self.weights[d])
            });
            let mix = (0..3).fold(S::zero(), |acc, m| acc + fr[m] * S::from_f64(self.deposition[m][g]));
            heat = heat + mix * value;
            phi.push(value);
        }
        let mut source = S::zero();
        for o in out.iter_mut() {
            *o = S::zero();
        }
        if let Some(drive) = &self.drive {
            let m = &drive.model;
            let ls = m.local_size();
            let solid = &current[..drive.width];
            let old = &previous[..drive.width];
            let forcing: Vec<S> = (0..2)
                .map(|e| {
                    (0..self.ng).fold(S::zero(), |acc, g| acc + phi[g] * S::from_f64(drive.damage[e][g]))
                })
                .collect();
            let mut driven = solid.to_vec();
            driven[ls + 2..ls + 4].copy_from_slice(&forcing);
            let grad0 = &drive.grad0[item];
            let mut with = vec![S::zero(); ls];
            let mut without = vec![S::zero(); ls];
            m.residual(grad0, &driven, old, design, &mut with);
            m.residual(grad0, solid, old, design, &mut without);
            for (o, (a, b)) in out.iter_mut().zip(with.iter().zip(&without)) {
                *o = *a - *b;
            }
            if let Some(history) = &m.history {
                let start = m.layout.material_start();
                let state: Vec<S> =
                    (start..m.internal_size).map(|k| solid[16 + k] * S::from_f64(m.scales[k])).collect();
                let mean = (solid[0] + solid[1] + solid[2] + solid[3]) / S::from_f64(4.0);
                let te = S::from_f64(m.t0) + S::from_f64(m.ts) * mean;
                source = design[0] * history.energy(&state, te, &forcing, design[4]).external;
            }
        }
        let milli = S::from_f64(1e-3);
        let v = design[1] * milli * (design[2] * milli) * (design[3] * milli) / S::from_f64(6.0);
        let value = -((heat - source) * v) / S::from_f64(4.0 * self.thermal_scale);
        for o in out.iter_mut().take(4) {
            *o = *o + value;
        }
    }
}

pub struct DepositionResidual {
    local: LocalResidualAssembly<DepositionElement>,
    width: usize,
    ne: usize,
    drive: Option<Arc<SolidKernel>>,
}

impl DepositionResidual {
    fn data(&self, n: usize) -> (Vec<f64>, Vec<f64>) {
        match &self.drive {
            None => (vec![0.0; self.ne * self.width], vec![0.0; self.ne * self.width]),
            Some(s) => {
                let (a, b) = s.local_data(n);
                let w = s.model.local_width();
                let pad = |v: Vec<f64>| -> Vec<f64> {
                    v.chunks(w)
                        .flat_map(|row| row.iter().copied().chain(std::iter::repeat_n(0.0, self.width)))
                        .collect()
                };
                (pad(a), pad(b))
            }
        }
    }
}

impl HistoryInterface for DepositionResidual {
    fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        let (a, b) = self.data(n);
        self.local.residual(z, old, x, &a, &b)
    }

    fn jacobian(&self, kind: Kind, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        let (a, b) = self.data(n);
        Ok(Jacobian::Csr(self.local.jacobian(kind, z, old, x, &a, &b)?))
    }
}

pub struct CoupledNeutralSource {
    solid: Arc<SolidKernel>,
    t: Arc<NeutralKernel>,
    drive: bool,
    block: HistoryBlock,
    exchange: OnceLock<(Arc<DepositionResidual>, (usize, usize), usize)>,
}

const RESPONSES: [(&str, &str); 3] = [
    ("neutral_local_deposition_W", "W"),
    ("neutral_outgoing_particles_per_s", "1/s"),
    ("neutral_solid_damage_rate_mean_dpa_s", "dpa/s"),
];

impl CoupledNeutralSource {
    fn slice(&self) -> CaeResult<(usize, usize)> {
        self.exchange.get().map(|(_, s, _)| *s).ok_or_else(|| err("neutral transport source is not attached"))
    }

    fn solid_start(&self) -> CaeResult<usize> {
        self.exchange.get().map(|(_, _, s)| *s).ok_or_else(|| err("neutral transport source is not attached"))
    }

    fn required_production(
        &self,
        n: usize,
        zs: &[f64],
        x: &[f64],
        rates: &[[f64; 2]],
    ) -> CaeResult<Vec<f64>> {
        let s = &self.solid;
        let m = &s.model;
        let history = m.history.as_ref().ok_or_else(|| err("driven material history is not bound"))?;
        let start = m.layout.material_start();
        let fields = s.fields(n, zs, x);
        Ok(fields
            .iter()
            .zip(&s.mesh.owners)
            .map(|(f, o)| {
                let e = history.energy(&f.state[start..], f.prop.temperature, &rates[*o], x[s.nc + 3 + o]);
                e.external * x[*o]
            })
            .collect())
    }

    fn volume(&self, x: &[f64]) -> f64 {
        let nc = self.solid.nc;
        (0..3).map(|a| x[nc + a] * 1e-3).product()
    }
}

impl BoundFieldSource for CoupledNeutralSource {
    fn response_units(&self) -> Vec<(String, String)> {
        RESPONSES.iter().map(|(k, u)| ((*k).to_string(), (*u).to_string())).collect()
    }

    fn blocks(&self) -> Vec<HistoryBlock> {
        vec![self.block.clone()]
    }

    fn attach(&self, assembly: &CoupledHistoryAssembly) -> CaeResult<()> {
        let s = &self.solid;
        let t = &self.t;
        let solid_start = block_start(assembly, "solid")?;
        let sl = i64::try_from(solid_start).map_err(|_| err("solid block offset exceeds the index range"))?;
        let tl = assembly
            .slice(BLOCK)
            .ok_or_else(|| err(format!("coupled history assembly has no {BLOCK} block")))?;
        let tl0 = i64::try_from(tl.0).unwrap_or(0);
        let nloc = i64::try_from(t.nloc).unwrap_or(0);
        let m = &s.model;
        let shift = |v: i64| if v < 0 { -1 } else { v + sl };

        let (n_t, n_u) = (s.free_t.len(), s.free_u.len());
        let element_rows = |e: usize, tet: &[usize; 4]| -> Vec<i64> {
            let mut row: Vec<i64> = tet.iter().map(|n| shift(s.tmap[*n])).collect();
            if self.drive {
                for n in tet {
                    for c in 0..3 {
                        row.push(shift(s.umap[3 * n + c]));
                    }
                }
                for k in 0..s.internal_size {
                    row.push(shift(i64::try_from(n_t + n_u + e * s.internal_size + k).unwrap_or(-1)));
                }
            }
            row
        };
        let solid_width = if self.drive { m.local_width() } else { 0 };
        let row_width = if self.drive { m.local_size() } else { 4 };
        let mut rows = Vec::with_capacity(s.ne * row_width);
        let mut current = Vec::with_capacity(s.ne * (solid_width + t.nloc));
        for (e, tet) in s.mesh.tets.iter().enumerate() {
            let row = element_rows(e, tet);
            if self.drive {
                current.extend_from_slice(&row);
                current.extend(std::iter::repeat_n(-1, solid_width - row.len()));
            }
            rows.extend(row);
            let base = tl0 + i64::try_from(s.mesh.owners[e]).unwrap_or(0) * nloc;
            current.extend((0..nloc).map(|k| base + k));
        }
        let drive = if self.drive {
            let damage = |e: usize| t.cards[e].damage.clone();
            Some(Drive {
                model: Arc::clone(m),
                grad0: Arc::new(s.mesh.gradients.clone()),
                damage: [damage(0), damage(1)],
                width: solid_width,
            })
        } else {
            None
        };
        let kernel = DepositionElement {
            ng: t.ng,
            nd: t.nd,
            scale: t.scale,
            weights: t.weights.clone(),
            deposition: t.cards.iter().map(|c| c.deposition.clone()).collect(),
            thermal_scale: m.ks * m.ts * m.ls,
            drive,
        };
        let local = LocalResidualAssembly::new(
            kernel,
            incidence(s.ne, row_width, rows)?,
            incidence(s.ne, solid_width + t.nloc, current.clone())?,
            incidence(s.ne, solid_width + t.nloc, current)?,
            incidence(s.ne, 5, design_incidence(s))?,
            assembly.state_size(),
            assembly.design_size(),
            crate::common::assembly_options(s),
        )?;
        let exchange = Arc::new(DepositionResidual {
            local,
            width: t.nloc,
            ne: s.ne,
            drive: self.drive.then(|| Arc::clone(s)),
        });
        assembly.add_interface(Arc::clone(&exchange) as Arc<dyn HistoryInterface>)?;
        self.exchange
            .set((exchange, tl, solid_start))
            .map_err(|_| err("neutral transport source attached twice"))
    }

    fn exchange(&self) -> Option<Arc<dyn HistoryInterface>> {
        self.exchange.get().map(|(e, _, _)| Arc::clone(e) as Arc<dyn HistoryInterface>)
    }

    fn forcing(&self, _n: usize, z: &[f64], x: &[f64]) -> CaeResult<Option<Vec<f64>>> {
        if !self.drive {
            return Ok(None);
        }
        let tl = self.slice()?;
        let (_, _, rates, _) = self.t.cell_quantities(&z[tl.0..tl.1], x);
        Ok(Some(rates.iter().flatten().copied().collect()))
    }

    fn forcing_jacobians(
        &self,
        _n: usize,
        z: &[f64],
        x: &[f64],
    ) -> CaeResult<Option<(CsrMatrix, CsrMatrix)>> {
        if !self.drive {
            return Ok(None);
        }
        let tl = self.slice()?;
        let t = &self.t;
        let (mut r, mut c, mut v) = (Vec::new(), Vec::new(), Vec::new());
        for cell in 0..t.nc {
            for e in 0..2 {
                for g in 0..t.ng {
                    for d in 0..t.nd {
                        r.push(cell * 2 + e);
                        c.push(tl.0 + (cell * t.ng + g) * t.nd + d);
                        v.push(t.cards[e].damage[g] * t.scale * t.weights[d]);
                    }
                }
            }
        }
        let dz = CsrMatrix::from_triplets(t.nc * 2, z.len(), &r, &c, &v).map_err(|e| err(e.to_string()))?;
        Ok(Some((dz, zero_csr(t.nc * 2, x.len()))))
    }

    fn energy(&self, n: usize, z: &[f64], x: &[f64]) -> CaeResult<SourceEnergy> {
        let tl = self.slice()?;
        let (_, deposition, rates, _) = self.t.cell_quantities(&z[tl.0..tl.1], x);
        let v = self.volume(x);
        let heat = deposition.iter().sum::<f64>() * v;
        let mut source = 0.0;
        if self.drive {
            let s = &self.solid;
            let sl = self.solid_start()?;
            let required = self.required_production(n, &z[sl..sl + s.state_size], x, &rates)?;
            source = required.iter().sum::<f64>() * v / 6.0;
        }
        Ok(SourceEnergy {
            deposition_w: heat,
            sensible_deposition_w: heat - source,
            material_production_w: source,
        })
    }

    fn diagnostics(&self, n: usize, z: &[f64], _old: &[f64], x: &[f64]) -> CaeResult<Map<String, Value>> {
        let tl = self.slice()?;
        let ledger = self.t.validate_solution(n, &z[tl.0..tl.1], x)?;
        let en = self.energy(n, z, x)?;
        if ![en.deposition_w, en.material_production_w, en.sensible_deposition_w]
            .iter()
            .all(|v| v.is_finite())
        {
            return Err(CaeError::convergence("nonfinite transport deposition energy ledger"));
        }
        let s = &self.solid;
        let sl = self.solid_start()?;
        let temperature = s.nodal_temperature(n, &z[sl..sl + s.state_size]);
        if !temperature.iter().all(|v| v.is_finite()) {
            return Err(CaeError::convergence("nonfinite temperature in transport data validity check"));
        }
        for c in 0..s.nc {
            let fr = self.t.fractions(x, c);
            for (i, card) in self.t.cards.iter().enumerate() {
                if fr[i] > 0.0 {
                    let out = s
                        .mesh
                        .tets
                        .iter()
                        .zip(&s.mesh.owners)
                        .filter(|(_, o)| **o == c)
                        .flat_map(|(t, _)| t.iter())
                        .any(|node| {
                            temperature[*node] < card.interval[0] || temperature[*node] > card.interval[1]
                        });
                    if out {
                        return Err(CaeError::convergence(
                            "fixed group transport data outside declared temperature interval; no extrapolation",
                        ));
                    }
                }
            }
        }
        if self.drive {
            let (_, deposition, rates, _) = self.t.cell_quantities(&z[tl.0..tl.1], x);
            let required = self.required_production(n, &z[sl..sl + s.state_size], x, &rates)?;
            let available: Vec<f64> = s.mesh.owners.iter().map(|o| deposition[*o]).collect();
            if !required.iter().chain(&available).all(|v| v.is_finite()) {
                return Err(CaeError::convergence(
                    "nonfinite local deposition or material production energy",
                ));
            }
            if required.iter().zip(&available).any(|(r, a)| *r > a + 1e-8 * a.abs().max(1e-300)) {
                return Err(CaeError::convergence(
                    "material defect-production energy exceeds local deposited energy; data/energy partition invalid",
                ));
            }
        }
        let mut m: Map<String, Value> = ledger.into_iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
        m.insert("deposition_W".into(), json!(en.deposition_w));
        m.insert("material_production_W".into(), json!(en.material_production_w));
        m.insert("sensible_deposition_W".into(), json!(en.sensible_deposition_w));
        m.insert(
            "material_forcing".into(),
            json!(if self.drive { "resolved_current_scalar_flux" } else { "not_bound" }),
        );
        m.insert("state_unknowns".into(), json!(self.t.state_size));
        m.insert("group_count".into(), json!(self.t.ng));
        m.insert("direction_count".into(), json!(self.t.nd));
        m.insert("kernel".into(), json!("quasistatic_nonmultiplying_isotropic_upwind"));
        m.insert("experimental_qualification_verified".into(), json!(false));
        m.insert(
            "data_provenance".into(),
            json!(self.t.cards.iter().map(|c| c.provenance.clone()).collect::<Vec<_>>()),
        );
        Ok(m)
    }

    fn responses(&self, history: &[Vec<f64>], x: &[f64]) -> CaeResult<Vec<f64>> {
        let tl = self.slice()?;
        let n = history.len() - 1;
        let zt = &history[n][tl.0..tl.1];
        let heat = self.energy(n, &history[n], x)?.deposition_w;
        let ledger = self.t.ledger(n, zt, x);
        let outgoing = ledger[1].1;
        let (_, _, rates, _) = self.t.cell_quantities(zt, x);
        let nc = self.solid.nc;
        let den = x[..nc].iter().sum::<f64>().max(1e-15);
        let mean = (0..nc)
            .map(|c| x[c] * ((1.0 - x[nc + 3 + c]) * rates[c][0] + x[nc + 3 + c] * rates[c][1]))
            .sum::<f64>()
            / den;
        Ok(vec![heat, outgoing, mean])
    }

    #[allow(clippy::many_single_char_names)]
    fn response_vjp(&self, history: &[Vec<f64>], x: &[f64], weights: &[f64]) -> CaeResult<ResponseVjp> {
        let tl = self.slice()?;
        let t = &self.t;
        let n = history.len() - 1;
        let zt = &history[n][tl.0..tl.1];
        let nc = self.solid.nc;
        let w = |k: usize| weights.get(k).copied().unwrap_or(0.0);
        let h: [f64; 3] = std::array::from_fn(|a| x[nc + a] * 1e-3);
        let v = h[0] * h[1] * h[2];
        let (phi, deposition, rates, _) = t.cell_quantities(zt, x);
        let mut phi_bar = vec![vec![0.0; t.ng]; nc];
        let mut psi_bar = vec![0.0; zt.len()];
        let mut xb = vec![0.0; x.len()];

        let total_dep: f64 = deposition.iter().sum();
        for c in 0..nc {
            let fr = t.fractions(x, c);
            let (rho, ph) = (x[c], x[nc + 3 + c]);
            let dfr_rho = [1.0 - ph, ph, -1.0];
            let dfr_c = [-rho, rho, 0.0];
            for g in 0..t.ng {
                let mix: f64 = (0..3).map(|m| fr[m] * t.cards[m].deposition[g]).sum();
                phi_bar[c][g] += w(0) * v * mix;
                xb[c] +=
                    w(0) * v * (0..3).map(|m| dfr_rho[m] * t.cards[m].deposition[g]).sum::<f64>() * phi[c][g];
                xb[nc + 3 + c] +=
                    w(0) * v * (0..3).map(|m| dfr_c[m] * t.cards[m].deposition[g]).sum::<f64>() * phi[c][g];
            }
        }
        for a in 0..3 {
            xb[nc + a] += w(0) * total_dep * v / h[a] * 1e-3;
        }

        for ax in 0..3 {
            let area = v / h[ax];
            for side in 0..2 {
                let ix = t.cell_ids(ax, side);
                let mut face_sum = 0.0;
                for d in 0..t.nd {
                    let mask = if side == 0 { t.directions[d][ax] > 0.0 } else { t.directions[d][ax] < 0.0 };
                    if mask {
                        continue;
                    }
                    let wg = t.weights[d] * t.directions[d][ax].abs();
                    for c in &ix {
                        for g in 0..t.ng {
                            let k = (c * t.ng + g) * t.nd + d;
                            psi_bar[k] += w(1) * area * wg * t.scale;
                            face_sum += zt[k] * t.scale * wg;
                        }
                    }
                }
                for b in (0..3).filter(|b| *b != ax) {
                    let others: f64 = (0..3).filter(|q| *q != ax && *q != b).map(|q| h[q]).product();
                    xb[nc + b] += w(1) * face_sum * others * 1e-3;
                }
            }
        }

        let sum_rho: f64 = x[..nc].iter().sum();
        let den = sum_rho.max(1e-15);
        let num: f64 =
            (0..nc).map(|c| x[c] * ((1.0 - x[nc + 3 + c]) * rates[c][0] + x[nc + 3 + c] * rates[c][1])).sum();
        for c in 0..nc {
            let (rho, ph) = (x[c], x[nc + 3 + c]);
            for g in 0..t.ng {
                phi_bar[c][g] +=
                    w(2) * rho * ((1.0 - ph) * t.cards[0].damage[g] + ph * t.cards[1].damage[g]) / den;
            }
            xb[c] += w(2) * ((1.0 - ph) * rates[c][0] + ph * rates[c][1]) / den;
            if sum_rho > 1e-15 {
                xb[c] -= w(2) * num / (den * den);
            }
            xb[nc + 3 + c] += w(2) * rho * (rates[c][1] - rates[c][0]) / den;
        }
        for c in 0..nc {
            for g in 0..t.ng {
                for d in 0..t.nd {
                    psi_bar[(c * t.ng + g) * t.nd + d] += phi_bar[c][g] * t.scale * t.weights[d];
                }
            }
        }
        let mut states: Vec<Vec<f64>> = history.iter().map(|z| vec![0.0; z.len()]).collect();
        states[n][tl.0..tl.1].copy_from_slice(&psi_bar);
        Ok((states, xb))
    }

    fn fields(
        &self,
        history: &[Vec<f64>],
        x: &[f64],
    ) -> CaeResult<(BTreeMap<String, FieldValue>, Map<String, Value>)> {
        let tl = self.slice()?;
        let t = &self.t;
        let nt = history.len();
        let grid = t.grid;
        let q: Vec<_> = history.iter().map(|z| t.cell_quantities(&z[tl.0..tl.1], x)).collect();
        let last = &q[nt - 1];
        let mut values: Vec<(&str, Vec<f64>, Vec<usize>)> = vec![
            ("neutral_scalar_flux_m2_s", last.0.iter().map(|r| r.iter().sum()).collect(), grid.to_vec()),
            ("neutral_deposition_W_m3", last.1.clone(), grid.to_vec()),
            ("neutral_endpoint0_damage_rate_dpa_s", last.2.iter().map(|r| r[0]).collect(), grid.to_vec()),
            ("neutral_endpoint1_damage_rate_dpa_s", last.2.iter().map(|r| r[1]).collect(), grid.to_vec()),
            (
                "neutral_group_scalar_flux_history_m2_s",
                q.iter().flat_map(|x| x.0.iter().flatten().copied()).collect(),
                vec![nt, t.nc, t.ng],
            ),
            (
                "neutral_damage_rate_history_dpa_s",
                q.iter().flat_map(|x| x.2.iter().flatten().copied()).collect(),
                vec![nt, t.nc, 2],
            ),
            (
                "neutral_angular_flux_history_m2_s_sr",
                history
                    .iter()
                    .flat_map(|z| z[tl.0..tl.1].iter().map(|v| v * t.scale).collect::<Vec<_>>())
                    .collect(),
                vec![nt, t.nc, t.ng, t.nd],
            ),
            ("neutral_direction_vectors", t.directions.iter().flatten().copied().collect(), vec![t.nd, 3]),
            ("neutral_angular_weights_sr", t.weights.clone(), vec![t.nd]),
            ("neutral_group_energies_J", t.energy.clone(), vec![t.ng]),
        ];
        let mut out = BTreeMap::new();
        let mut meta = Map::new();
        for (name, data, shape) in values.drain(..) {
            let units = if name.contains("flux_m2_s") || name.ends_with("_m2_s") {
                "1/(m^2*s)"
            } else if name.contains("dpa_s") {
                "dpa/s"
            } else if name.contains("W_m3") {
                "W/m^3"
            } else if name.ends_with("_sr") && name.contains("flux") {
                "1/(m^2*s*sr)"
            } else if name.ends_with("_sr") {
                "sr"
            } else if name.ends_with("_J") {
                "J"
            } else {
                "1"
            };
            let mut row = json!({"units": units, "source": "native_neutral_transport_solver", "rank": "scalar", "association": "exact_transport_history"});
            if shape == grid {
                row["association"] = json!("cell");
            }
            if name.contains("endpoint") {
                row["interpretation"] =
                    json!("potential_response_of_named_material_endpoint_per_fluence_not_bulk_dose");
            }
            if name.contains("angular_flux_history") {
                row["axes"] = json!(["time", "cell", "energy_group", "ordinate"]);
            } else if name.contains("group_scalar_flux_history") {
                row["axes"] = json!(["time", "cell", "energy_group"]);
            } else if name.contains("damage_rate_history") {
                row["axes"] = json!(["time", "cell", "ordered_solid_endpoint"]);
            }
            meta.insert(name.to_string(), row);
            out.insert(name.to_string(), FieldValue::Array(array(data, &shape)?));
        }
        Ok((out, meta))
    }
}

#[must_use]
pub fn editor_schema(settings: &Value, _context: &Value) -> Value {
    if !settings.is_object() {
        return json!({});
    }
    let u = units();
    let values = |title: &str, unit: &Value, description: &str| json!({"title": title, "units": unit, "type": "array", "description": description});
    let card_fields = json!({
        "name": {"title": "Material data name"},
        "host_material_fingerprint": {"title": "Host material identity", "description": "Exact fingerprint of the corresponding authored solid endpoint or fluid material."},
        "provenance": {"title": "Processed data source and calibration provenance"},
        "temperature_interval_K": values("Data validity interval", &json!("K"), "Two values: minimum and maximum valid material temperature; no extrapolation."),
        "absorption_m_inv": values("Group absorption", &u["absorption_m_inv"], "One nonnegative coefficient per energy group."),
        "scattering_from_to_m_inv": values("Group scattering", &u["scattering_from_to_m_inv"], "Matrix [source group, destination group]. Includes diagonal within-group scattering."),
        "reaction_release_J_m": values("Reaction energy release", &u["reaction_release_J_m"], "One nonnegative response per group."),
        "local_deposition_J_m": values("Local energy deposition", &u["local_deposition_J_m"], "One nonnegative response per group, bounded by collision energy removal plus declared reaction release."),
        "damage_response_m2": values("Damage response", &u["damage_response_m2"], "One nonnegative response per group. Solid endpoints provide the two material-state dose channels.")});
    let prefix: Vec<Value> = ["Solid endpoint 1", "Solid endpoint 2", "Fluid material"]
        .iter()
        .map(|n| json!({"title": n, "properties": card_fields}))
        .collect();
    let cards = json!({"title": "Ordered material data", "type": "array", "minItems": 3, "maxItems": 3, "prefixItems": prefix});
    let transport = json!({"title": "Processed multigroup transport data", "properties": {
        "name": {"title": "Transport setup name"}, "provenance": {"title": "Transport data provenance"},
        "energy_edges_J": values("Energy-group edges", &json!("J"), "Strictly descending edges; one more edge than groups."),
        "group_energy_J": values("Representative group energies", &json!("J"), "One energy strictly inside each corresponding group interval; 1–16 groups."),
        "quadrature": {"title": "Angular quadrature", "description": "Even polar count and azimuth count divisible by four; at most 128 directions in total.", "properties": {
            "n_mu": {"title": "Polar nodes", "type": "integer", "minimum": 2, "maximum": 32, "multipleOf": 2},
            "n_phi": {"title": "Azimuth directions", "type": "integer", "minimum": 4, "maximum": 64, "multipleOf": 4}}},
        "materials": cards,
        "incoming_angular_flux": values("Incoming boundary angular flux", &u["incoming_angular_flux"], "Array [time, face, group, direction]. Faces: −x, +x, −y, +y, −z, +z. Incoming directions only; initial node zero. Absolute SI rates, not per source particle."),
        "isotropic_volume_source": values("Isotropic volume source", &u["isotropic_volume_source"], "Array [time, native C-order cell, group]. Angle-integrated absolute source; initial node zero; right-endpoint sampling."),
        "numerics": {"title": "Transport numerical controls", "properties": {
            "angular_flux_scale": {"title": "Angular-flux reference scale", "units": u["incoming_angular_flux"], "type": "number", "exclusiveMinimum": 0},
            "length_scale_m": {"title": "Residual reference length", "units": "m", "type": "number", "exclusiveMinimum": 0},
            "batch_size": {"title": "Assembly batch size", "type": "integer", "minimum": 1, "maximum": 256},
            "max_estimated_bytes": {"title": "Assembly memory budget", "units": "bytes", "type": "integer", "minimum": 1},
            "balance_relative_tolerance": {"title": "Relative balance tolerance", "type": "number", "exclusiveMinimum": 0, "maximum": 1e-5}}}}});
    json!({"properties": {"transport": transport,
        "drive_material_history": {"title": "Drive material damage from transport", "type": "boolean", "description": "Requires a compatible material-state component and zero prescribed dose; deposition also accounts for defect storage."},
        "energy_convention": {"title": "Energy accounting", "enum": [ENERGY_CONVENTION]}}})
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NeutralDeposition;

fn validate(s: &Value, context: &Value) -> CaeResult<Value> {
    let keys = ["transport", "drive_material_history", "energy_convention"];
    let ok = s.as_object().is_some_and(|m| m.len() == 3 && keys.iter().all(|k| m.contains_key(*k)))
        && s["drive_material_history"].is_boolean();
    if !ok {
        return Err(err("explicit transport, forcing selection and energy convention required"));
    }
    if s["energy_convention"] != json!(ENERGY_CONVENTION) {
        return Err(err(
            "transport deposition must exclude double-counting in authored heat and include its own defect storage",
        ));
    }
    let p = normalise(&s["transport"], context)?;
    if s["drive_material_history"] == json!(true) {
        let solid = &context["solid"];
        let binding = implexity_physics_solid::history::bind(
            solid.get("material_history").unwrap_or(&Value::Null),
            solid,
        )?;
        let compatible = binding.as_ref().and_then(|b| b.component.external_forcing_contract())
            == Some(json!({"quantity": "ordered_endpoint_damage_rate", "units": "dpa/s", "channels": 2,
                "energy": "external_energy_W_m3_is_nonthermal_storage_source"}));
        if !compatible {
            return Err(err(
                "selected material has no compatible ordered endpoint damage-rate port/energy contract",
            ));
        }
        if binding.as_ref().is_some_and(|b| b.forcing_values.iter().any(|v| *v != 0.0)) {
            return Err(err(
                "externally driven material requires zero prescribed forcing; sources are not silently added/replaced",
            ));
        }
    }
    let mut out = s.as_object().cloned().unwrap_or_default();
    out.insert("transport".into(), p);
    Ok(Value::Object(out))
}

impl HistorySourceComponent for NeutralDeposition {
    fn validate(&self, settings: &Value, context: &dyn Any) -> CaeResult<Value> {
        let context = context
            .downcast_ref::<Value>()
            .ok_or_else(|| err("neutral transport requires a host context"))?;
        validate(settings, context)
    }

    fn create(&self, settings: &Value, host: &dyn Any) -> CaeResult<Box<dyn Any + Send + Sync>> {
        let host = host_of(host)?;
        let p = validate(settings, host.problem())?;
        let drive = p["drive_material_history"] == json!(true);
        if drive && host.solid().model.history.as_ref().is_none_or(|h| h.forcing_shape[2] != 2) {
            return Err(err(
                "selected material has no compatible ordered endpoint damage-rate port/energy contract",
            ));
        }
        let t = Arc::new(NeutralKernel::new(&p["transport"], host.problem())?);
        let block = HistoryBlock {
            name: BLOCK.into(),
            initial: vec![0.0; t.state_size],
            design_indices: (0..t.design_size).collect(),
            callbacks: Arc::new(TransportBlock(Arc::clone(&t))),
            field: None,
        };
        Ok(Box::new(BoundSource(Arc::new(CoupledNeutralSource {
            solid: Arc::clone(host.solid()),
            t,
            drive,
            block,
            exchange: OnceLock::new(),
        }))))
    }

    fn coupling(&self, base: Value, settings: &Value) -> CaeResult<Value> {
        let node = BLOCK;
        let mut edges = vec![(
            node,
            "thermal",
            "resolved_local_energy_deposition",
            "monolithic",
            "same sparse field system; geometry/material-dependent group transport",
        )];
        if settings["drive_material_history"] == json!(true) {
            edges.push((
                node,
                "material_state",
                "resolved_endpoint_damage_rate",
                "monolithic",
                "current group scalar flux drives the material residual, not a prescribed dose history",
            ));
        }
        extend_coupling(
            &base,
            &[node],
            &edges,
            &[],
            &["Neutral transport uses fixed processed material data; temperature/depletion feedback to cross sections is NOT claimed.".to_string()],
        )
    }

    fn owns_material_forcing(&self, settings: &Value) -> bool {
        settings["drive_material_history"] == json!(true)
    }
}

impl FieldSourceAuthoring for NeutralDeposition {
    fn editor_schema(&self, settings: &Value, context: &Value) -> Value {
        editor_schema(settings, context)
    }
}
