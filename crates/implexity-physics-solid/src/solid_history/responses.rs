// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Value, json};

use implexity_ad::{Dual, Scalar};
use implexity_core::CaeError;
use implexity_linalg::dense::DenseMatrix;

use super::SolidKernel;
use crate::inelastic::YIELD_SWITCH_CERTIFICATE;
use crate::mandel::{self, Mandel};
use crate::material::idx;
use crate::util::{contract, convergence};

const W: usize = 8;

#[derive(Debug, Clone, Default)]
pub struct Observed {
    pub material_state: Vec<Vec<f64>>,
    pub material_stored_energy: Vec<f64>,
    pub material_evolution_heat: Vec<f64>,
    pub material_external_energy: Vec<f64>,
    pub conductivity: Vec<f64>,
    pub yield_stress: Vec<f64>,
    pub temperature_nodes: Vec<f64>,
    pub displacement_nodes: Vec<[f64; 3]>,
    pub strain: Vec<[f64; 6]>,
    pub stress: Vec<[f64; 6]>,
    pub von_mises: Vec<f64>,
    pub equivalent_plastic: Vec<f64>,
    pub equivalent_creep: Vec<f64>,
    pub plastic_strain: Vec<[f64; 6]>,
    pub creep_strain: Vec<[f64; 6]>,
    pub heat_increment: Vec<f64>,
    pub plastic_dissipation: Vec<f64>,
    pub creep_dissipation: Vec<f64>,
    pub elastic_energy: f64,
    pub mass: f64,
    pub cell_volume: f64,
    pub material_temperature: Vec<f64>,
    pub backstress: Vec<[f64; 6]>,
    pub yield_residual: Vec<f64>,
    pub velocity: Vec<[f64; 3]>,
    pub acceleration: Vec<[f64; 3]>,
    pub kinetic_energy: f64,
    pub external_force: Vec<[f64; 3]>,
    pub polymer: Vec<Vec<f64>>,
    pub thermoelastic_defect: Option<Vec<f64>>,
}

pub const POLYMER_COLUMNS: [&str; 11] = [
    "viscoelastic_stored_energy_J_m3",
    "viscoelastic_mechanical_stored_energy_J_m3",
    "viscoelastic_dissipation_increment_J_m3",
    "viscoelastic_heat_increment_J_m3",
    "viscoelastic_coefficient_exchange_J_m3",
    "viscoelastic_chemical_release_J_m3",
    "viscoelastic_numerical_dissipation_increment_J_m3",
    "viscoelastic_energy_balance_residual_J_m3",
    "viscoelastic_strain_margin",
    "viscoelastic_assembled_heat_increment_J_m3",
    "viscoelastic_assembled_numerical_dissipation_increment_J_m3",
];

impl Observed {
    #[must_use]
    pub fn polymer_column(&self, name: &str) -> Vec<f64> {
        let full = format!("viscoelastic_{name}");
        let Some(k) = POLYMER_COLUMNS.iter().position(|c| *c == full) else { return Vec::new() };
        self.polymer.iter().map(|r| r[k]).collect()
    }
}

fn v6<S: Scalar>(m: &Mandel<S>) -> [f64; 6] {
    m.map(|x| x.value())
}

impl SolidKernel {
    fn step_data(&self, n: usize) -> (Vec<f64>, Vec<f64>, f64) {
        if n == 0 {
            let (mut a, _) = self.local_data(1);
            let width = self.model.local_width();
            let ls = self.model.local_size();
            for e in 0..self.ne {
                let row = &mut a[e * width..(e + 1) * width];
                for (i, node) in self.mesh.tets[e].iter().enumerate() {
                    row[i] = (self.fixed_t[0][*node] - self.model.t0) / self.model.ts;
                    for c in 0..3 {
                        row[4 + 3 * i + c] = self.fixed_u[0][3 * node + c] / self.model.us;
                    }
                }
                row[ls] = 1.0;
            }
            return (a.clone(), a, 1.0);
        }
        let (a, b) = self.local_data(n);
        (a, b, self.times[n] - self.times[n - 1])
    }

