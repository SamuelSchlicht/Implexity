// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;

use serde_json::{Value, json};

use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;
use implexity_physics_solid::solid_history::HOST_RESPONSES;

use super::RESPONSES;
use super::kernel::UnifiedKernel;
use super::observers::HistorySample;
use crate::liquid_admissibility::ObserverSample;
use crate::rv::{Recording, Rv};
use implexity_physics_base::cyclic_plastic_strain::{CELL_REGION_SAMPLE, PLASTIC_STRAIN_SAMPLE};
use implexity_physics_base::region_temperature_extrema::REGION_FRACTION_SAMPLE;
use implexity_physics_base::wall_film_temperature::{SPEED_SAMPLE, WALL_FLUX_SAMPLE};

fn contract<T>(message: impl Into<String>) -> CaeResult<T> {
    Err(CaeError::contract(message))
}

pub(crate) fn max_ties<S: Scalar>(values: &[S]) -> S {
    let m = values
        .iter()
        .fold(f64::NEG_INFINITY, |a, v| if v.value() > a || v.value().is_nan() { v.value() } else { a });
    let ties = values.iter().filter(|v| v.value() == m).count().max(1);
    let grad: Vec<f64> =
        values.iter().map(|v| if v.value() == m { 1.0 / ties as f64 } else { 0.0 }).collect();
    S::lift(m, values, &grad, &[])
}

#[derive(Debug, Clone)]
struct Cotangent {
    states: Vec<Vec<f64>>,
    design: Vec<f64>,
}

impl UnifiedKernel {
    fn expand_generic<S: Scalar>(&self, n: usize, z: &[S]) -> Vec<S> {
        let mut out: Vec<S> = self.offsets[n].iter().map(|v| S::from_f64(*v)).collect();
        for (r, o) in out.iter_mut().enumerate() {
            let (idx, val) = self.p_map.row(r);
            for (c, v) in idx.iter().zip(val) {
                *o += z[*c] * *v;
            }
        }
        out
    }

    pub(crate) fn nodal_temperature_generic<S: Scalar>(&self, n: usize, full: &[S]) -> Vec<S> {
        let s = &self.s;
        let mut t: Vec<S> = s.fixed_t[n].iter().map(|v| S::from_f64(*v)).collect();
        for (k, node) in s.free_t.iter().enumerate() {
            t[*node] = full[self.solid_slice.start + k] * s.model.ts + s.model.t0;
        }
        t
    }

    fn material_forcing_generic<S: Scalar>(
        &self,
        n: usize,
        full: &[S],
        x: &[S],
    ) -> CaeResult<Option<Vec<Vec<S>>>> {
        let Some(mh) = &self.s.model.history else { return Ok(None) };
        let channels = mh.forcing_shape[2];
        let fv: Vec<f64> = full.iter().map(Scalar::value).collect();
        let xv: Vec<f64> = x.iter().map(Scalar::value).collect();
        for source in &self.sources {
            if let Some(values) = source.source.forcing(n, &fv, &xv)? {
                let (jz, jx) = source
                    .source
                    .forcing_jacobians(n, &fv, &xv)?
                    .ok_or_else(|| CaeError::contract("owned material forcing lacks exact partials"))?;
                let mut rows = Vec::with_capacity(self.nc);
                for c in 0..self.nc {
                    let mut row = Vec::with_capacity(channels);
                    for ch in 0..channels {
                        let i = c * channels + ch;
                        let (zi, zv) = jz.row(i);
                        let (xi, xv2) = jx.row(i);
                        let mut inputs: Vec<S> = zi.iter().map(|j| full[*j]).collect();
                        inputs.extend(xi.iter().map(|j| x[*j]));
                        let mut grad: Vec<f64> = zv.to_vec();
                        grad.extend_from_slice(xv2);
                        row.push(S::lift(values[i], &inputs, &grad, &[]));
                    }
                    rows.push(row);
                }
                return Ok(Some(rows));
            }
        }
        Ok(Some(
            (0..self.nc).map(|c| mh.forcing_row(n, c).iter().map(|v| S::from_f64(*v)).collect()).collect(),
        ))
    }

