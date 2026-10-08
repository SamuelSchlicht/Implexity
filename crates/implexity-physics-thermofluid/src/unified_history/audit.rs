// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Map, Value, json};

use implexity_core::CaeResult;
use implexity_core::contracts::FieldValue;

use super::kernel::UnifiedKernel;
use crate::incompressible_transport::flat;

pub const SCHEMA: &str = "implexity-unified-history-field-audit/1";
pub const REPORTING_THRESHOLD: f64 = 0.5;
pub const MECHANICS_REGIONS: [&str; 5] =
    ["solid", "endmember_0", "endmember_1", "endmember_0_bond_line", "endmember_1_bond_line"];
pub const CELL_REGIONS: [&str; 5] = ["all", "solid", "endmember_0", "endmember_1", "fluid"];

struct SourceCellHistory {
    source: String,
    name: String,
    units: Value,
    width: usize,
    data: Vec<f64>,
}

fn node_id(shape: [usize; 3], i: usize, j: usize, k: usize) -> usize {
    (i * shape[1] + j) * shape[2] + k
}

#[derive(Debug, Clone, Copy)]
struct Extent {
    max: f64,
    sum: f64,
    count: usize,
}

impl Extent {
    const fn new() -> Self {
        Self { max: f64::NEG_INFINITY, sum: 0.0, count: 0 }
    }

    fn add(&mut self, v: f64) {
        self.max = self.max.max(v);
        self.sum += v;
        self.count += 1;
    }

    fn json(&self) -> Value {
        if self.count == 0 {
            return json!({"count": 0, "max_K": Value::Null, "mean_K": Value::Null});
        }
        #[allow(clippy::cast_precision_loss)]
        let mean = self.sum / self.count as f64;
        json!({"count": self.count, "max_K": self.max, "mean_K": mean})
    }

    fn mean(&self) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let n = self.count as f64;
        if self.count == 0 { f64::NAN } else { self.sum / n }
    }
}

struct Classes {
    fluid_cell: Vec<bool>,
    solid_cell: Vec<bool>,
    near_wall_cell: Vec<bool>,
    region_node: [Vec<bool>; 3],
    support_node: [Vec<bool>; 3],
    bond_node: Vec<bool>,
    wall_node: Vec<bool>,
    wall_node_by_endmember: [Vec<bool>; 2],
    wetted_area_m2: f64,
    fluid_volume_m3: f64,
}

