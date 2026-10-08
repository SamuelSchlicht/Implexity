// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;
use implexity_linalg::sparse::CsrMatrix;
use implexity_physics_cfd::pyfmt::fmt_g;
use implexity_physics_solid::phase_stress_transfer::DensityJumpStressTransfer;
use implexity_physics_solid::solid_elements::SolidModel;
use implexity_physics_solid::solid_history::SolidKernel;
use implexity_solve::factorization::Factorization;
use implexity_solve::local_assembly::{
    AssemblyOptions, Incidence, Kind, LocalResidual, LocalResidualAssembly,
};
use implexity_solve::matrix::Jacobian;

use crate::incompressible_transport::FluidKernel;
use crate::unified_history::FluidNodalDualVolume;

pub const SCHEMA: &str = "implexity-elastic-pressure-initialization/1";

fn contract<T>(message: impl Into<String>) -> CaeResult<T> {
    Err(CaeError::contract(message))
}

fn lin(e: impl std::fmt::Display) -> CaeError {
    CaeError::contract(e.to_string())
}


pub fn normalise_initialization(raw: &Value) -> CaeResult<Value> {
    let keys = ["method", "provenance", "schema", "yield_margin_fraction"];
    let ok =
        raw.as_object().is_some_and(|m| m.len() == keys.len() && keys.iter().all(|k| m.contains_key(*k)));
    if !ok || raw["schema"] != SCHEMA {
        return contract("explicit versioned elastic pressure initialization required");
    }
    if raw["method"] != "equilibrated_elastic_pressure" {
        return contract("unsupported pressure initialization method");
    }
    let margin = match &raw["yield_margin_fraction"] {
        Value::Number(n) => n.as_f64().filter(|m| m.is_finite() && *m > 0.0 && *m < 1.0),
        _ => None,
    };
    let Some(margin) = margin else {
        return contract("elastic initialization requires a positive yield-surface margin below one");
    };
    if raw["provenance"].as_str().is_none_or(|s| s.trim().is_empty()) {
        return contract("initialization provenance required");
    }
    let mut out = raw.clone();
    out["yield_margin_fraction"] = json!(margin);
    Ok(out)
}

pub struct PreloadHost {
    pub s: Arc<SolidKernel>,
    pub f: Arc<FluidKernel>,
    pub transfer: Arc<DensityJumpStressTransfer<FluidKernel>>,
    pub volume_transfer: Arc<FluidNodalDualVolume>,
    pub p: Value,
    pub p_map: CsrMatrix,
    pub offset0: Vec<f64>,
    pub base: Vec<f64>,
    pub retained: Vec<usize>,
    pub solid_start: usize,
    pub fluid_slice: std::ops::Range<usize>,
    pub design_size: usize,
    pub has_sources: bool,
    pub condition_limit: f64,
}

struct PreloadElement {
    model: Arc<SolidModel>,
    grad0: Vec<[[f64; 3]; 4]>,
    internal: Vec<Vec<f64>>,
    forcing: Vec<Vec<f64>>,
}

impl LocalResidual for PreloadElement {
    fn residual<S: Scalar>(&self, item: usize, current: &[S], _previous: &[S], design: &[S], out: &mut [S]) {
        let m = &self.model;
        let ls = m.local_size();
        let width = m.local_width();
        let mut cur = vec![S::zero(); width];
        let mut old = vec![S::zero(); width];
        for (j, v) in self.internal[item].iter().enumerate() {
            cur[16 + j] = S::from_f64(*v);
            old[16 + j] = S::from_f64(*v);
        }
        cur[4..16].copy_from_slice(&current[..12]);
        for target in [&mut cur, &mut old] {
            target[ls] = S::from_f64(1.0);
            target[ls + 1] = S::zero();
            for (k, v) in self.forcing[item].iter().enumerate() {
                target[ls + 2 + k] = S::from_f64(*v);
            }
        }
        let mut full = vec![S::zero(); ls];
        m.residual(&self.grad0[item], &cur, &old, design, &mut full);
        out.copy_from_slice(&full[4..16]);
    }
}

struct Computed {
    key: Vec<u64>,
    state: Vec<f64>,
    u: Vec<f64>,
    factorization: Arc<Factorization>,
    report: Value,
}

pub struct ElasticPressurePreload {
    host: PreloadHost,
    policy: Value,
    local: LocalResidualAssembly<PreloadElement>,
    template: Vec<f64>,
    base_full: Vec<f64>,
    full_rows: Vec<usize>,
    reduced_rows: Vec<usize>,
    cache: Mutex<Option<Arc<Computed>>>,
}