    fn element_local_generic<S: Scalar>(&self, n: usize, e: usize, full: &[S], data: &[f64]) -> Vec<S> {
        let s = &self.s;
        let m = &s.model;
        let width = m.local_width();
        let mut local: Vec<S> = data[e * width..(e + 1) * width].iter().map(|v| S::from_f64(*v)).collect();
        for (i, node) in s.mesh.tets[e].iter().enumerate() {
            local[i] = match usize::try_from(s.tmap[*node]) {
                Ok(row) => full[self.solid_slice.start + row],
                Err(_) => S::from_f64((s.fixed_t[n][*node] - m.t0) / m.ts),
            };
            for c in 0..3 {
                local[4 + 3 * i + c] = match usize::try_from(s.umap[3 * node + c]) {
                    Ok(row) => full[self.solid_slice.start + row],
                    Err(_) => S::from_f64(s.fixed_u[n][3 * node + c] / m.us),
                };
            }
        }
        let start = self.solid_slice.start + s.n_t() + s.n_u() + e * s.internal_size;
        local[16..16 + s.internal_size].copy_from_slice(&full[start..start + s.internal_size]);
        local
    }

    fn cell_equivalent_plastic_strain<S: Scalar>(&self, n: usize, full: &[S], x: &[S]) -> Vec<S> {
        let s = &self.s;
        let m = &s.model;
        let mut out = vec![S::zero(); self.nc];
        if m.plastic.is_none() {
            return out;
        }
        let (data, _) = s.local_data(n.max(1));
        for e in 0..s.mesh.tets.len() {
            let local = self.element_local_generic(n, e, full, &data);
            let o = s.mesh.owners[e];
            let design = [x[o], x[self.nc], x[self.nc + 1], x[self.nc + 2], x[self.nc + 3 + o]];
            let fe = m.fields(&s.mesh.gradients[e], &local, &design);
            out[o] += fe.state[6] / 6.0;
        }
        out
    }

    fn cell_region_fractions<S: Scalar>(&self, x: &[S]) -> Vec<S> {
        (0..self.nc)
            .flat_map(|c| {
                let e1 = x[c] * x[self.nc + 3 + c];
                [x[c] - e1, e1, -x[c] + 1.0]
            })
            .collect()
    }

    fn nodal_solid_heat_flux<S: Scalar>(&self, n: usize, full: &[S], nodal: &[S], x: &[S]) -> Vec<S> {
        let s = &self.s;
        let m = &s.model;
        let (data, _) = s.local_data(n.max(1));
        let h: [S; 3] = std::array::from_fn(|a| x[self.nc + a] * 1e-3);
        let mut sums = vec![S::zero(); nodal.len()];
        let mut weights = vec![S::zero(); nodal.len()];
        for (e, tet) in s.mesh.tets.iter().enumerate() {
            let local = self.element_local_generic(n, e, full, &data);
            let o = s.mesh.owners[e];
            let design = [x[o], x[self.nc], x[self.nc + 1], x[self.nc + 2], x[self.nc + 3 + o]];
            let fe = m.fields(&s.mesh.gradients[e], &local, &design);
            let k = fe.prop.get(implexity_physics_solid::material::idx::K);
            let mut g2 = S::zero();
            for a in 0..3 {
                let mut g = S::zero();
                for (i, node) in tet.iter().enumerate() {
                    g += nodal[*node] * s.mesh.gradients[e][i][a];
                }
                let g = g / h[a];
                g2 += g * g;
            }
            let q = k * (g2 + 1.0e-12).sqrt();
            for node in tet {
                sums[*node] += q * x[o];
                weights[*node] += x[o];
            }
        }
        sums.iter().zip(&weights).map(|(q, w)| if w.value() > 0.0 { *q / *w } else { S::zero() }).collect()
    }

    fn nodal_fluid_speed<S: Scalar>(&self, full: &[S], x: &[S]) -> Vec<S> {
        let f = &self.f;
        let [nx, ny, nz] = self.grid;
        let shape = [nx + 1, ny + 1, nz + 1];
        let face = |a: usize, ix: [usize; 3]| -> S {
            let id = f.maps[a][crate::incompressible_transport::flat(f.map_shapes[a], ix)];
            usize::try_from(id).map_or(S::zero(), |i| full[self.fluid_slice.start + i] * f.us)
        };
        let nn: usize = shape.iter().product();
        let mut sums = vec![S::zero(); nn];
        let mut weights = vec![S::zero(); nn];
        for i in 0..nx {
            for j in 0..ny {
                for k in 0..nz {
                    let cell = [i, j, k];
                    let c = (i * ny + j) * nz + k;
                    let mut u2 = S::from_f64(1.0e-12);
                    for a in 0..3 {
                        let mut hi = cell;
                        hi[a] += 1;
                        let u = (face(a, cell) + face(a, hi)) * 0.5;
                        u2 += u * u;
                    }
                    let speed = u2.sqrt();
                    let w = -x[c] + 1.0;
                    for (di, dj, dk) in
                        (0..2).flat_map(|a| (0..2).flat_map(move |b| (0..2).map(move |d| (a, b, d))))
                    {
                        let node = ((i + di) * shape[1] + j + dj) * shape[2] + k + dk;
                        sums[node] += speed * w;
                        weights[node] += w;
                    }
                }
            }
        }
        sums.iter().zip(&weights).map(|(q, w)| if w.value() > 0.0 { *q / *w } else { S::zero() }).collect()
    }

