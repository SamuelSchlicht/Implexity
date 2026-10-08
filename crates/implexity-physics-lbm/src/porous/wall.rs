// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use serde_json::{Map, Value, json};

use super::caloric::{CaloricLedger, SharedFluidCaloric};
use super::lattice::{C, CS2, Grid, OPPOSITE, Q, W, cf};
use super::sp::{self, Sp, SpMap};

pub const SCHEMA: &str = "implexity-porous-reference-wall/1";
pub const PROFILE: &str = "planar_x_pressure_reference_wall_periodic_yz";
pub const KINEMATICS: &str = "small_displacement_reference_domain";

fn err(msg: &str) -> CaeError {
    CaeError::contract(msg)
}

fn text(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.trim().is_empty())
}


pub fn normalise_selection(value: &Value) -> CaeResult<Option<Value>> {
    if value.is_null() {
        return Ok(None);
    }
    let keys = ["schema", "kind", "face", "kinematics", "pressure_load", "provenance"];
    let ok =
        value.as_object().is_some_and(|m| m.len() == keys.len() && keys.iter().all(|k| m.contains_key(*k)));
    if !ok {
        return Err(err("reference_wall requires its exact explicit authoring fields"));
    }
    if value["schema"] != json!(SCHEMA) || value["kind"] != json!("halfway_momentum_exchange") {
        return Err(err("unsupported reference-wall schema or boundary law"));
    }
    if !(value["face"] == json!("x_min") || value["face"] == json!("x_max"))
        || value["kinematics"] != json!(KINEMATICS)
    {
        return Err(err(
            "reference wall supports an explicitly selected planar x face and small-displacement reference kinematics only",
        ));
    }
    if !(value["pressure_load"] == json!("internal_absolute")
        || value["pressure_load"] == json!("internal_minus_exterior"))
    {
        return Err(err("explicit absolute or differential wall pressure-load convention required"));
    }
    if !text(&value["provenance"]) {
        return Err(err("reference-wall geometry and kinematics provenance required"));
    }
    Ok(Some(value.clone()))
}


pub fn pressure_array(value: &Value, nt: usize, wall: &Value) -> CaeResult<Vec<[f64; 2]>> {
    let rows = value
        .as_array()
        .filter(|r| r.len() == nt && r.iter().all(|x| x.as_array().is_some_and(|x| x.len() == 2)));
    let Some(rows) = rows else {
        return Err(err("two x-face pressure columns and every physical time required"));
    };
    let face = usize::from(wall["face"] != json!("x_min"));
    if rows.iter().any(|r| !r[face].is_null()) {
        return Err(err("sealed wall pressure entries must be null, not imposed pressure data"));
    }
    if rows.iter().any(|r| r[1 - face].is_boolean()) {
        return Err(err("boolean pressure data are not physical pressure values"));
    }
    let active: Vec<Option<f64>> = rows.iter().map(|r| r[1 - face].as_f64()).collect();
    if active.iter().any(|v| v.is_none_or(|v| !v.is_finite())) {
        return Err(err("finite real prescribed pressure at the remaining reservoir face required"));
    }
    Ok(active
        .iter()
        .map(|v| {
            let mut row = [0.0; 2];
            row[1 - face] = v.unwrap_or(0.0);
            row
        })
        .collect())
}