impl std::fmt::Debug for ElasticPressurePreload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ElasticPressurePreload").field("policy", &self.policy).finish_non_exhaustive()
    }
}

impl ElasticPressurePreload {

    pub fn new(host: PreloadHost, policy: &Value) -> CaeResult<Self> {
        let policy = normalise_initialization(policy)?;
        let s = Arc::clone(&host.s);
        let m = &s.model;
        if m.viscoelastic.is_some()
            || m.creep.is_some()
            || host.has_sources
            || m.law.reversible_thermoelastic()
        {
            return contract(
                "elastic pressure initialization does not admit creep, viscoelastic or additional field-source preload; author a supported full loading history",
            );
        }
        if let Some(mh) = &m.history {
            if !mh.effects.iter().all(|e| ["k", "yield_stress", "creep_rate_ref"].contains(e)) {
                return contract(
                    "elastic preload does not admit evolving elastic/caloric/eigenstrain properties",
                );
            }
            mh.check_state(&mh.initial)?;
        }
        if let Some(acc) = host.f.p.get("body_acceleration").filter(|v| !v.is_null())
            && acc["acceleration_m_s2"].as_array().into_iter().flatten().any(|v| v.as_f64() != Some(0.0))
        {
            return contract("uniform-pressure initial equilibrium is not a hydrostatic gravity solve");
        }
        let nu = s.n_u();
        if nu < 1 {
            return contract("pressure initialization requires free solid displacement coordinates");
        }
        if s.fixed_t[0].iter().any(|t| *t != m.t0) || s.fixed_u[0].iter().any(|u| *u != 0.0) {
            return contract(
                "elastic pressure preload requires the uniform undeformed reference-temperature state",
            );
        }
        let mut mapping = vec![-1i64; s.nn * 3];
        for (k, dof) in s.free_u.iter().enumerate() {
            mapping[*dof] = k as i64;
        }
        let mut incidence = Vec::with_capacity(s.ne * 12);
        let mut design = Vec::with_capacity(s.ne * 5);
        for (e, tet) in s.mesh.tets.iter().enumerate() {
            for node in tet {
                for c in 0..3 {
                    incidence.push(mapping[3 * node + c]);
                }
            }
            let o = s.mesh.owners[e] as i64;
            let nc = s.nc as i64;
            design.extend([o, nc, nc + 1, nc + 2, nc + 3 + o]);
        }
        let initial = s.initial_material_state();
        let base = s.n_t() + nu;
        let internal: Vec<Vec<f64>> = (0..s.ne)
            .map(|e| initial[base + e * s.internal_size..base + (e + 1) * s.internal_size].to_vec())
            .collect();
        let forcing: Vec<Vec<f64>> = (0..s.ne)
            .map(|e| {
                m.history.as_ref().map(|mh| mh.forcing_row(0, s.mesh.owners[e]).to_vec()).unwrap_or_default()
            })
            .collect();
        let batch = s.p["assembly"]["batch_size"].as_u64().map_or(64, |b| b as usize).max(1);
        let local = LocalResidualAssembly::new(
            PreloadElement { model: Arc::clone(m), grad0: s.mesh.gradients.clone(), internal, forcing },
            Incidence::new(s.ne, 12, incidence.clone())?,
            Incidence::new(s.ne, 12, incidence.clone())?,
            Incidence::new(s.ne, 12, incidence)?,
            Incidence::new(s.ne, 5, design)?,
            nu,
            host.design_size,
            AssemblyOptions { batch_size: batch, ..AssemblyOptions::default() },
        )?;
        let base_full = {
            let pz = host.p_map.matvec(&host.base).map_err(lin)?;
            pz.iter().zip(&host.offset0).map(|(a, b)| a + b).collect::<Vec<f64>>()
        };
        let drs = s.displacement_row_slice();
        let full_rows: Vec<usize> = drs.clone().map(|r| host.solid_start + r).collect();
        let mut reduced_rows = Vec::with_capacity(full_rows.len());
        for r in &full_rows {
            match host.retained.binary_search(r) {
                Ok(i) => reduced_rows.push(i),
                Err(_) => return contract("preload displacement layout disagrees with the reduced history"),
            }
        }
        Ok(Self {
            template: vec![0.0; s.ne * 12],
            host,
            policy,
            local,
            base_full,
            full_rows,
            reduced_rows,
            cache: Mutex::new(None),
        })
    }

    #[must_use]
    pub fn base(&self) -> &[f64] {
        &self.host.base
    }