    fn needs_sample(&self, name: &str) -> bool {
        self.observers.iter().any(|o| o.required_sample_names.iter().any(|r| r == name))
    }


    pub(crate) fn sample_generic<S: Scalar>(
        &self,
        n: usize,
        z: &[S],
        x: &[S],
    ) -> CaeResult<HistorySample<S>> {
        let s = &self.s;
        let f = &self.f;
        let full = self.expand_generic(n, z);
        let fs = self.fluid_slice.start;
        let pressure: Vec<S> = (0..self.nc).map(|c| full[fs + f.nv + c] * f.ps + f.pref).collect();
        let fluid_temperature: Vec<S> = (0..self.nc).map(|c| full[self.ft + c] * f.ts + f.t0).collect();
        let nodal = self.nodal_temperature_generic(n, &full);
        let upper: Vec<S> = (0..self.nc)
            .map(|c| {
                let values: Vec<S> =
                    (6 * c..6 * c + 6).flat_map(|e| s.mesh.tets[e].iter().map(|node| nodal[*node])).collect();
                max_ties(&values)
            })
            .collect();
        let fluid_fraction: Vec<S> = x[..self.nc].iter().map(|v| -*v + 1.0).collect();
        let material = match &s.model.history {
            None => None,
            Some(mh) => {
                let forcing = self.material_forcing_generic(n, &full, x)?.unwrap_or_default();
                let m = &s.model;
                let (data, _) = s.local_data(n.max(1));
                let mut energy = vec![S::zero(); self.nc];
                let mut k = vec![S::zero(); self.nc];
                let mut y = vec![S::zero(); self.nc];
                for e in 0..s.mesh.tets.len() {
                    let local = self.element_local_generic(n, e, &full, &data);
                    let o = s.mesh.owners[e];
                    let design = [x[o], x[self.nc], x[self.nc + 1], x[self.nc + 2], x[self.nc + 3 + o]];
                    let fe = m.fields(&s.mesh.gradients[e], &local, &design);
                    let state = &fe.state[m.layout.material_start()..s.internal_size];
                    let stored = mh.energy(state, fe.prop.temperature, &forcing[o], design[4]).stored;
                    energy[o] += stored / 6.0;
                    k[o] += fe.prop.get(implexity_physics_solid::material::idx::K) / 6.0;
                    y[o] += fe.prop.get(implexity_physics_solid::material::idx::YIELD) / 6.0;
                }
                let cell_volume = x[self.nc] * x[self.nc + 1] * x[self.nc + 2] * 1e-9;
                let volume: Vec<S> = x[..self.nc].iter().map(|r| *r * cell_volume).collect();
                Some([energy, k, y, volume])
            }
        };
        let nodal_for_flux = nodal.clone();
        Ok(HistorySample {
            pressure,
            fluid_temperature,
            upper,
            fluid_fraction,
            nodal_temperature: nodal,
            material,
            nodal_region_fractions: self.needs_region_fractions().then(|| self.nodal_region_fractions(x)),
            cell_equivalent_plastic_strain: self
                .needs_sample(PLASTIC_STRAIN_SAMPLE)
                .then(|| self.cell_equivalent_plastic_strain(n, &full, x)),
            cell_region_fractions: self
                .needs_sample(CELL_REGION_SAMPLE)
                .then(|| self.cell_region_fractions(x)),
            nodal_solid_heat_flux: self
                .needs_sample(WALL_FLUX_SAMPLE)
                .then(|| self.nodal_solid_heat_flux(n, &full, &nodal_for_flux, x)),
            nodal_fluid_speed: self.needs_sample(SPEED_SAMPLE).then(|| self.nodal_fluid_speed(&full, x)),
            nodal_fluid_temperature: self.film.as_ref().map(|film| film.fluid_nodal_temperature(n, &full)),
        })
    }