fn face_trace(grid: Grid, nn: usize, points: &[[f64; 3]]) -> CaeResult<Sp> {
    let ns = grid.n.map(|v| v + 1);
    let node = |p: [usize; 3]| (p[0] * ns[1] + p[1]) * ns[2] + p[2];
    let (mut rows, mut cols, mut data) = (Vec::new(), Vec::new(), Vec::new());
    for (row, point) in points.iter().enumerate() {
        let mut images = vec![*point];
        for axis in [1, 2] {
            #[allow(clippy::cast_precision_loss)]
            let top = grid.n[axis] as f64;
            if point[axis] == 0.0 || point[axis] == top {
                let mut next = Vec::new();
                for p in &images {
                    let mut a = *p;
                    a[axis] = 0.0;
                    next.push(a);
                }
                for p in &images {
                    let mut a = *p;
                    a[axis] = top;
                    next.push(a);
                }
                images = next;
            }
        }
        #[allow(clippy::cast_precision_loss)]
        let count = images.len() as f64;
        for p in &images {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let lower: [usize; 3] =
                std::array::from_fn(|a| (p[a].floor().max(0.0) as usize).min(grid.n[a] - 1));
            #[allow(clippy::cast_precision_loss)]
            let f: [f64; 3] = std::array::from_fn(|a| p[a] - lower[a] as f64);
            for corner in 0..8 {
                let off = [corner >> 2 & 1, corner >> 1 & 1, corner & 1];
                let w: f64 =
                    (0..3).map(|a| if off[a] == 1 { f[a] } else { 1.0 - f[a] }).product::<f64>() / count;
                if w != 0.0 {
                    rows.push(row);
                    cols.push(node([lower[0] + off[0], lower[1] + off[1], lower[2] + off[2]]));
                    data.push(w);
                }
            }
        }
    }
    let out = sp::triplets(points.len(), nn, &rows, &cols, &data)?;
    let sums = sp::mv(&out, &vec![1.0; nn])?;
    if sums.iter().any(|s| (s - 1.0).abs() > 2e-15) || out.data().iter().any(|v| *v < 0.0) {
        return Err(err("invalid reference-wall interpolation"));
    }
    Ok(out)
}

#[derive(Clone, Debug)]
pub struct WallExchange<S> {
    pub returned: Vec<S>,
    pub outgoing: Vec<S>,
    pub mass_delta_lattice: Vec<S>,
    pub wall_velocity_m_s: Vec<[S; 3]>,
    pub solid_event_force_n: Vec<[S; 3]>,
    pub raw_fluid_momentum_rate_n: Vec<[S; 3]>,
    pub mass_momentum_rate_n: Vec<[S; 3]>,
    pub fluid_material_force_n: Vec<[S; 3]>,
    pub kinetic_solid_nodal_force_n: Vec<[S; 3]>,
    pub pressure_datum_nodal_force_n: Vec<[f64; 3]>,
    pub solid_nodal_force_n: Vec<[S; 3]>,
    pub reference_mass_rate_kg_s: Vec<S>,
}

#[derive(Clone, Debug)]
pub struct ReferenceWall {
    pub selection: Value,
    grid: Grid,
    nc: usize,
    dt: f64,
    pub face: usize,
    pub cell: Vec<usize>,
    pub direction: Vec<usize>,
    c: Vec<[f64; 3]>,
    w: Vec<f64>,
    pub outgoing: Vec<usize>,
    pub incoming: Vec<usize>,
    pub count: usize,
    pub h: Sp,
    hmap: SpMap,
    force_scale: f64,
    mass_rate_scale: f64,
    velocity_scale: f64,
    pub port_mask: Vec<f64>,
    pub datum_load_n: Vec<[f64; 3]>,
}

#[derive(Clone, Debug)]
pub struct WallPartials {
    pub returned_post: Sp,
    pub returned_velocity: Sp,
    pub solid_force_post: Sp,
    pub solid_force_velocity: Sp,
    pub mass_rate_post: Sp,
    pub mass_rate_velocity: Sp,
}

impl ReferenceWall {