    fn force(&self, x: &[f64]) -> CaeResult<Vec<f64>> {
        let s = &self.host.s;
        let spacing = [x[s.nc], x[s.nc + 1], x[s.nc + 2]];
        let boundary = s.boundary_load(0, &spacing);
        let transfer = self.host.transfer.residual(0, &self.base_full, &self.base_full, x)?;
        let drs = s.displacement_row_slice();
        Ok(drs.zip(&self.full_rows).map(|(r, f)| boundary[r] + transfer[*f]).collect())
    }

    fn force_jacobian(&self, x: &[f64]) -> CaeResult<CsrMatrix> {
        let s = &self.host.s;
        let dc = self.host.transfer.jacobian(Kind::Design, 0, &self.base_full, &self.base_full, x)?;
        let (mut rows, mut cols, mut vals) = (Vec::new(), Vec::new(), Vec::new());
        for (i, r) in self.full_rows.iter().enumerate() {
            let (idx, val) = dc.row(*r);
            for (c, v) in idx.iter().zip(val) {
                rows.push(i);
                cols.push(*c);
                vals.push(*v);
            }
        }
        let spacing: [Dual<3>; 3] = std::array::from_fn(|a| Dual::variable(x[s.nc + a], a));
        let load = s.boundary_load(0, &spacing);
        for (i, r) in s.displacement_row_slice().enumerate() {
            for a in 0..3 {
                let d = load[r].eps[a];
                if d != 0.0 {
                    rows.push(i);
                    cols.push(s.nc + a);
                    vals.push(d);
                }
            }
        }
        CsrMatrix::from_triplets(s.n_u(), self.host.design_size, &rows, &cols, &vals).map_err(lin)
    }

    fn expand0(&self, z: &[f64]) -> CaeResult<Vec<f64>> {
        let pz = self.host.p_map.matvec(z).map_err(lin)?;
        Ok(pz.iter().zip(&self.host.offset0).map(|(a, b)| a + b).collect())
    }