    #[must_use]
    pub fn observe(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> Observed {
        let m = &self.model;
        let (cur_data, prev_data, dt) = self.step_data(n);
        let mut o = Observed::default();
        let h = [x[self.nc] * 1e-3, x[self.nc + 1] * 1e-3, x[self.nc + 2] * 1e-3];
        o.cell_volume = h[0] * h[1] * h[2];
        let pn = n.saturating_sub(1);
        for e in 0..self.ne {
            let cur = self.element_local(n, e, z, &cur_data);
            let prev = self.element_local(pn, e, old, &prev_data);
            let design = self.element_design(e, x);
            let forcing: Vec<f64> = m
                .history
                .as_ref()
                .map(|mh| mh.forcing_row(n, self.mesh.owners[e]).to_vec())
                .unwrap_or_default();
            let (f, ob) = m.observables(&self.mesh.gradients[e], &cur, &prev, &design, dt, &forcing);
            if m.history.is_some() {
                o.material_state.push(f.state[m.layout.material_start()..].to_vec());
            }
            o.material_stored_energy.push(ob.history_energy.stored);
            o.material_evolution_heat.push(ob.history_energy.sensible_heat);
            o.material_external_energy.push(ob.history_energy.external);
            o.conductivity.push(f.prop.get(idx::K));
            o.yield_stress.push(f.prop.get(idx::YIELD));
            o.strain.push(v6(&f.strain));
            o.stress.push(v6(&f.stress));
            o.von_mises.push(mandel::equivalent(&f.stress));
            o.equivalent_plastic.push(ob.equivalent_plastic);
            o.equivalent_creep.push(ob.equivalent_creep);
            let zero = [0.0; 6];
            o.plastic_strain.push(if m.plastic.is_some() {
                mandel::from_slice(&f.state[m.layout.plastic_strain()])
            } else {
                zero
            });
            o.creep_strain.push(if m.creep.is_some() {
                mandel::from_slice(&f.state[m.layout.creep_strain()])
            } else {
                zero
            });
            o.heat_increment.push(ob.heat_increment);
            o.plastic_dissipation.push(ob.plastic_dissipation);
            o.creep_dissipation.push(ob.creep_dissipation);
            o.elastic_energy += ob.elastic_energy;
            o.mass += ob.mass;
            o.material_temperature.push(f.prop.temperature);
            o.backstress.push(v6(&ob.backstress));
            o.yield_residual.push(ob.yield_residual);
            if let Some((r, heat)) = &ob.polymer {
                let mut row: Vec<f64> = r.diagnostics().iter().map(|(_, v)| *v).collect();
                row.push(*heat);
                row.push(ob.stiffness * r.numerical_dissipation_increment);
                o.polymer.push(row);
            }
            if let Some(d) = ob.thermoelastic_defect {
                o.thermoelastic_defect.get_or_insert_with(Vec::new).push(d);
            }
        }
        o.temperature_nodes = self.nodal_temperature(n, z);
        o.displacement_nodes = self.nodal_displacement(n, z);
        let (v, a) = self.kinematic_nodes(z);
        if let Some(d) = &self.dynamics {
            let rho = &x[..self.nc];
            let c = &x[self.nc + 3..];
            for (e, tet) in self.mesh.tets.iter().enumerate() {
                let vel: [[f64; 3]; 4] = std::array::from_fn(|i| v[tet[i]]);
                let owner = self.mesh.owners[e];
                o.kinetic_energy +=
                    crate::structural_inertia::kinetic_energy(m, d, &vel, rho[owner], c[owner], h);
            }
        }
        o.velocity = v;
        o.acceleration = a;
        o.external_force = self.external_force(n, &h);
        o
    }

    fn element_stress_temperature(&self, n: usize, z: &[f64], x: &[f64]) -> (Vec<[f64; 6]>, Vec<f64>) {
        let (cur_data, _, _) = self.step_data(n);
        let mut stress = Vec::with_capacity(self.ne);
        let mut temperature = Vec::with_capacity(self.ne);
        for e in 0..self.ne {
            let cur = self.element_local(n, e, z, &cur_data);
            let f = self.model.fields(&self.mesh.gradients[e], &cur, &self.element_design(e, x));
            stress.push(v6(&f.stress));
            temperature.push(f.prop.temperature);
        }
        (stress, temperature)
    }

    fn external_work<S: Scalar>(&self, n: usize, spacing_mm: &[S; 3], u: &[[f64; 3]]) -> S {
        let h = spacing_mm.map(|v| v * 1e-3);
        let force = self.external_force(n, &h);
        let mut acc = S::zero();
        for (f, d) in force.iter().zip(u) {
            for c in 0..3 {
                acc += f[c] * d[c];
            }
        }
        acc
    }


    #[allow(clippy::too_many_lines)]
    pub fn responses(
        &self,
        states: &[Vec<f64>],
        x: &[f64],
        gradients: bool,
    ) -> Result<ResponseEval, CaeError> {
        let m = &self.model;
        let nsteps = states.len();
        let last = nsteps - 1;
        let nz = self.state_size;
        let nx = x.len();
        let mut gu: Vec<Vec<[f64; 8]>> =
            if gradients { vec![vec![[0.0; 8]; nz]; nsteps] } else { Vec::new() };
        let mut gx = vec![[0.0; 8]; if gradients { nx } else { 0 }];
        let ls = m.local_size();

        let mut heat = 0.0;
        let (mut np_, mut nc_, mut den, mut elastic, mut mass) = (0.0, 0.0, 0.0, 0.0, 0.0);
        let mut dp = LocalGrad::default();
        let mut dcr = LocalGrad::default();
        let mut dden = LocalGrad::default();
        for n in 1..nsteps {
            let (cur_data, prev_data, dt) = self.step_data(n);
            let final_step = n == last;
            for e in 0..self.ne {
                let cur = self.element_local(n, e, &states[n], &cur_data);
                let prev = self.element_local(n - 1, e, &states[n - 1], &prev_data);
                let design = self.element_design(e, x);
                let forcing: Vec<f64> = m
                    .history
                    .as_ref()
                    .map(|mh| mh.forcing_row(n, self.mesh.owners[e]).to_vec())
                    .unwrap_or_default();
                let grad0 = &self.mesh.gradients[e];
                let eval = |input: &[Dual<W>]| -> Vec<Dual<W>> {
                    let mut c: Vec<Dual<W>> = input[..ls].to_vec();
                    c.extend(cur[ls..].iter().map(|v| Dual::constant(*v)));
                    let mut p: Vec<Dual<W>> = input[ls..2 * ls].to_vec();
                    p.extend(prev[ls..].iter().map(|v| Dual::constant(*v)));
                    let d = &input[2 * ls..];
                    let (f, ob) = m.observables(grad0, &c, &p, d, Dual::constant(dt), &forcing);
                    let volume = f.volume;
                    let w = d[0];
                    vec![
                        ob.heat_increment * volume,
                        w * ob.equivalent_plastic,
                        w * ob.equivalent_creep,
                        w,
                        ob.elastic_energy,
                        ob.mass,
                    ]
                };
                let mut input = cur[..ls].to_vec();
                input.extend_from_slice(&prev[..ls]);
                input.extend_from_slice(&design);
                if gradients {
                    let jac = implexity_ad::forward::jacobian::<W, _>(eval, &input)
                        .map_err(|e| CaeError::contract(e.to_string()))?;
                    let value = |k: usize| jac.value[k];
                    heat += value(0);
                    self.scatter(&mut gu, &mut gx, n, e, &jac, 0, 2, 1.0);
                    if final_step {
                        np_ += value(1);
                        nc_ += value(2);
                        den += value(3);
                        elastic += value(4);
                        mass += value(5);
                        dp.push(e, n, jac_row(&jac, 1));
                        dcr.push(e, n, jac_row(&jac, 2));
                        dden.push(e, n, jac_row(&jac, 3));
                        self.scatter(&mut gu, &mut gx, n, e, &jac, 4, 5, 1.0);
                        self.scatter(&mut gu, &mut gx, n, e, &jac, 5, 4, 1.0);
                    }
                } else {
                    let out: Vec<f64> = eval(&input.iter().map(|v| Dual::constant(*v)).collect::<Vec<_>>())
                        .iter()
                        .map(|d| d.re)
                        .collect();
                    heat += out[0];
                    if final_step {
                        np_ += out[1];
                        nc_ += out[2];
                        den += out[3];
                        elastic += out[4];
                        mass += out[5];
                    }
                }
            }
        }
        let plastic = np_ / den;
        let creep = nc_ / den;
        if gradients {
            for (rows, col, value) in [(&dp, 0, plastic), (&dcr, 1, creep)] {
                for (e, n, row) in &rows.rows {
                    self.scatter_row(&mut gu, &mut gx, *n, *e, row, col, 1.0 / den);
                }
                for (e, n, row) in &dden.rows {
                    self.scatter_row(&mut gu, &mut gx, *n, *e, row, col, -value / den);
                }
            }
        }
        let mut peak = f64::NEG_INFINITY;
        let mut arg: Vec<(usize, usize)> = Vec::new();
        for (n, z) in states.iter().enumerate().skip(1) {
            let t = self.nodal_temperature(n, z);
            for (node, v) in t.iter().enumerate() {
                if *v > peak || v.is_nan() {
                    peak = *v;
                    arg.clear();
                    arg.push((n, node));
                } else if v.total_cmp(&peak).is_eq() || (*v == 0.0 && peak == 0.0) {
                    arg.push((n, node));
                }
            }
        }
        if gradients && !arg.is_empty() {
            let share = 1.0 / arg.len() as f64;
            for (n, node) in &arg {
                if let Ok(k) = usize::try_from(self.tmap[*node]) {
                    gu[*n][k][3] += share * m.ts;
                }
            }
        }
        let spacing = [x[self.nc], x[self.nc + 1], x[self.nc + 2]];
        let mut work = vec![0.0; nsteps];
        let mut dwork_dx = vec![[0.0; 3]; nsteps];
        let mut displacement = vec![Vec::new(); nsteps];
        for n in 1..nsteps {
            let u = self.nodal_displacement(n, &states[n]);
            let jac = implexity_ad::forward::jacobian::<3, _>(
                |s: &[Dual<3>]| vec![self.external_work(n, &[s[0], s[1], s[2]], &u)],
                &spacing,
            )
            .map_err(|e| CaeError::contract(e.to_string()))?;
            work[n] = jac.value[0];
            dwork_dx[n] = [jac.get(0, 0), jac.get(0, 1), jac.get(0, 2)];
            displacement[n] = u;
        }
        let (start, end) = self.compliance_window;
        let steps: Vec<usize> =
            (0..nsteps - 1).filter(|i| self.times[*i] >= start && self.times[i + 1] <= end).collect();
        let mut compliance = 0.0;
        let mut weights = vec![0.0; nsteps];
        for i in &steps {
            let dt = self.times[i + 1] - self.times[*i];
            compliance += 0.5 * dt * (work[*i] + work[i + 1]);
            weights[*i] += 0.5 * dt;
            weights[i + 1] += 0.5 * dt;
        }
        let span = end - start;
        compliance /= span;
        if gradients {
            for n in 1..nsteps {
                let wgt = weights[n] / span;
                if wgt == 0.0 {
                    continue;
                }
                let h = [x[self.nc] * 1e-3, x[self.nc + 1] * 1e-3, x[self.nc + 2] * 1e-3];
                let force = self.external_force(n, &h);
                let n_t = self.n_t();
                for (k, dof) in self.free_u.iter().enumerate() {
                    gu[n][n_t + k][6] += wgt * force[dof / 3][dof % 3] * m.us;
                }
                for a in 0..3 {
                    gx[self.nc + a][6] += wgt * dwork_dx[n][a];
                }
            }
        }

        let mut kinetic = 0.0;
        if let Some(d) = &self.dynamics {
            let z = &states[last];
            let (v, _) = self.kinematic_nodes(z);
            let mut vmap = vec![-1_i64; self.nn * 3];
            for (k, dof) in self.free_u.iter().enumerate() {
                vmap[*dof] = i64::try_from(self.velocity.start + k).unwrap_or(-1);
            }
            for (e, tet) in self.mesh.tets.iter().enumerate() {
                let design = self.element_design(e, x);
                let mut input: Vec<f64> = tet.iter().flat_map(|n| v[*n]).collect();
                input.extend_from_slice(&design);
                let dynamics = *d;
                let jac = implexity_ad::forward::jacobian::<W, _>(
                    |s: &[Dual<W>]| {
                        let vel: [[Dual<W>; 3]; 4] =
                            std::array::from_fn(|i| std::array::from_fn(|a| s[3 * i + a]));
                        let q = &s[12..];
                        let h = [q[1] * 1e-3, q[2] * 1e-3, q[3] * 1e-3];
                        vec![crate::structural_inertia::kinetic_energy(m, &dynamics, &vel, q[0], q[4], h)]
                    },
                    &input,
                )
                .map_err(|err| CaeError::contract(err.to_string()))?;
                kinetic += jac.value[0];
                if gradients {
                    for (i, node) in tet.iter().enumerate() {
                        for a in 0..3 {
                            if let Ok(k) = usize::try_from(vmap[3 * node + a]) {
                                gu[last][k][7] += jac.get(0, 3 * i + a) * d.vs;
                            }
                        }
                    }
                    let owner = self.mesh.owners[e];
                    let cols = [owner, self.nc, self.nc + 1, self.nc + 2, self.nc + 3 + owner];
                    for (j, col) in cols.iter().enumerate() {
                        gx[*col][7] += jac.get(0, 12 + j);
                    }
                }
            }
        }
        let values = [plastic, creep, heat, peak, mass, elastic, compliance, kinetic];
        Ok(ResponseEval { values, gu, gx })
    }

    #[allow(clippy::too_many_arguments)]
    fn scatter(
        &self,
        gu: &mut [Vec<[f64; 8]>],
        gx: &mut [[f64; 8]],
        n: usize,
        e: usize,
        jac: &implexity_ad::forward::Jacobian,
        row: usize,
        col: usize,
        factor: f64,
    ) {
        let r = jac_row(jac, row);
        self.scatter_row(gu, gx, n, e, &r, col, factor);
    }

    #[allow(clippy::too_many_arguments)]
    fn scatter_row(
        &self,
        gu: &mut [Vec<[f64; 8]>],
        gx: &mut [[f64; 8]],
        n: usize,
        e: usize,
        row: &[f64],
        col: usize,
        factor: f64,
    ) {
        let ls = self.model.local_size();
        let tet = &self.mesh.tets[e];
        let base = self.n_t() + self.n_u() + e * self.internal_size;
        for (step, offset) in [(n, 0), (n - 1, ls)] {
            for (i, node) in tet.iter().enumerate() {
                if let Ok(k) = usize::try_from(self.tmap[*node]) {
                    gu[step][k][col] += factor * row[offset + i];
                }
                for c in 0..3 {
                    if let Ok(k) = usize::try_from(self.umap[3 * node + c]) {
                        gu[step][k][col] += factor * row[offset + 4 + 3 * i + c];
                    }
                }
            }
            for j in 0..self.internal_size {
                gu[step][base + j][col] += factor * row[offset + 16 + j];
            }
        }
        let owner = self.mesh.owners[e];
        let cols = [owner, self.nc, self.nc + 1, self.nc + 2, self.nc + 3 + owner];
        for (j, c) in cols.iter().enumerate() {
            gx[*c][col] += factor * row[2 * ls + j];
        }
    }


    pub fn response_subvector(
        &self,
        states: &[Vec<f64>],
        x: &[f64],
        names: &[&str],
    ) -> Result<Vec<f64>, CaeError> {
        let missing: Vec<&str> = names.iter().copied().filter(|n| !super::RESPONSES.contains(n)).collect();
        if !missing.is_empty() {
            return contract(format!(
                "unknown solid responses {}",
                implexity_core::pyobj::list_repr(&missing)
            ));
        }
        let full = self.responses(states, x, false)?.values;
        Ok(names.iter().map(|n| full[super::RESPONSES.iter().position(|r| r == n).unwrap_or(0)]).collect())
    }

    #[must_use]
    pub fn energy_ledger(&self, states: &[Vec<f64>], x: &[f64]) -> Value {
        let mut kinetic = vec![0.0];
        let mut elastic = vec![0.0];
        let mut work = vec![0.0];
        let h = [x[self.nc] * 1e-3, x[self.nc + 1] * 1e-3, x[self.nc + 2] * 1e-3];
        let mut prev_f = self.external_force(0, &h);
        let mut prev_u = vec![[0.0; 3]; self.nn];
        for n in 1..states.len() {
            let d = self.observe(n, &states[n], &states[n - 1], x);
            kinetic.push(d.kinetic_energy);
            elastic.push(d.elastic_energy);
            let mut w = 0.0;
            for ((f, pf), (u, pu)) in
                d.external_force.iter().zip(&prev_f).zip(d.displacement_nodes.iter().zip(&prev_u))
            {
                for c in 0..3 {
                    w += 0.5 * (f[c] + pf[c]) * (u[c] - pu[c]);
                }
            }
            work.push(w);
            prev_u = d.displacement_nodes;
            prev_f = d.external_force;
        }
        let energy: Vec<f64> = kinetic.iter().zip(&elastic).map(|(a, b)| a + b).collect();
        let remainder: Vec<f64> = energy.windows(2).zip(&work[1..]).map(|(e, w)| e[1] - e[0] - w).collect();
        let scale = energy
            .iter()
            .map(|v| v.abs())
            .fold(0.0, f64::max)
            .max(work.iter().map(|v| v.abs()).sum())
            .max(f64::MIN_POSITIVE);
        let maximum = if remainder.is_empty() {
            0.0
        } else {
            remainder.iter().map(|v| v.abs()).fold(0.0, f64::max) / scale
        };
        json!({"kinetic_energy_J": kinetic, "elastic_energy_J": elastic, "external_work_increment_J": work,
            "energy_change_minus_work_J": remainder, "maximum_relative_remainder": maximum,
            "interpretation": "zero for linear elastic undamped material; negative = dissipation (damping/inelastic)"})
    }


    pub fn fatigue_diagnostics(&self, states: &[Vec<f64>], x: &[f64]) -> Result<Value, CaeError> {
        let Some(row) = self.p.get("fatigue_observer").filter(|v| !v.is_null()) else {
            return Ok(Value::Null);
        };
        let points: Vec<usize> = (0..self.ne).filter(|e| x[self.mesh.owners[*e]] > 0.0).collect();
        if points.is_empty() {
            return convergence("fatigue observer requires physical solid material points");
        }
        let mut stress = Vec::with_capacity(states.len());
        let mut temperature = Vec::with_capacity(states.len());
        for (n, z) in states.iter().enumerate() {
            let (s, t) = self.element_stress_temperature(n, z, x);
            stress.push(points.iter().map(|e| s[*e]).collect::<Vec<_>>());
            temperature.push(points.iter().map(|e| t[*e]).collect::<Vec<_>>());
        }
        let mut report = crate::fatigue::evaluate(&row["settings"], &stress, &temperature, &self.times)?;
        report["tetrahedron_indices"] = json!(points);
        report["solid_support"] = json!("rho > 0 exactly");
        report["role"] = json!("postprocess_only_not_an_objective_or_stiffness_feedback");
        Ok(report)
    }

    #[must_use]
    pub fn yield_switch_distance(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> Option<Vec<f64>> {
        let m = &self.model;
        let plastic = m.plastic?;
        plastic.switch_distance::<f64>(
            &[0.0; 6],
            &[0.0; 7],
            &[0.0; 7],
            &m.properties(300.0, 0.0, None),
            1.0,
        )?;
        let (cur_data, prev_data, _) = self.step_data(n);
        let pr = m.layout.plastic();
        let e_scale = m.ss / m.es;
        Some(
            (0..self.ne)
                .map(|e| {
                    let cur = self.element_local(n, e, z, &cur_data);
                    let prev = self.element_local(n - 1, e, old, &prev_data);
                    let f = m.fields(&self.mesh.gradients[e], &cur, &self.element_design(e, x));
                    let old_state: Vec<f64> =
                        prev[16..16 + self.internal_size].iter().zip(&m.scales).map(|(v, s)| v * s).collect();
                    plastic
                        .switch_distance(
                            &f.stress,
                            &f.state[pr.clone()],
                            &old_state[pr.clone()],
                            &f.prop,
                            e_scale,
                        )
                        .unwrap_or(f64::NAN)
                })
                .collect(),
        )
    }

    #[must_use]
    pub fn yield_switch_report(&self, states: &[Vec<f64>], x: &[f64]) -> Value {
        let mut minimum = f64::INFINITY;
        let (mut step, mut element): (Option<usize>, Option<usize>) = (None, None);
        let mut points = 0usize;
        for n in 1..states.len() {
            let Some(d) = self.yield_switch_distance(n, &states[n], &states[n - 1], x) else {
                return json!({"declared": false, "minimum_yield_switch_distance": null, "threshold": YIELD_SWITCH_CERTIFICATE,
                    "points_inside_certificate": 0, "sensitivity_admissible": true});
            };
            if d.is_empty() {
                continue;
            }
            if d.iter().any(|v| !v.is_finite()) {
                minimum = f64::NAN;
                step = Some(n);
                element = d.iter().position(|v| !v.is_finite());
                break;
            }
            points += d.iter().filter(|v| **v <= YIELD_SWITCH_CERTIFICATE).count();
            let (arg, min) = d
                .iter()
                .enumerate()
                .fold((0, f64::INFINITY), |(a, m), (i, v)| if *v < m { (i, *v) } else { (a, m) });
            if min < minimum {
                minimum = min;
                step = Some(n);
                element = Some(arg);
            }
        }
        json!({"declared": true, "minimum_yield_switch_distance": crate::util::float(minimum).as_f64().map_or(json!(minimum.to_string()), |v| json!(v)),
            "threshold": YIELD_SWITCH_CERTIFICATE,
            "history_step": step, "tetrahedron": element, "points_inside_certificate": points,
            "sensitivity_admissible": minimum.is_finite() && minimum > YIELD_SWITCH_CERTIFICATE,
            "derivative_scope": "piecewise branch-local; no derivative across the yield-onset active-set switch"})
    }


    pub fn certify_sensitivity(&self, states: &[Vec<f64>], x: &[f64]) -> Result<Value, CaeError> {
        let report = self.yield_switch_report(states, x);
        if report["sensitivity_admissible"] != json!(true) {
            let min = report["minimum_yield_switch_distance"].as_f64().unwrap_or(f64::NAN);
            return contract(format!(
                "J2 yield-onset active-set switch prevents branch-local sensitivity admission: minimum |dg + f/E_scale| = {} <= {} at history_step={}, tetrahedron={} ({} material point(s) inside the certificate)",
                format_e3(min),
                "1e-08",
                implexity_core::pyobj::py_str(&report["history_step"]),
                implexity_core::pyobj::py_str(&report["tetrahedron"]),
                report["points_inside_certificate"],
            ));
        }
        Ok(report)
    }

    #[must_use]
    pub fn temperature_history(&self, states: &[Vec<f64>]) -> Vec<Vec<f64>> {
        states.iter().enumerate().map(|(n, z)| self.nodal_temperature(n, z)).collect()
    }

    #[must_use]
    pub fn element_stress(&self, n: usize, z: &[f64], x: &[f64]) -> Vec<[f64; 6]> {
        self.element_stress_temperature(n, z, x).0
    }
}

#[must_use]
pub fn format_e3(x: f64) -> String {
    if !x.is_finite() {
        return if x.is_nan() {
            "nan".into()
        } else if x > 0.0 {
            "inf".into()
        } else {
            "-inf".into()
        };
    }
    let s = format!("{x:.3e}");

    match s.split_once('e') {
        Some((m, e)) => {
            let (sign, digits) = if let Some(d) = e.strip_prefix('-') { ("-", d) } else { ("+", e) };
            format!("{m}e{sign}{digits:0>2}")
        }
        None => s,
    }
}

#[derive(Default)]
struct LocalGrad {
    rows: Vec<(usize, usize, Vec<f64>)>,
}

impl LocalGrad {
    fn push(&mut self, e: usize, n: usize, row: Vec<f64>) {
        self.rows.push((e, n, row));
    }
}

#[derive(Debug, Clone)]
pub struct ResponseEval {
    pub values: [f64; 8],
    pub gu: Vec<Vec<[f64; 8]>>,
    pub gx: Vec<[f64; 8]>,
}

impl ResponseEval {
    #[must_use]
    pub fn gu_dense(&self, indices: &[usize]) -> Vec<DenseMatrix> {
        self.gu
            .iter()
            .map(|rows| {
                let mut m = DenseMatrix::zeros(rows.len(), indices.len());
                for (i, r) in rows.iter().enumerate() {
                    for (j, k) in indices.iter().enumerate() {
                        m.data[i * indices.len() + j] = r[*k];
                    }
                }
                m
            })
            .collect()
    }

    #[must_use]
    pub fn gx_dense(&self, indices: &[usize]) -> DenseMatrix {
        let mut m = DenseMatrix::zeros(self.gx.len(), indices.len());
        for (i, r) in self.gx.iter().enumerate() {
            for (j, k) in indices.iter().enumerate() {
                m.data[i * indices.len() + j] = r[*k];
            }
        }
        m
    }
}

fn jac_row(jac: &implexity_ad::forward::Jacobian, i: usize) -> Vec<f64> {
    jac.matrix[i * jac.cols..(i + 1) * jac.cols].to_vec()
}