impl UnifiedKernel {
    fn audit_classes(&self, x: &[f64]) -> Classes {
        let [nx, ny, nz] = self.grid;
        let nc = self.nc;
        let rho = &x[..nc];
        let phase = &x[nc + 3..nc + 3 + nc];
        let h: [f64; 3] = std::array::from_fn(|a| x[nc + a] * 1e-3);
        let cell_fraction =
            |c: usize| -> [f64; 3] { [rho[c] * (1.0 - phase[c]), rho[c] * phase[c], 1.0 - rho[c]] };
        let shape = [nx + 1, ny + 1, nz + 1];
        let nn = shape.iter().product::<usize>();
        let mut sums = vec![[0.0f64; 3]; nn];
        let mut counts = vec![0usize; nn];
        let mut adj_dominant = vec![[false; 4]; nn];
        let fluid_cell: Vec<bool> = rho.iter().map(|r| 1.0 - r >= REPORTING_THRESHOLD).collect();
        let solid_cell: Vec<bool> = rho.iter().map(|r| *r >= REPORTING_THRESHOLD).collect();
        for i in 0..nx {
            for j in 0..ny {
                for k in 0..nz {
                    let c = (i * ny + j) * nz + k;
                    let fr = cell_fraction(c);
                    let dominant = [
                        fr[0] >= REPORTING_THRESHOLD,
                        fr[1] >= REPORTING_THRESHOLD,
                        fluid_cell[c],
                        solid_cell[c],
                    ];
                    for (di, dj, dk) in
                        (0..2).flat_map(|a| (0..2).flat_map(move |b| (0..2).map(move |d| (a, b, d))))
                    {
                        let node = node_id(shape, i + di, j + dj, k + dk);
                        for r in 0..3 {
                            sums[node][r] += fr[r];
                        }
                        counts[node] += 1;
                        for (flag, d) in adj_dominant[node].iter_mut().zip(dominant) {
                            *flag |= d;
                        }
                    }
                }
            }
        }
        #[allow(clippy::cast_precision_loss)]
        let nodal: Vec<[f64; 3]> =
            sums.iter().zip(&counts).map(|(s, c)| s.map(|v| v / (*c).max(1) as f64)).collect();
        let region_node: [Vec<bool>; 3] =
            std::array::from_fn(|r| nodal.iter().map(|f| f[r] >= REPORTING_THRESHOLD).collect());
        let support_node: [Vec<bool>; 3] =
            std::array::from_fn(|r| nodal.iter().map(|f| f[r] > 0.0).collect());
        let bond_node: Vec<bool> = adj_dominant.iter().map(|d| d[0] && d[1]).collect();
        let wall_node: Vec<bool> = adj_dominant.iter().map(|d| d[2] && d[3]).collect();
        let wall_node_by_endmember: [Vec<bool>; 2] =
            std::array::from_fn(|r| adj_dominant.iter().map(|d| d[2] && d[r]).collect());
        let mut near_wall_cell = vec![false; nc];
        let mut wetted_area_m2 = 0.0;
        for i in 0..nx {
            for j in 0..ny {
                for k in 0..nz {
                    let c = (i * ny + j) * nz + k;
                    for (axis, (ii, jj, kk)) in
                        [(i + 1, j, k), (i, j + 1, k), (i, j, k + 1)].into_iter().enumerate()
                    {
                        if ii >= nx || jj >= ny || kk >= nz {
                            continue;
                        }
                        let d = (ii * ny + jj) * nz + kk;
                        let area = h[0] * h[1] * h[2] / h[axis];
                        wetted_area_m2 += (rho[c] - rho[d]).abs() * area;
                        if fluid_cell[c] && solid_cell[d] {
                            near_wall_cell[c] = true;
                        }
                        if fluid_cell[d] && solid_cell[c] {
                            near_wall_cell[d] = true;
                        }
                    }
                }
            }
        }
        let fluid_volume_m3 = rho.iter().map(|r| 1.0 - r).sum::<f64>() * h[0] * h[1] * h[2];
        Classes {
            fluid_cell,
            solid_cell,
            near_wall_cell,
            region_node,
            support_node,
            bond_node,
            wall_node,
            wall_node_by_endmember,
            wetted_area_m2,
            fluid_volume_m3,
        }
    }

    fn face_nodes(&self, axis: usize, hi: bool) -> Vec<usize> {
        let shape = [self.grid[0] + 1, self.grid[1] + 1, self.grid[2] + 1];
        let plane = if hi { self.grid[axis] } else { 0 };
        let mut out = Vec::new();
        for i in 0..shape[0] {
            for j in 0..shape[1] {
                for k in 0..shape[2] {
                    if [i, j, k][axis] == plane {
                        out.push(node_id(shape, i, j, k));
                    }
                }
            }
        }
        out
    }

    fn opening_geometry(&self, raw: &Value, axis: usize, x: &[f64]) -> (f64, f64) {
        let extent: [f64; 3] = std::array::from_fn(|a| {
            #[allow(clippy::cast_precision_loss)]
            let n = self.grid[a] as f64;
            n * x[self.nc + a] * 1e-3
        });
        let axes: Vec<usize> = (0..3).filter(|a| *a != axis).collect();
        let fraction = |key: &str, i: usize, default: f64| raw["opening"][key][i].as_f64().unwrap_or(default);
        let sides: Vec<f64> = axes
            .iter()
            .enumerate()
            .map(|(i, a)| {
                (fraction("upper_fraction", i, 1.0) - fraction("lower_fraction", i, 0.0)) * extent[*a]
            })
            .collect();
        let area = sides[0] * sides[1];
        (area, 4.0 * area / (2.0 * (sides[0] + sides[1])))
    }