    #[allow(clippy::too_many_lines)]
    fn compute(&self, x: &[f64]) -> CaeResult<Arc<Computed>> {
        let h = &self.host;
        if x.len() != h.design_size || x.iter().any(|v| !v.is_finite()) {
            return contract("invalid preload design");
        }
        let key: Vec<u64> = x.iter().map(|v| v.to_bits()).collect();
        let mut cache = self.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(c) = cache.as_ref()
            && c.key == key
        {
            return Ok(Arc::clone(c));
        }
        let s = &h.s;
        let nu = s.n_u();
        let zero = vec![0.0; nu];
        let stiffness =
            self.local.jacobian(Kind::Current, &zero, &zero, x, &self.template, &self.template)?;
        let trace =
            implexity_solve::trace_fields! {"purpose" => "design_dependent_elastic_pressure_initialization"};
        let fac = Factorization::new(Jacobian::Csc(stiffness.to_csc()), nu, h.condition_limit, Some(&trace))?;
        let force = self.force(x)?;
        let rhs: Vec<f64> = force.iter().map(|v| -v).collect();
        let (u, _, linear_relative) = fac.solve(&rhs, false)?;
        let residual = self.local.residual(&u, &zero, x, &self.template, &self.template)?;
        let norm = residual.iter().zip(&force).map(|(r, f)| (r + f) * (r + f)).sum::<f64>().sqrt();
        if norm > h.p["numerics"]["tolerance"].as_f64().unwrap_or(0.0) {
            return Err(CaeError::convergence(
                "elastic preload failed the common mechanical residual tolerance",
            ));
        }
        let mut state = h.base.clone();
        for (r, v) in self.reduced_rows.iter().zip(&u) {
            state[*r] = *v;
        }
        let full = self.expand0(&state)?;
        let sl = h.solid_start..h.solid_start + s.state_size;
        let zs = &full[sl];
        s.check(0, zs, zs, x)?;
        h.volume_transfer.check(0, &full, &full, x)?;
        let fz = &full[h.fluid_slice.clone()];
        h.f.check(0, fz, fz, &x[..s.nc + 3])?;
        let observed = s.observe(0, zs, zs, x);
        let max_strain = observed
            .strain
            .iter()
            .map(|e| e.iter().map(|v| v * v).sum::<f64>().sqrt())
            .fold(f64::NEG_INFINITY, f64::max);
        let limit = s.p["numerics"]["max_small_strain"].as_f64().unwrap_or(f64::NAN);
        if max_strain > limit {
            return Err(CaeError::convergence(format!(
                "elastic preload outside small-strain validity: maximum strain norm {} exceeds max_small_strain={}; no reference state was accepted",
                fmt_g(max_strain, 4),
                fmt_g(limit, 6)
            )));
        }
        let spacing = [x[s.nc] * 1e-3, x[s.nc + 1] * 1e-3, x[s.nc + 2] * 1e-3];
        let screen = crate::unified_history::fixed_geometry_validity(
            &observed.displacement_nodes,
            &spacing,
            &x[..s.nc],
            &s.mesh.tets,
            &s.mesh.owners,
            &h.p["validity"],
        );
        if screen["displacement_over_cell_screen_passed"] != json!(true)
            || screen["channel_width_screen_passed"] != json!(true)
        {
            return Err(CaeError::convergence(format!(
                "elastic preload rejected, no reference state was accepted: {}",
                crate::unified_history::fixed_geometry_failure_message(&screen, 0)
            )));
        }
        let fields = s.fields(0, zs, x);
        let margin: Vec<f64> = observed
            .yield_residual
            .iter()
            .zip(&fields)
            .map(|(r, f)| -r / f.prop.get(implexity_physics_solid::material::idx::YIELD))
            .collect();
        let required = self.policy["yield_margin_fraction"].as_f64().unwrap_or(f64::NAN);
        if margin.iter().any(|m| !m.is_finite()) || margin.iter().any(|m| *m < required) {
            return Err(CaeError::convergence(
                "pressure preload is not strictly elastic with the authored yield margin; an inelastic preload history is required",
            ));
        }
        let m = &s.model;
        let force_norm = force.iter().map(|v| v * v).sum::<f64>().sqrt();
        let report = json!({
            "schema": SCHEMA,
            "method": self.policy["method"],
            "design_dependent": true,
            "equilibrium_residual_norm": norm,
            "free_force_residual_N": norm * m.ss * m.ls * m.ls,
            "initial_force_norm_N": force_norm * m.ss * m.ls * m.ls,
            "linear_relative_residual": linear_relative,
            "condition_estimate": fac.condition(),
            "maximum_strain_norm": max_strain,
            "fixed_geometry_validity": screen,
            "minimum_relative_yield_margin": margin.iter().copied().fold(f64::INFINITY, f64::min),
            "pressure_is_absolute_and_unchanged": true,
            "preload_heat_or_plastic_state_invented": false,
            "initial_state_design_derivative": "implicit_equilibrium_pullback",
            "initial_material_history": if m.history.is_some() {
                "prescribed_inventory_preserved_without_elapsed_time"
            } else {
                "not_selected"
            },
            "initial_yield_test": "current_initial_material_properties",
            "physical_qualification": false,
        });
        let computed = Arc::new(Computed { key, state, u, factorization: Arc::new(fac), report });
        *cache = Some(Arc::clone(&computed));
        Ok(computed)
    }


    pub fn state(&self, x: &[f64]) -> CaeResult<Vec<f64>> {
        Ok(self.compute(x)?.state.clone())
    }


    pub fn report(&self, x: &[f64]) -> CaeResult<Value> {
        Ok(self.compute(x)?.report.clone())
    }


    pub fn pullback(&self, x: &[f64], covectors: &DenseMatrix) -> CaeResult<DenseMatrix> {
        let result = self.compute(x)?;
        if covectors.nrows != self.host.base.len() || covectors.data.iter().any(|v| !v.is_finite()) {
            return contract("invalid initial-state covectors");
        }
        let m = covectors.ncols;
        let nu = self.reduced_rows.len();
        let mut rhs = vec![0.0; nu * m];
        for (i, r) in self.reduced_rows.iter().enumerate() {
            rhs[i * m..(i + 1) * m].copy_from_slice(&covectors.data[r * m..(r + 1) * m]);
        }
        let adjoint = result.factorization.solve_block(&rhs, m, true)?.solution;
        let zero = vec![0.0; nu];
        let design =
            self.local.jacobian(Kind::Design, &result.u, &zero, x, &self.template, &self.template)?;
        let derivative = design.add_scaled(1.0, &self.force_jacobian(x)?, 1.0).map_err(lin)?;
        let mut out = DenseMatrix::zeros(self.host.design_size, m);
        for i in 0..nu {
            let (idx, val) = derivative.row(i);
            for (c, v) in idx.iter().zip(val) {
                for j in 0..m {
                    out.data[c * m + j] -= v * adjoint[i * m + j];
                }
            }
        }
        Ok(out)
    }
}