    pub fn new(
        grid: Grid,
        nn: usize,
        selection: &Value,
        h: f64,
        dt: f64,
        rho: f64,
        pressure_reference: f64,
        exterior: f64,
    ) -> CaeResult<Self> {
        let Some(selection) = normalise_selection(selection)? else {
            return Err(err("explicit reference-wall selection required"));
        };
        if [h, dt, rho, pressure_reference].iter().any(|v| !v.is_finite() || *v <= 0.0)
            || !exterior.is_finite()
        {
            return Err(err("finite positive physical wall scales/reference required"));
        }
        let nc = grid.cells();
        let face = usize::from(selection["face"] != json!("x_min"));
        let index = if face == 0 { 0 } else { grid.n[0] - 1 };
        let side = if face == 0 { -1 } else { 1 };
        let normal = [f64::from(side), 0.0, 0.0];
        let cells = grid.plane(index);
        let directions: Vec<usize> = (0..Q).filter(|i| C[*i][0] == side).collect();
        let cell: Vec<usize> = cells.iter().flat_map(|c| vec![*c; directions.len()]).collect();
        let direction: Vec<usize> = cells.iter().flat_map(|_| directions.clone()).collect();
        let c: Vec<[f64; 3]> = direction.iter().map(|d| cf(*d)).collect();
        let w: Vec<f64> = direction.iter().map(|d| W[*d]).collect();
        let outgoing: Vec<usize> = cell.iter().zip(&direction).map(|(x, d)| Q * x + d).collect();
        let incoming: Vec<usize> = cell.iter().zip(&direction).map(|(x, d)| Q * x + OPPOSITE[*d]).collect();
        let count = cell.len();
        #[allow(clippy::cast_precision_loss)]
        let centre = |x: usize| grid.ijk(x).map(|v| v as f64 + 0.5);
        let points: Vec<[f64; 3]> =
            cell.iter().zip(&c).map(|(x, cc)| std::array::from_fn(|a| centre(*x)[a] + 0.5 * cc[a])).collect();
        #[allow(clippy::cast_precision_loss)]
        let face_x = if face == 0 { 0.0 } else { grid.n[0] as f64 };
        let face_centres: Vec<[f64; 3]> = cells
            .iter()
            .map(|x| {
                let mut p = centre(*x);
                p[0] = face_x;
                p
            })
            .collect();
        let htrace = face_trace(grid, nn, &points)?;
        let centre_trace = face_trace(grid, nn, &face_centres)?;
        let force_scale = rho * h.powi(4) / dt.powi(2);
        let mut port_mask = vec![1.0; nc];
        for x in &cells {
            port_mask[*x] = 0.0;
        }
        let reference_event: Vec<[f64; 3]> =
            (0..count).map(|k| c[k].map(|v| 2.0 * w[k] * v * force_scale)).collect();
        let hmap = SpMap::new(&htrace);
        let kinetic = hmap.apply_t_vec(&reference_event);
        let external =
            if selection["pressure_load"] == json!("internal_minus_exterior") { exterior } else { 0.0 };
        let load: Vec<[f64; 3]> =
            vec![normal.map(|n| (pressure_reference - external) * h.powi(2) * n); cells.len()];
        let physical = SpMap::new(&centre_trace).apply_t_vec(&load);
        let datum =
            physical.iter().zip(&kinetic).map(|(p, k)| std::array::from_fn(|a| p[a] - k[a])).collect();
        Ok(Self {
            selection,
            grid,
            nc,
            dt,
            face,
            cell,
            direction,
            c,
            w,
            outgoing,
            incoming,
            count,
            h: htrace,
            hmap,
            force_scale,
            mass_rate_scale: rho * h.powi(3) / dt,
            velocity_scale: dt / h,
            port_mask,
            datum_load_n: datum,
        })
    }

    #[must_use]
    pub fn exchange<S: Scalar>(&self, post: &[S], nodal_velocity: &[[S; 3]]) -> WallExchange<S> {
        let velocity = self.hmap.apply_vec(nodal_velocity);
        let v: Vec<[S; 3]> = velocity.iter().map(|x| x.map(|y| y * self.velocity_scale)).collect();
        let a: Vec<S> = self.outgoing.iter().map(|i| post[*i]).collect();
        let mass: Vec<S> = self
            .cell
            .iter()
            .map(|x| post[x * Q..(x + 1) * Q].iter().fold(S::zero(), |acc, f| acc + *f))
            .collect();
        let fs = self.force_scale;
        let mut delta = Vec::with_capacity(self.count);
        let mut b = Vec::with_capacity(self.count);
        let mut raw = Vec::with_capacity(self.count);
        let mut convective = Vec::with_capacity(self.count);
        let mut event = Vec::with_capacity(self.count);
        for k in 0..self.count {
            let c = self.c[k];
            let cv = v[k][0] * c[0] + v[k][1] * c[1] + v[k][2] * c[2];
            let d = mass[k] * (-2.0 * self.w[k]) * cv / CS2;
            let bk = a[k] + d;
            let ab = a[k] + bk;
            raw.push(std::array::from_fn(|x| -(ab * c[x]) * fs));
            convective.push(std::array::from_fn(|x| d * v[k][x] * fs));
            event.push(std::array::from_fn(|x| (ab * c[x] + d * v[k][x]) * fs));
            delta.push(d);
            b.push(bk);
        }
        let kinetic = self.hmap.apply_t_vec(&event);
        let mut rate = vec![S::zero(); self.nc];
        for (k, x) in self.cell.iter().enumerate() {
            rate[*x] += delta[k];
        }
        let rate: Vec<S> = rate.into_iter().map(|r| r * self.mass_rate_scale).collect();
        let material = raw
            .iter()
            .zip(&convective)
            .map(|(r, c): (&[S; 3], &[S; 3])| std::array::from_fn(|x| r[x] - c[x]))
            .collect();
        let total = kinetic
            .iter()
            .zip(&self.datum_load_n)
            .map(|(k, d)| std::array::from_fn(|x| k[x] + d[x]))
            .collect();
        WallExchange {
            returned: b,
            outgoing: a,
            mass_delta_lattice: delta,
            wall_velocity_m_s: velocity,
            solid_event_force_n: event,
            raw_fluid_momentum_rate_n: raw,
            mass_momentum_rate_n: convective,
            fluid_material_force_n: material,
            kinetic_solid_nodal_force_n: kinetic,
            pressure_datum_nodal_force_n: self.datum_load_n.clone(),
            solid_nodal_force_n: total,
            reference_mass_rate_kg_s: rate,
        }
    }