    fn needs_region_fractions(&self) -> bool {
        self.needs_sample(REGION_FRACTION_SAMPLE)
    }

    pub(crate) fn nodal_region_fractions<S: Scalar>(&self, x: &[S]) -> Vec<S> {
        let [nx, ny, nz] = self.grid;
        let shape = [nx + 1, ny + 1, nz + 1];
        let nn: usize = shape.iter().product();
        let mut sums = vec![S::zero(); 3 * nn];
        let mut counts = vec![0u32; nn];
        for i in 0..nx {
            for j in 0..ny {
                for k in 0..nz {
                    let c = (i * ny + j) * nz + k;
                    let rho = x[c];
                    let phase = x[self.nc + 3 + c];
                    let e1 = rho * phase;
                    let fr = [rho - e1, e1, -rho + 1.0];
                    for (di, dj, dk) in
                        (0..2).flat_map(|a| (0..2).flat_map(move |b| (0..2).map(move |d| (a, b, d))))
                    {
                        let node = ((i + di) * shape[1] + j + dj) * shape[2] + k + dk;
                        for r in 0..3 {
                            sums[3 * node + r] += fr[r];
                        }
                        counts[node] += 1;
                    }
                }
            }
        }
        let loaded: Vec<(usize, usize)> = self.s.p["heat_fluxes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|row| row["values"].as_array().is_some_and(|v| v.iter().any(|q| q.as_f64() != Some(0.0))))
            .filter_map(|row| {
                let axis = usize::try_from(row["axis"].as_u64()?).ok()?;
                let plane = if row["side"].as_str()? == "hi" { self.grid[axis] } else { 0 };
                Some((axis, plane))
            })
            .collect();
        let mut out = Vec::with_capacity(4 * nn);
        for (node, count) in counts.iter().enumerate() {
            let inv = 1.0 / f64::from((*count).max(1));
            for r in 0..3 {
                out.push(sums[3 * node + r] * inv);
            }
            let ijk = [node / (shape[1] * shape[2]), (node / shape[2]) % shape[1], node % shape[2]];
            let on_loaded_face = loaded.iter().any(|(axis, plane)| ijk[*axis] == *plane);
            out.push(S::from_f64(if on_loaded_face { 1.0 } else { 0.0 }));
        }
        out
    }


    pub fn history_sample(&self, n: usize, z: &[f64], x: &[f64]) -> CaeResult<HistorySample<f64>> {
        self.sample_generic(n, z, x)
    }


    pub fn validate_observers(&self, states: &[Vec<f64>], x: &[f64]) -> CaeResult<Vec<Value>> {
        if self.observers.is_empty() {
            return Ok(Vec::new());
        }
        let samples: Vec<HistorySample<f64>> =
            states.iter().enumerate().map(|(n, z)| self.history_sample(n, z, x)).collect::<CaeResult<_>>()?;
        let mut boundaries = Vec::new();
        for n in 0..self.nt {
            for b in self.f.p["boundaries"].as_array().into_iter().flatten() {
                if b["momentum"] == "pressure" {
                    let t = b["incoming_temperature_K"][n].as_f64().unwrap_or(f64::NAN);
                    boundaries.push(ObserverSample {
                        pressure_absolute_pa: vec![b["pressure_absolute_Pa"][n].as_f64().unwrap_or(f64::NAN)],
                        fluid_temperature_k: vec![t],
                        phase_cell_temperature_upper_k: vec![t],
                        fluid_fraction: vec![1.0],
                    });
                }
            }
        }
        self.observers
            .iter()
            .map(|o| {
                let rows: &[ObserverSample<f64>] =
                    if o.accepts_boundary_samples() { &boundaries } else { &[] };
                o.check(&samples, rows)
            })
            .collect()
    }

    fn solid_states(&self, full: &[Vec<f64>]) -> Vec<Vec<f64>> {
        full.iter().map(|z| z[self.solid_slice.clone()].to_vec()).collect()
    }

    fn base_values(&self, full: &[Vec<f64>], x: &[f64]) -> CaeResult<Vec<f64>> {
        let solid = self.s.responses(&self.solid_states(full), x, false)?;
        let mut out: Vec<f64> = solid.values[..HOST_RESPONSES.len()].to_vec();
        out[3] = self.smooth_peak(full).0;
        out.push(self.p12(full, x).0);
        let (hp, mf) = self.flow_metrics(full, x);
        out.push(hp.0);
        out.push(mf.0);
        let h: f64 = (0..3).map(|a| x[self.nc + a] * 1e-3).product();
        out.push(x[..self.nc].iter().sum::<f64>() * h);
        out.push(self.compliance(full, x).0);
        if out.len() != RESPONSES.len() {
            return contract("unified response vector does not match its published RESPONSES");
        }
        Ok(out)
    }

    fn smooth_peak(&self, full: &[Vec<f64>]) -> (f64, Vec<Vec<f64>>) {
        let t0 = self.s.model.t0;
        let temps: Vec<Vec<f64>> =
            (1..self.nt).map(|n| self.nodal_temperature_generic(n, &full[n])).collect();
        let count = temps.iter().map(Vec::len).sum::<usize>() as f64;
        let m = temps.iter().flatten().map(|t| (t / t0).powi(32)).sum::<f64>() / count;
        let value = t0 * m.powf(1.0 / 32.0);
        let factor = m.powf(1.0 / 32.0 - 1.0) / count;
        let grads =
            temps.iter().map(|row| row.iter().map(|t| factor * (t / t0).powi(31)).collect()).collect();
        (value, grads)
    }

    #[allow(clippy::type_complexity)]
    fn p12(&self, full: &[Vec<f64>], x: &[f64]) -> (f64, Vec<(usize, f64, Vec<f64>, Vec<f64>)>) {
        let f = &self.f;
        let eps: Vec<f64> = x[..self.nc].iter().map(|v| 1.0 - v).collect();
        let sum: f64 = eps.iter().sum();
        let den = sum.max(1e-15);
        let dden = if sum > 1e-15 {
            1.0
        } else if sum == 1e-15 {
            0.5
        } else {
            0.0
        };
        let mut peaks = Vec::new();
        let mut parts = Vec::new();
        for n in 1..self.nt {
            let t: Vec<f64> = (0..self.nc).map(|c| f.t0 + f.ts * full[n][self.ft + c]).collect();
            let r12: Vec<f64> = t.iter().map(|v| (v / f.t0).powi(12)).collect();
            let inner = eps.iter().zip(&r12).map(|(e, r)| e * r).sum::<f64>() / den;
            let peak = f.t0 * inner.powf(1.0 / 12.0);
            let scale = inner.powf(1.0 / 12.0 - 1.0);
            let dt: Vec<f64> = (0..self.nc).map(|c| scale * eps[c] * (t[c] / f.t0).powi(11) / den).collect();
            let dx: Vec<f64> =
                (0..self.nc).map(|c| f.t0 / 12.0 * scale * (inner * dden - r12[c]) / den).collect();
            peaks.push(peak);
            parts.push((n, dt, dx));
        }
        let m = peaks.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let ties = peaks.iter().filter(|p| **p == m).count().max(1) as f64;
        let rows = parts
            .into_iter()
            .zip(&peaks)
            .filter(|(_, p)| **p == m)
            .map(|((n, dt, dx), _)| (n, 1.0 / ties, dt, dx))
            .collect();
        (m, rows)
    }

    fn flow_metrics(&self, full: &[Vec<f64>], x: &[f64]) -> ((f64, f64, f64), (f64, f64, f64)) {
        let f = &self.f;
        let fx = self.fx(x);
        let m = f.metrics(self.nt - 1, &full[self.nt - 1][self.fluid_slice.clone()], &fx);
        let axis = f.flow_axis;
        let h: [f64; 3] = std::array::from_fn(|a| x[self.nc + a] * 1e-3);
        let area = h[0] * h[1] * h[2] / h[axis];
        let dp = self.pressure_drop();
        (
            (m.hydraulic_power, dp * 0.5 * area * f.us, dp * 0.5 * area * f.us),
            (f.rho * m.flow_out, 0.0, f.rho * area * f.us),
        )
    }

    fn pressure_drop(&self) -> f64 {
        let f = &self.f;
        let n = self.nt - 1;
        let get = |side: &str| {
            f.boundary(f.flow_axis, side)
                .and_then(|b| b.raw["pressure_absolute_Pa"][n].as_f64())
                .unwrap_or(f64::NAN)
        };
        get("lo") - get("hi")
    }

    fn compliance(&self, full: &[Vec<f64>], x: &[f64]) -> (f64, Vec<[f64; 3]>, [f64; 3]) {
        let s = &self.s;
        let zs = &full[1][self.solid_slice.clone()];
        let u = s.nodal_displacement(1, zs);
        let h = [x[self.nc] * 1e-3, x[self.nc + 1] * 1e-3, x[self.nc + 2] * 1e-3];
        let force = s.external_force(1, &h);
        let value: f64 = force.iter().zip(&u).map(|(f, v)| f[0] * v[0] + f[1] * v[1] + f[2] * v[2]).sum();
        let hd: [Dual<3>; 3] = std::array::from_fn(|a| Dual::variable(h[a], a));
        let fd = s.external_force(1, &hd);
        let mut dh = [0.0; 3];
        for (fnode, unode) in fd.iter().zip(&u) {
            for c in 0..3 {
                for (a, d) in dh.iter_mut().enumerate() {
                    *d += fnode[c].eps[a] * unode[c];
                }
            }
        }
        (value, force, dh)
    }


    pub fn response_values(&self, states: &[Vec<f64>], x: &[f64]) -> CaeResult<Vec<f64>> {
        let full: Vec<Vec<f64>> =
            states.iter().enumerate().map(|(n, z)| self.expand(n, z)).collect::<CaeResult<_>>()?;
        let mut out = self.base_values(&full, x)?;
        for o in &self.observers {
            let samples: Vec<HistorySample<f64>> = o
                .sampled_states(states.len())
                .into_iter()
                .map(|n| self.history_sample(n, &states[n], x))
                .collect::<CaeResult<_>>()?;
            out.extend(o.values(&samples)?);
        }
        for source in &self.sources {
            out.extend(source.source.responses(&full, x)?);
        }
        Ok(out)
    }

    fn zero_cotangent(&self, x: &[f64]) -> Cotangent {
        Cotangent { states: vec![vec![0.0; self.full_size]; self.nt], design: vec![0.0; x.len()] }
    }


    #[allow(clippy::too_many_lines)]
    pub fn response_partials(
        &self,
        states: &[Vec<f64>],
        x: &[f64],
        names: &[String],
    ) -> CaeResult<(Vec<f64>, Vec<DenseMatrix>, DenseMatrix)> {
        let all_names = self.response_names();
        let idx: Vec<usize> = names
            .iter()
            .map(|n| {
                all_names
                    .iter()
                    .position(|a| a == n)
                    .ok_or_else(|| CaeError::contract("invalid shared-domain response set"))
            })
            .collect::<CaeResult<_>>()?;
        let full: Vec<Vec<f64>> =
            states.iter().enumerate().map(|(n, z)| self.expand(n, z)).collect::<CaeResult<_>>()?;
        let values = self.response_values(states, x)?;
        let m = names.len();
        let mut cot: Vec<Cotangent> = (0..m).map(|_| self.zero_cotangent(x)).collect();

        let mut reduced_extra: Vec<Vec<Vec<f64>>> = vec![Vec::new(); m];
        let s = &self.s;
        let base = RESPONSES.len();
        let observer_offsets: Vec<usize> = self
            .observers
            .iter()
            .scan(base, |acc, o| {
                let start = *acc;
                *acc += o.response_units().len();
                Some(start)
            })
            .collect();
        let source_start = base + self.observers.iter().map(|o| o.response_units().len()).sum::<usize>();
        let needs_solid = idx.iter().any(|i| *i < HOST_RESPONSES.len() && *i != 3);
        let solid = if needs_solid { Some(s.responses(&self.solid_states(&full), x, true)?) } else { None };
        for (j, &i) in idx.iter().enumerate() {
            let c = &mut cot[j];
            match i {
                3 => {
                    let (_, grads) = self.smooth_peak(&full);
                    for (step, row) in grads.iter().enumerate() {
                        let n = step + 1;
                        for (k, node) in s.free_t.iter().enumerate() {
                            c.states[n][self.solid_slice.start + k] += row[*node] * s.model.ts;
                        }
                    }
                }
                i if i < HOST_RESPONSES.len() => {
                    let r = solid
                        .as_ref()
                        .ok_or_else(|| CaeError::contract("solid response partials missing"))?;
                    for (n, rows) in r.gu.iter().enumerate() {
                        for (k, row) in rows.iter().enumerate() {
                            c.states[n][self.solid_slice.start + k] += row[i];
                        }
                    }
                    for (k, row) in r.gx.iter().enumerate() {
                        c.design[k] += row[i];
                    }
                }
                6 => {
                    let (_, rows) = self.p12(&full, x);
                    for (n, w, dt, dx) in rows {
                        for cell in 0..self.nc {
                            c.states[n][self.ft + cell] += w * dt[cell] * self.f.ts;
                            c.design[cell] += w * dx[cell];
                        }
                    }
                }
                7 | 8 => {
                    let ((hp, hlo, hhi), (mf, mlo, mhi)) = self.flow_metrics(&full, x);
                    let (value, lo, hi) = if i == 7 { (hp, hlo, hhi) } else { (mf, mlo, mhi) };
                    let f = &self.f;
                    let axis = f.flow_axis;
                    let shape = f.map_shapes[axis];
                    for ix in crate::incompressible_transport::ndindex(shape) {
                        let id = f.maps[axis][crate::incompressible_transport::flat(shape, ix)];
                        let Ok(id) = usize::try_from(id) else { continue };
                        let row = self.fluid_slice.start + id;
                        if ix[axis] == 0 {
                            c.states[self.nt - 1][row] += lo;
                        }
                        if ix[axis] == shape[axis] - 1 {
                            c.states[self.nt - 1][row] += hi;
                        }
                    }
                    for a in 0..3 {
                        if a != axis {
                            c.design[self.nc + a] += value / x[self.nc + a];
                        }
                    }
                }
                9 => {
                    let h: f64 = (0..3).map(|a| x[self.nc + a] * 1e-3).product();
                    let volume = x[..self.nc].iter().sum::<f64>() * h;
                    for cell in 0..self.nc {
                        c.design[cell] += h;
                    }
                    for a in 0..3 {
                        c.design[self.nc + a] += volume / x[self.nc + a];
                    }
                }
                10 => {
                    let (_, force, dh) = self.compliance(&full, x);
                    let n_t = s.n_t();
                    for (k, dof) in s.free_u.iter().enumerate() {
                        c.states[1][self.solid_slice.start + n_t + k] += force[dof / 3][dof % 3] * s.model.us;
                    }
                    for a in 0..3 {
                        c.design[self.nc + a] += dh[a] * 1e-3;
                    }
                }
                i if i < source_start => {
                    let (o, start) = self
                        .observers
                        .iter()
                        .zip(&observer_offsets)
                        .rev()
                        .find(|(_, start)| i >= **start)
                        .ok_or_else(|| CaeError::contract("observer response index"))?;
                    let (gz, gx) = self.observer_partials(o, i - start, states, x)?;
                    reduced_extra[j] = gz;
                    for (a, b) in c.design.iter_mut().zip(gx) {
                        *a += b;
                    }
                }
                _ => {
                    let mut start = source_start;
                    for source in &self.sources {
                        let count = source.source.response_units().len();
                        if i < start + count {
                            let mut weights = vec![0.0; count];
                            weights[i - start] = 1.0;
                            let (gz, gx) = source.source.response_vjp(&full, x, &weights)?;
                            for (n, row) in gz.iter().enumerate() {
                                for (a, b) in c.states[n].iter_mut().zip(row) {
                                    *a += b;
                                }
                            }
                            for (a, b) in c.design.iter_mut().zip(&gx) {
                                *a += b;
                            }
                            break;
                        }
                        start += count;
                    }
                }
            }
        }
        let mut gu: Vec<DenseMatrix> = (0..self.nt).map(|_| DenseMatrix::zeros(self.state_size, m)).collect();
        let mut gx = DenseMatrix::zeros(x.len(), m);
        for (j, c) in cot.iter().enumerate() {
            for (n, row) in c.states.iter().enumerate() {
                if row.iter().all(|v| *v == 0.0) && reduced_extra[j].is_empty() {
                    continue;
                }
                let mut reduced = self.reduce_cotangent(row)?;
                if let Some(extra) = reduced_extra[j].get(n) {
                    for (a, b) in reduced.iter_mut().zip(extra) {
                        *a += b;
                    }
                }
                for (k, v) in reduced.iter().enumerate() {
                    gu[n].data[k * m + j] = *v;
                }
            }
            for (k, v) in c.design.iter().enumerate() {
                gx.data[k * m + j] = *v;
            }
        }
        Ok((values, gu, gx))
    }

    fn observer_partials(
        &self,
        observer: &super::observers::UnifiedObserver,
        k: usize,
        states: &[Vec<f64>],
        x: &[f64],
    ) -> CaeResult<(Vec<Vec<f64>>, Vec<f64>)> {
        let rec = Recording::start()?;
        let xs = rec.inputs(x);
        let steps = observer.sampled_states(states.len());
        let mut zs: BTreeMap<usize, Vec<Rv>> = BTreeMap::new();
        let mut samples = Vec::with_capacity(steps.len());
        for n in &steps {
            let z = zs.entry(*n).or_insert_with(|| rec.inputs(&states[*n])).clone();
            samples.push(self.sample_generic(*n, &z, &xs)?);
        }
        let values = observer.values(&samples)?;
        let out = values.get(k).copied().ok_or_else(|| CaeError::contract("observer response index"))?;
        let mut gz = vec![vec![0.0; self.state_size]; states.len()];
        for (n, z) in &zs {
            gz[*n] = rec.vjp(&[out], &[1.0], z);
        }
        let gx = rec.vjp(&[out], &[1.0], &xs);
        Ok((gz, gx))
    }


    pub fn observer_fields(
        &self,
        final_state: &[f64],
        x: &[f64],
    ) -> CaeResult<Vec<(BTreeMap<String, Vec<f64>>, serde_json::Map<String, Value>)>> {
        let sample = self.history_sample(self.nt - 1, final_state, x)?;
        self.observers.iter().map(|o| Ok((o.fields(&sample)?, o.field_metadata()))).collect()
    }


    pub fn solid_enthalpy(&self, n: usize, zs: &[f64], x: &[f64]) -> CaeResult<f64> {
        let s = &self.s;
        let m = &s.model;
        if m.law.reversible_thermoelastic() {
            return contract(
                "Helmholtz solid requires its internal-energy/entropy ledger, not sensible enthalpy",
            );
        }
        let t = s.nodal_temperature(n, zs);
        let h = [x[self.nc] * 1e-3, x[self.nc + 1] * 1e-3, x[self.nc + 2] * 1e-3];
        let volume = h[0] * h[1] * h[2] / 6.0;
        let [a, b] = &m.materials;
        let mut total = 0.0;
        for (e, tet) in s.mesh.tets.iter().enumerate() {
            let o = s.mesh.owners[e];
            let c = x[self.nc + 3 + o];
            let mut sum = 0.0;
            for node in tet {
                let (ha, hb) = if m.numerical {
                    (
                        m.law.numerical_sensible_enthalpy(a, t[*node]),
                        m.law.numerical_sensible_enthalpy(b, t[*node]),
                    )
                } else {
                    (m.law.sensible_enthalpy(a, t[*node])?, m.law.sensible_enthalpy(b, t[*node])?)
                };
                sum += (1.0 - c) * a.density() * ha + c * b.density() * hb;
            }
            total += x[o] * volume / 4.0 * sum;
        }
        Ok(total)
    }

    #[must_use]
    pub fn solid_step_ledger(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> Option<Value> {
        let s = &self.s;
        let m = &s.model;
        if !m.law.reversible_thermoelastic() {
            return None;
        }
        let cur = s.fields(n, z, x);
        let prev = s.fields(n - 1, old, x);
        let t = s.nodal_temperature(n, z);
        let tp = s.nodal_temperature(n - 1, old);
        let h = [x[self.nc] * 1e-3, x[self.nc + 1] * 1e-3, x[self.nc + 2] * 1e-3];
        let volume = h[0] * h[1] * h[2] / 6.0;
        let mut totals: BTreeMap<String, f64> = BTreeMap::new();
        for (e, tet) in s.mesh.tets.iter().enumerate() {
            let o = s.mesh.owners[e];
            let density = x[o];
            let stiffness = m.stiffness(density);
            let te: [f64; 4] = std::array::from_fn(|i| t[tet[i]]);
            let tpe: [f64; 4] = std::array::from_fn(|i| tp[tet[i]]);
            let terms = implexity_physics_solid::material::MaterialLaw::step_energy(
                &m.materials[0],
                &m.materials[1],
                x[self.nc + 3 + o],
                density,
                stiffness,
                &te,
                &tpe,
                &cur[e].strain,
                &prev[e].strain,
            );
            for (key, value) in terms {
                *totals.entry(key.trim_end_matches("_m3").to_string()).or_insert(0.0) +=
                    value.iter().sum::<f64>() * volume / 4.0;
            }
        }
        Some(json!(totals))
    }
}