    fn cell_regions(&self, x: &[f64]) -> Vec<[bool; 5]> {
        let nc = self.nc;
        let rho = &x[..nc];
        let phase = &x[nc + 3..nc + 3 + nc];
        (0..nc)
            .map(|c| {
                [
                    true,
                    rho[c] >= REPORTING_THRESHOLD,
                    rho[c] * (1.0 - phase[c]) >= REPORTING_THRESHOLD,
                    rho[c] * phase[c] >= REPORTING_THRESHOLD,
                    1.0 - rho[c] >= REPORTING_THRESHOLD,
                ]
            })
            .collect()
    }

    fn tetrahedron_regions(&self, cells: &[[bool; 5]], classes: &Classes) -> Vec<[bool; 5]> {
        let mesh = &self.s.mesh;
        mesh.tets
            .iter()
            .zip(&mesh.owners)
            .map(|(tet, owner)| {
                let c = cells[*owner];
                let bond = tet.iter().any(|node| classes.bond_node[*node]);
                [c[1], c[2], c[3], c[2] && bond, c[3] && bond]
            })
            .collect()
    }

    fn cell_ijk(&self, c: usize) -> [usize; 3] {
        let [_, ny, nz] = self.grid;
        [c / (ny * nz), (c / nz) % ny, c % nz]
    }

    fn mechanics_audit(
        &self,
        n: usize,
        full: &[f64],
        prev: &[f64],
        x: &[f64],
        regions: &[[bool; 5]],
    ) -> Value {
        let sl = self.solid_slice.clone();
        if sl.is_empty() {
            return Value::Null;
        }
        let o = self.s.observe(n, &full[sl.clone()], &prev[sl], x);
        let owners = &self.s.mesh.owners;
        let mut out = Map::new();
        for (r, name) in MECHANICS_REGIONS.iter().enumerate() {
            let mut count = 0usize;
            let mut vm_at: Option<usize> = None;
            let mut util_at: Option<(usize, f64)> = None;
            let mut margin = f64::INFINITY;
            let (mut plastic, mut creep) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
            let mut cell_sum = vec![(0.0_f64, 0_usize); self.nc];
            for (e, member) in regions.iter().enumerate() {
                if !member[r] {
                    continue;
                }
                count += 1;
                let vm = o.von_mises[e];
                if vm_at.is_none_or(|b| vm > o.von_mises[b]) {
                    vm_at = Some(e);
                }
                let sy = o.yield_stress[e];
                if sy > 0.0 {
                    let u = vm / sy;
                    if util_at.is_none_or(|(_, b)| u > b) {
                        util_at = Some((e, u));
                    }
                    margin = margin.min(sy - vm);
                }
                plastic = plastic.max(o.equivalent_plastic[e]);
                creep = creep.max(o.equivalent_creep[e]);
                let acc = &mut cell_sum[owners[e]];
                acc.0 += vm;
                acc.1 += 1;
            }
            #[allow(clippy::cast_precision_loss)]
            let cell_mean = cell_sum
                .iter()
                .filter(|(_, k)| *k > 0)
                .map(|(v, k)| v / *k as f64)
                .fold(f64::NEG_INFINITY, f64::max);
            let finite = |v: f64| if v.is_finite() { json!(v) } else { Value::Null };
            let at = |e: usize| {
                json!({"tetrahedron": e, "cell": owners[e], "cell_ijk": self.cell_ijk(owners[e]),
                       "von_mises_Pa": o.von_mises[e], "yield_stress_Pa": o.yield_stress[e],
                       "temperature_K": o.material_temperature[e]})
            };
            out.insert(
                (*name).to_string(),
                json!({
                    "tetrahedra": count,
                    "von_mises_max_Pa": vm_at.map(|e| json!(o.von_mises[e])),
                    "von_mises_max_at": vm_at.map(at),
                    "von_mises_cell_mean_max_Pa": finite(cell_mean),
                    "utilisation_max": util_at.map(|(_, u)| json!(u)),
                    "utilisation_max_at": util_at.map(|(e, _)| at(e)),
                    "yield_margin_min_Pa": finite(margin),
                    "equivalent_plastic_strain_max": finite(plastic),
                    "equivalent_creep_strain_max": finite(creep),
                }),
            );
        }
        json!({
            "regions": out,
            "semantics": "literal_tetrahedron_values; utilisation = von_Mises / yield_stress(T) of the tetrahedron's material; regions of the owning cell at the reporting threshold, bond line = tetrahedra with a node adjacent to both endmembers",
        })
    }