    pub fn return_populations<S: Scalar>(&self, streamed: &mut [S], exchange: &WallExchange<S>) {
        for (i, b) in self.incoming.iter().zip(&exchange.returned) {
            streamed[*i] += *b;
        }
    }

    #[must_use]
    pub fn caloric<S: Scalar>(
        &self,
        cal: &SharedFluidCaloric,
        mut ledger: CaloricLedger<S>,
        exchange: &WallExchange<S>,
        previous_temperature: &[S],
        cp: f64,
    ) -> CaloricLedger<S> {
        let rate = &exchange.reference_mass_rate_kg_s;
        let cr: Vec<S> = rate.iter().map(|r| *r * cp).collect();
        let capacity = cal.lmap().apply_t(&cr);
        let nodal: Vec<S> = capacity.iter().zip(previous_temperature).map(|(c, t)| *c * *t).collect();
        let power = nodal.iter().fold(S::zero(), |acc, v| acc + *v);
        for (r, n) in ledger.nodal_residual_w.iter_mut().zip(&nodal) {
            *r -= *n;
        }
        for (r, n) in ledger.nodal_transport_outward_w.iter_mut().zip(&nodal) {
            *r -= *n;
        }
        ledger.port_boundary_power_into_w = Some(ledger.boundary_power_into_w);
        ledger.wall_reference_caloric_power_into_w = Some(power);
        ledger.boundary_power_into_w += power;
        for (e, r) in ledger.cell_mass_balance_error_kg.iter_mut().zip(rate) {
            *e -= *r * self.dt;
        }
        ledger.wall_reference_nodal_power_w = Some(nodal);
        ledger.port_net_mass_rate_kg_s = Some(ledger.transport.net_mass_rate_kg_s.clone());
        for (n, r) in ledger.transport.net_mass_rate_kg_s.iter_mut().zip(rate) {
            *n += *r;
        }
        ledger
    }


    pub fn partials(&self, post: &[f64], nodal_velocity: &[[f64; 3]]) -> CaeResult<WallPartials> {
        let nf = Q * self.nc;
        let count = self.count;
        let v: Vec<[f64; 3]> =
            self.hmap.apply_vec(nodal_velocity).iter().map(|x| x.map(|y| y * self.velocity_scale)).collect();
        let msel: Vec<usize> =
            (0..count).flat_map(|k| (0..Q).map(move |i| (k, i))).map(|(k, i)| Q * self.cell[k] + i).collect();
        let mrows: Vec<usize> = (0..count).flat_map(|k| [k; Q]).collect();
        let mmat = sp::triplets(count, nf, &mrows, &msel, &vec![1.0; msel.len()])?;
        let amat =
            sp::triplets(count, nf, &(0..count).collect::<Vec<_>>(), &self.outgoing, &vec![1.0; count])?;
        let m = sp::mv(&mmat, post)?;
        let dm: Vec<f64> = (0..count)
            .map(|k| {
                -2.0 * self.w[k] * (self.c[k][0] * v[k][0] + self.c[k][1] * v[k][1] + self.c[k][2] * v[k][2])
                    / CS2
            })
            .collect();
        let dv: Vec<[f64; 3]> =
            (0..count).map(|k| self.c[k].map(|c| -2.0 * self.w[k] * m[k] * c / CS2)).collect();
        let delta: Vec<f64> = dm.iter().zip(&m).map(|(a, b)| a * b).collect();
        let (mut r, mut c, mut x) = (Vec::new(), Vec::new(), Vec::new());
        for k in 0..count {
            for a in 0..3 {
                r.push(k);
                c.push(3 * k + a);
                x.push(dv[k][a]);
            }
        }
        let dvm = sp::triplets(count, 3 * count, &r, &c, &x)?;
        let h3 = sp::kron_eye(&self.h, 3)?;
        let dpost = sp::mm(&sp::diag(&dm), &mmat)?;
        let dvel = sp::scale(&sp::mm(&dvm, &h3)?, self.velocity_scale);
        let columns = |values: &[[f64; 3]]| {
            let (mut r, mut c, mut x) = (Vec::new(), Vec::new(), Vec::new());
            for (k, v) in values.iter().enumerate() {
                for a in 0..3 {
                    r.push(3 * k + a);
                    c.push(k);
                    x.push(v[a]);
                }
            }
            sp::triplets(3 * count, count, &r, &c, &x)
        };
        let force_a: Vec<[f64; 3]> = self.c.iter().map(|c| c.map(|v| 2.0 * v * self.force_scale)).collect();
        let force_d: Vec<[f64; 3]> = self
            .c
            .iter()
            .zip(&v)
            .map(|(c, v)| std::array::from_fn(|a| (c[a] + v[a]) * self.force_scale))
            .collect();
        let fp = sp::add(&sp::mm(&columns(&force_a)?, &amat)?, &sp::mm(&columns(&force_d)?, &dpost)?)?;
        let rep: Vec<f64> = delta.iter().flat_map(|d| [d * self.force_scale; 3]).collect();
        let fv = sp::add(
            &sp::mm(&columns(&force_d)?, &dvel)?,
            &sp::scale(&sp::mm(&sp::diag(&rep), &h3)?, self.velocity_scale),
        )?;
        let scatter =
            sp::triplets(self.nc, count, &self.cell, &(0..count).collect::<Vec<_>>(), &vec![1.0; count])?;
        let h3t = sp::t(&h3);
        Ok(WallPartials {
            returned_post: sp::add(&amat, &dpost)?,
            returned_velocity: dvel.clone(),
            solid_force_post: sp::mm(&h3t, &fp)?,
            solid_force_velocity: sp::mm(&h3t, &fv)?,
            mass_rate_post: sp::scale(&sp::mm(&scatter, &dpost)?, self.mass_rate_scale),
            mass_rate_velocity: sp::scale(&sp::mm(&scatter, &dvel)?, self.mass_rate_scale),
        })
    }


    pub fn inject(&self) -> CaeResult<Sp> {
        sp::triplets(
            Q * self.nc,
            self.count,
            &self.incoming,
            &(0..self.count).collect::<Vec<_>>(),
            &vec![1.0; self.count],
        )
    }

    #[must_use]
    pub fn metadata(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("schema".into(), json!(SCHEMA));
        m.insert("selection".into(), self.selection.clone());
        m.insert("reciprocal_kinetic_traction".into(), json!(true));
        m.insert("velocity_force_transfer".into(), json!("same_trace_and_transpose"));
        m.insert("geometry_moves".into(), json!(false));
        m.insert("finite_motion_fsi".into(), json!(false));
        m.insert("normal_mass_term".into(), json!("reference_domain_swept_volume_approximation"));
        m.insert("pressure_datum".into(), json!("separate_authored_reference_exterior_load_and_work"));
        m.insert("mechanical_work_added_as_heat".into(), json!(false));
        m.insert("total_energy_qualified".into(), json!(false));
        m.insert("face".into(), self.selection["face"].clone());
        m.insert("link_events".into(), json!(self.count));
        m
    }

    #[must_use]
    pub fn grid(&self) -> Grid {
        self.grid
    }
}