    fn source_cell_histories(&self, fulls: &[Vec<f64>], x: &[f64]) -> CaeResult<Vec<SourceCellHistory>> {
        let mut out = Vec::new();
        for source in &self.sources {
            let (values, meta) = source.source.fields(fulls, x)?;
            for (name, value) in values {
                let FieldValue::Array(a) = value else { continue };
                let axes: Vec<&str> = meta
                    .get(&name)
                    .and_then(|m| m["axes"].as_array())
                    .map(|v| v.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                let shape = a.shape().to_vec();
                let cell_history =
                    matches!(axes.as_slice(), ["time", "cell"] | ["time", "cell", "component"])
                        && shape.len() == axes.len()
                        && shape[0] == fulls.len()
                        && shape[1] == self.nc;
                if !cell_history {
                    continue;
                }
                let units = meta.get(&name).map_or(Value::Null, |m| m["units"].clone());
                out.push(SourceCellHistory {
                    source: source.component.clone(),
                    width: shape.get(2).copied().unwrap_or(1),
                    name,
                    units,
                    data: a.iter().copied().collect(),
                });
            }
        }
        Ok(out)
    }

    fn source_audit(&self, n: usize, fields: &[SourceCellHistory], cells: &[[bool; 5]]) -> Value {
        let nc = self.nc;
        let rows: Vec<Value> = fields
            .iter()
            .map(|field| {
                let w = field.width;
                let block = &field.data[n * nc * w..(n + 1) * nc * w];
                let magnitude =
                    |c: usize| block[c * w..(c + 1) * w].iter().map(|v| v * v).sum::<f64>().sqrt();
                let mut regions = Map::new();
                for (r, name) in CELL_REGIONS.iter().enumerate() {
                    let mut best: Option<(usize, f64)> = None;
                    let (mut sum, mut count) = (0.0, 0usize);
                    for (c, member) in cells.iter().enumerate() {
                        if !member[r] {
                            continue;
                        }
                        let m = magnitude(c);
                        sum += m;
                        count += 1;
                        if best.is_none_or(|(_, b)| m > b) {
                            best = Some((c, m));
                        }
                    }
                    #[allow(clippy::cast_precision_loss)]
                    let mean = if count > 0 { json!(sum / count as f64) } else { Value::Null };
                    regions.insert(
                        (*name).to_string(),
                        json!({
                            "cells": count,
                            "magnitude_max": best.map(|(_, m)| json!(m)),
                            "magnitude_max_cell": best.map(|(c, _)| json!(c)),
                            "magnitude_max_cell_ijk": best.map(|(c, _)| json!(self.cell_ijk(c))),
                            "components_at_max": best.map(|(c, _)| json!(block[c * w..(c + 1) * w])),
                            "magnitude_mean": mean,
                        }),
                    );
                }
                json!({"source": field.source, "field": field.name, "units": field.units, "components": w,
                       "regions": regions})
            })
            .collect();
        Value::Array(rows)
    }


    #[allow(clippy::too_many_lines)]
    pub fn field_audit(&self, states: &[Vec<f64>], x: &[f64]) -> CaeResult<Value> {
        let f = &self.f;
        let s = &self.s;
        let nc = self.nc;
        let classes = self.audit_classes(x);
        let h: [f64; 3] = std::array::from_fn(|a| x[nc + a] * 1e-3);
        let volume = h[0] * h[1] * h[2];
        let axis = f.flow_axis;
        #[allow(clippy::cast_precision_loss)]
        let flow_length_m = self.grid[axis] as f64 * h[axis];
        let mean_cross_section_m2 = classes.fluid_volume_m3 / flow_length_m;
        let hydraulic_diameter_m =
            4.0 * classes.fluid_volume_m3 / classes.wetted_area_m2.max(f64::MIN_POSITIVE);
        let fx = self.fx(x);
        let heat_faces: Vec<(usize, bool, Vec<f64>)> = s.p["heat_fluxes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| {
                let a = usize::try_from(row["axis"].as_u64()?).ok()?;
                let hi = row["side"].as_str()? == "hi";
                let values: Vec<f64> =
                    row["values"].as_array()?.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect();
                Some((a, hi, values))
            })
            .collect();
        let opening_rows: Vec<(String, bool, f64, f64)> = ["lo", "hi"]
            .iter()
            .filter_map(|side| {
                let b = f.boundary(axis, side)?;
                b.pressure.then(|| {
                    let (area, dh) = self.opening_geometry(&b.raw, axis, x);
                    ((*side).to_string(), *side == "hi", area, dh)
                })
            })
            .collect();
        let region_names = ["endmember_0", "endmember_1", "fluid"];
        let fulls: Vec<Vec<f64>> =
            states.iter().enumerate().map(|(n, z)| self.expand(n, z)).collect::<CaeResult<_>>()?;
        let cell_regions = self.cell_regions(x);
        let tet_regions = self.tetrahedron_regions(&cell_regions, &classes);
        let source_fields = self.source_cell_histories(&fulls, x)?;
        let mut rows = Vec::new();
        for n in 1..fulls.len() {
            let full = fulls[n].as_slice();
            let nodal = self.nodal_temperature_generic(n, full);

            let coolant: Vec<f64> = self
                .film
                .as_ref()
                .map_or_else(|| nodal.clone(), |film| film.fluid_nodal_temperature(n, full));
            let mut temperature = Map::new();
            let mut all = Extent::new();
            for t in &nodal {
                all.add(*t);
            }
            temperature.insert("all_nodes".into(), all.json());
            for (r, name) in region_names.iter().enumerate() {
                let (mut dominated, mut support) = (Extent::new(), Extent::new());
                let field = if *name == "fluid" { &coolant } else { &nodal };
                for (node, t) in field.iter().enumerate() {
                    if classes.region_node[r][node] {
                        dominated.add(*t);
                    }
                    if classes.support_node[r][node] {
                        support.add(*t);
                    }
                }
                temperature.insert(
                    (*name).to_string(),
                    json!({"reporting_threshold": dominated.json(), "positive_support": support.json()}),
                );
            }
            let mut bond = Extent::new();
            let mut wall = Extent::new();
            let mut wall_em: [Extent; 2] = [Extent::new(), Extent::new()];
            for (node, t) in nodal.iter().enumerate() {
                if classes.bond_node[node] {
                    bond.add(*t);
                }
                if classes.wall_node[node] {
                    wall.add(*t);
                }
                for r in 0..2 {
                    if classes.wall_node_by_endmember[r][node] {
                        wall_em[r].add(*t);
                    }
                }
            }
            temperature.insert("endmember_bond_nodes".into(), bond.json());
            temperature.insert(
                "wetted_wall_nodes".into(),
                json!({"all": wall.json(), "endmember_0": wall_em[0].json(), "endmember_1": wall_em[1].json()}),
            );
            let loaded: Vec<Value> = heat_faces
                .iter()
                .map(|(a, hi, values)| {
                    let mut e = Extent::new();
                    for node in self.face_nodes(*a, *hi) {
                        e.add(nodal[node]);
                    }
                    json!({"axis": a, "side": if *hi { "hi" } else { "lo" },
                           "heat_flux_W_m2": values.get(n).copied().unwrap_or(0.0), "nodes": e.json()})
                })
                .collect();
            temperature.insert("heat_flux_faces".into(), Value::Array(loaded));
            let cells: Vec<f64> = (0..nc).map(|c| full[self.ft + c] * f.ts + f.t0).collect();
            let (mut fluid_cells, mut near_wall) = (Extent::new(), Extent::new());
            for c in 0..nc {
                if classes.fluid_cell[c] {
                    fluid_cells.add(cells[c]);
                }
                if classes.near_wall_cell[c] {
                    near_wall.add(cells[c]);
                }
            }
            temperature.insert("fluid_cells".into(), fluid_cells.json());
            temperature.insert("near_wall_fluid_cells".into(), near_wall.json());

            let fields = f.fields(&full[self.fluid_slice.clone()]);
            let shape = f.map_shapes[axis];
            let face_area = volume / h[axis];
            let mut openings = Vec::new();
            let mut bulk = Vec::new();
            let (mut q_lo, mut q_hi) = (0.0, 0.0);
            for (side, hi, area, dh) in &opening_rows {
                let plane = if *hi { shape[axis] - 1 } else { 0 };
                let (mut q, mut qt, mut qa) = (0.0, 0.0, 0.0);
                for i in 0..shape[0] {
                    for j in 0..shape[1] {
                        for k in 0..shape[2] {
                            let ix = [i, j, k];
                            if ix[axis] != plane {
                                continue;
                            }
                            let u = fields.faces[axis][flat(shape, ix)];
                            if u == 0.0 {
                                continue;
                            }
                            let mut cell = ix;
                            if *hi {
                                cell[axis] -= 1;
                            }
                            let c = flat(self.grid, cell);
                            q += u * face_area;
                            qt += u.abs() * face_area * cells[c];
                            qa += u.abs() * face_area;
                        }
                    }
                }
                if *hi {
                    q_hi = q;
                } else {
                    q_lo = q;
                }
                let tb = if qa > 0.0 { qt / qa } else { f64::NAN };
                bulk.push(tb);
                let mu = f.law.properties(if tb.is_finite() { tb } else { f.t0 }).0;
                let velocity = q.abs() / area;
                openings.push(json!({"side": side, "area_m2": area, "hydraulic_diameter_m": dh,
                    "volumetric_flow_m3_s": q, "mean_velocity_m_s": velocity,
                    "reynolds": f.rho * velocity * dh / mu, "flow_weighted_bulk_temperature_K": tb,
                    "dynamic_viscosity_Pa_s": mu}));
            }
            let mut max_speed: f64 = 0.0;
            let mut max_cell_re: f64 = 0.0;
            let hmax = h.iter().copied().fold(0.0, f64::max);
            for c in 0..nc {
                if !classes.fluid_cell[c] {
                    continue;
                }
                let u = fields.velocity[c];
                let speed = (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt();
                max_speed = max_speed.max(speed);
                let mu = f.law.properties(cells[c]).0;
                max_cell_re = max_cell_re.max(f.rho * speed * hmax / mu);
            }
            let dp = ["lo", "hi"]
                .map(|side| f.boundary(axis, side).and_then(|b| b.raw["pressure_absolute_Pa"][n].as_f64()));
            let pressure_drop = match dp {
                [Some(a), Some(b)] => a - b,
                _ => f64::NAN,
            };
            let q_mean = 0.5 * (q_lo.abs() + q_hi.abs());
            let t_mean_bulk = bulk.iter().copied().filter(|t| t.is_finite()).sum::<f64>()
                / bulk.iter().filter(|t| t.is_finite()).count().max(1) as f64;
            let mu_bulk = f.law.properties(if t_mean_bulk.is_finite() { t_mean_bulk } else { f.t0 }).0;
            let interstitial = q_mean / mean_cross_section_m2.max(f64::MIN_POSITIVE);
            let enthalpy: Vec<(String, f64)> = match &self.nodal_transport {
                Some(t) => t.boundary_energy(n, full, x)?,
                None => f.boundary_energy(n, &full[self.fluid_slice.clone()], &fx),
            };
            let outward: f64 = enthalpy.iter().map(|(_, v)| v).sum();
            let heat_input: f64 = heat_faces
                .iter()
                .map(|(a, _, values)| {
                    #[allow(clippy::cast_precision_loss)]
                    let area: f64 = (0..3).filter(|b| b != a).map(|b| self.grid[b] as f64 * h[b]).product();
                    values.get(n).copied().unwrap_or(0.0) * area
                })
                .sum();
            let wall_mean = wall.mean();
            let htc = outward / (classes.wetted_area_m2 * (wall_mean - t_mean_bulk));
            let film = match &self.film {
                Some(film) => {
                    let mut row = film.ledger(n, full, x)?;
                    let mut wall_coolant = Extent::new();
                    for (node, t) in coolant.iter().enumerate() {
                        if classes.wall_node[node] {
                            wall_coolant.add(*t);
                        }
                    }
                    row["wetted_wall_coolant_nodes"] = wall_coolant.json();
                    row
                }
                None => Value::Null,
            };
            rows.push(json!({
                "step": n,
                "time_s": s.times[n],
                "temperature": temperature,
                "wall_film": film,
                "flow": {
                    "axis": axis,
                    "inlet_volumetric_flow_m3_s": q_lo,
                    "outlet_volumetric_flow_m3_s": q_hi,
                    "mass_flow_kg_s": f.rho * q_mean,
                    "pressure_difference_Pa": pressure_drop,
                    "openings": openings,
                    "max_fluid_cell_speed_m_s": max_speed,
                    "max_fluid_cell_reynolds": max_cell_re,
                    "mean_interstitial_speed_m_s": interstitial,
                    "channel_reynolds": f.rho * interstitial * hydraulic_diameter_m / mu_bulk,
                },
                "mechanics": self.mechanics_audit(n, full, &fulls[n - 1], x, &tet_regions),
                "field_sources": self.source_audit(n, &source_fields, &cell_regions),
                "energy": {
                    "authored_surface_heat_input_W": heat_input,
                    "fluid_boundary_net_outward_enthalpy_W": outward,
                    "fluid_boundary_enthalpy_by_face_W": enthalpy.iter().map(|(k, v)| json!({"face": k, "outward_W": v})).collect::<Vec<_>>(),
                    "mean_wall_heat_flux_W_m2": outward / classes.wetted_area_m2,
                    "mean_wetted_wall_temperature_K": wall_mean,
                    "mean_bulk_temperature_K": t_mean_bulk,
                    "implied_wall_heat_transfer_coefficient_W_m2K": htc,
                },
            }));
        }
        #[allow(clippy::cast_precision_loss)]
        let solid_cells = classes.solid_cell.iter().filter(|b| **b).count() as f64;
        Ok(json!({
            "schema": SCHEMA,
            "reporting_threshold": REPORTING_THRESHOLD,
            "semantics": "diagnostic_literal_values_no_derivative_not_engineering_acceptance",
            "geometry": {
                "cell_spacing_m": h,
                "solid_dominated_cells": solid_cells,
                "fluid_volume_m3": classes.fluid_volume_m3,
                "wetted_area_total_variation_m2": classes.wetted_area_m2,
                "channel_hydraulic_diameter_m": hydraulic_diameter_m,
                "flow_length_m": flow_length_m,
                "mean_fluid_cross_section_m2": mean_cross_section_m2,
            },
            "states": rows,
        }))
    }


    pub fn field_snapshot(&self, states: &[Vec<f64>], x: &[f64], n: usize) -> CaeResult<Value> {
        let z = states.get(n).ok_or_else(|| {
            implexity_core::CaeError::contract(format!(
                "state {n} outside the {} stored states",
                states.len()
            ))
        })?;
        let f = &self.f;
        let nc = self.nc;
        let full = self.expand(n, z)?;
        let nodal = self.nodal_temperature_generic(n, &full);
        let coolant: Vec<f64> =
            self.film.as_ref().map_or_else(|| nodal.clone(), |film| film.fluid_nodal_temperature(n, &full));
        let cells: Vec<f64> = (0..nc).map(|c| full[self.ft + c] * f.ts + f.t0).collect();
        let fields = f.fields(&full[self.fluid_slice.clone()]);
        let velocity: Vec<[f64; 3]> = (0..nc).map(|c| fields.velocity[c]).collect();
        let h: [f64; 3] = std::array::from_fn(|a| x[nc + a]);
        Ok(json!({
            "schema": "implexity-unified-history-field-snapshot/1",
            "semantics": "diagnostic_field_values_no_derivative",
            "state": n,
            "grid": self.grid,
            "cell_spacing_mm": h,
            "occupancy": &x[..nc],
            "phase_fraction": &x[nc + 3..nc + 3 + nc],
            "solid_nodal_temperature_K": nodal,
            "coolant_nodal_temperature_K": coolant,
            "two_temperature": self.film.is_some(),
            "fluid_cell_temperature_K": cells,
            "cell_velocity_m_s": velocity,
        }))
    }
}
