// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_ad::Scalar;

use super::lattice::{C, CS2, Q};
use super::owner::{Interval, Owner};

#[derive(Clone, Debug, Default)]
pub struct Table<S> {
    pub rows: Vec<(String, S)>,
}

impl<S: Copy> Table<S> {
    pub fn set(&mut self, name: &str, value: S) {
        if let Some(row) = self.rows.iter_mut().find(|r| r.0 == name) {
            row.1 = value;
        } else {
            self.rows.push((name.to_string(), value));
        }
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<S> {
        self.rows.iter().find(|r| r.0 == name).map(|r| r.1)
    }

    pub fn remove(&mut self, name: &str) {
        self.rows.retain(|r| r.0 != name);
    }

    pub fn extend(&mut self, other: Table<S>) {
        for (k, v) in other.rows {
            self.set(&k, v);
        }
    }
}

pub fn total<S: Scalar>(x: &[S]) -> S {
    x.iter().fold(S::zero(), |acc, v| acc + *v)
}

pub fn vmax<S: Scalar>(x: &[S]) -> S {
    let m = x.iter().map(Scalar::value).fold(f64::NEG_INFINITY, f64::max);
    let tied: Vec<S> = x.iter().filter(|v| v.value() == m).copied().collect();
    #[allow(clippy::cast_precision_loss)]
    let w = 1.0 / tied.len() as f64;
    S::lift(m, &tied, &vec![w; tied.len()], &[])
}

pub fn vmin<S: Scalar>(x: &[S]) -> S {
    let m = x.iter().map(Scalar::value).fold(f64::INFINITY, f64::min);
    let tied: Vec<S> = x.iter().filter(|v| v.value() == m).copied().collect();
    #[allow(clippy::cast_precision_loss)]
    let w = 1.0 / tied.len() as f64;
    S::lift(m, &tied, &vec![w; tied.len()], &[])
}

pub struct IntervalQuantities<S> {
    pub values: Table<S>,
    pub fields: Vec<(String, Vec<S>)>,
    pub balance: Table<S>,
}

impl Owner {
    #[must_use]
    pub fn interval_quantities<S: Scalar>(
        &self,
        n: usize,
        current: &[S],
        previous: &[S],
        ledger: &Interval<S>,
    ) -> IntervalQuantities<S> {
        let boundary = &ledger.boundary;
        let port = &ledger.port_boundary;
        let mass_scale = self.rho * self.h.powi(3);
        let diff: Vec<S> =
            current[self.ns..].iter().zip(&previous[self.ns..]).map(|(a, b)| *a - *b).collect();
        let mass_rate = total(&diff) * mass_scale / self.dt;
        let pressure_scale = self.rho * (self.h / self.dt).powi(2);
        let planes = [self.grid.plane(0), self.grid.plane(self.grid.n[0] - 1)];
        let face_density: [f64; 2] =
            std::array::from_fn(|f| self.rho * (1.0 + ledger.faces[f] / (CS2 * pressure_scale)));
        let inflow: [Vec<S>; 2] = std::array::from_fn(|f| planes[f].iter().map(|c| port[*c]).collect());
        let volume: [Vec<S>; 2] =
            std::array::from_fn(|f| inflow[f].iter().map(|v| *v / face_density[f]).collect());
        let cal = &ledger.caloric;
        let storage = cal.fluid_energy_increment_j / self.dt;
        let advective = -cal.boundary_power_into_w;
        let mut work = S::zero();
        for f in 0..2 {
            for v in &volume[f] {
                work += *v * ledger.faces[f];
            }
        }
        let mut values = Table::default();
        values.set("interval_inlet_mass_flow_kg_s", total(&inflow[0]));
        values.set("interval_outlet_mass_flow_kg_s", -total(&inflow[1]));
        values.set("interval_fluid_mass_rate_kg_s", mass_rate);
        values.set("interval_advective_enthalpy_out_W", advective);
        values.set("interval_fluid_caloric_storage_W", storage);
        values.set("interval_port_gauge_pressure_work_W", work);
        values.set("interval_drag_dissipation_W", total(&ledger.drag.drag_dissipation_w));
        let residual: Vec<S> = cal.cell_mass_balance_error_kg.iter().map(|v| *v / self.dt).collect();
        let mut fields = vec![
            ("interval_boundary_mass_inflow_kg_s".to_string(), boundary.clone()),
            ("interval_boundary_enthalpy_inflow_W".to_string(), cal.transport.boundary_power_w.clone()),
            ("interval_fluid_mass_balance_residual_kg_s".to_string(), residual.clone()),
        ];
        let caloric_sum = total(&cal.nodal_residual_w);
        let mut balance = Table::default();
        balance.set("mass_balance_residual_kg_s", mass_rate - total(boundary));
        balance.set("summed_local_mass_balance_residual_kg_s", total(&residual));
        balance.set("fluid_caloric_residual_sum_W", caloric_sum);
        balance.set("fluid_caloric_identity_error_W", caloric_sum - storage - advective);
        balance.set("inlet_volume_inflow_m3_s", total(&volume[0]));
        balance.set("outlet_volume_outflow_m3_s", -total(&volume[1]));
        if let (Some(w), Some(e)) = (&self.wall, &ledger.wall) {
            for name in super::catalog::missing_local(Some(&w.selection)) {
                values.remove(name);
            }
            fields[0].1.clone_from(port);
            let rate = &e.reference_mass_rate_kg_s;
            let (to, _) = self.nodal_fields(n - 1, previous);
            let tc = self.cal.cell_temperature(&to);
            fields.push(("interval_wall_reference_mass_rate_kg_s".to_string(), rate.clone()));
            fields.push((
                "interval_wall_reference_caloric_power_W".to_string(),
                rate.iter().zip(&tc).map(|(r, t)| *r * self.cp * *t).collect(),
            ));
            balance.set("wall_reference_mass_rate_kg_s", total(rate));
            if let Some(v) = cal.port_boundary_power_into_w {
                balance.set("port_boundary_power_into_W", v);
            }
            if let Some(v) = cal.wall_reference_caloric_power_into_w {
                balance.set("wall_reference_caloric_power_into_W", v);
            }
        }
        IntervalQuantities { values, fields, balance }
    }

    #[must_use]
    pub fn local_values<S: Scalar>(
        &self,
        n: usize,
        current: &[S],
        previous: &[S],
        x: &[S],
        names: &[String],
    ) -> Vec<S> {
        let mut table: Table<S> = Table::default();
        if names.iter().any(|n| n.starts_with("endpoint_")) {
            let (t, u) = self.nodal_fields(n, current);
            #[allow(clippy::cast_precision_loss)]
            let nn = t.len() as f64;
            table.set("endpoint_mean_temperature_K", total(&t) / nn);
            for (a, letter) in ["x", "y", "z"].iter().enumerate() {
                let col: Vec<S> = u.iter().map(|v| v[a]).collect();
                table.set(&format!("endpoint_mean_displacement_{letter}_m"), total(&col) / nn);
            }
            if names.iter().any(|n| n == "endpoint_max_temperature_K") {
                table.set("endpoint_max_temperature_K", vmax(&t));
            }
        }
        if names.iter().any(|n| n == "design_solid_volume_m3") {
            let (xs, _, _) = self.phase(x);
            table.set("design_solid_volume_m3", total(&xs[..self.nc()]) * self.h.powi(3));
        }
        if names.iter().any(|n| super::catalog::is_transport(n)) {
            let ledger = self.transport_interval(n, current, previous, x);
            table.extend(self.interval_quantities(n, current, previous, &ledger).values);
        }
        names.iter().map(|name| table.get(name).unwrap_or_else(S::zero)).collect()
    }

    #[must_use]
    pub fn mechanical_quantities<S: Scalar>(
        &self,
        n: usize,
        current: &[S],
        previous: &[S],
        ledger: &Interval<S>,
    ) -> (Table<S>, Table<S>) {
        let drag = &ledger.drag;
        let velocity: Vec<[S; 3]> = ledger
            .displacement
            .iter()
            .zip(&ledger.previous_displacement)
            .map(|(a, b)| std::array::from_fn(|k| (a[k] - b[k]) / self.dt))
            .collect();
        let mut quantities = Table::default();
        let mut identities = Table::default();
        let mut loads: Vec<(&str, Vec<[S; 3]>)> = vec![("drag", drag.solid_nodal_force_n.clone())];
        if self.wall.is_none() {
            loads.push(("pressure_trace", self.interface_load(ledger)));
        } else {
            let (q, e) = self.wall_quantities(ledger);
            quantities.extend(q);
            identities.extend(e);
        }
        if self.viscous.is_some() {
            let cells = self.viscous_cells(previous, ledger);
            if self.viscous_flag("heating") {
                let heat: Vec<S> = cells.iter().map(|c| c.heat).collect();
                quantities.set("interval_viscous_dissipation_W", total(&heat));
            }
            if self.viscous_flag("trace_traction") {
                loads.push(("viscous_trace", self.viscous_trace(&cells)));
            }
        }
        let free = &self.s.free_u;
        for (name, load) in &loads {
            let work: Vec<S> =
                load.iter().zip(&velocity).flat_map(|(f, v)| (0..3).map(move |k| f[k] * v[k])).collect();
            let all = total(&work);
            let selected: Vec<S> = free.iter().map(|d| work[*d]).collect();
            let free_work = total(&selected);
            let key = if *name == "drag" {
                "interval_solid_drag_work_W".to_string()
            } else {
                format!("interval_{name}_solid_work_W")
            };
            quantities.set(&key, all);
            identities.set(&format!("{name}_solid_free_dof_work_W"), free_work);
            identities.set(&format!("{name}_solid_prescribed_dof_work_W"), all - free_work);
        }
        quantities.set("interval_fluid_drag_work_W", total(&drag.fluid_drag_work_w));
        let scale = self.rho * self.h.powi(5) / self.dt.powi(3);
        let mut pp = S::zero();
        for c in &drag.cells {
            for a in 0..3 {
                pp += c.pressure_correction[a] * c.drag.intrinsic_velocity[a];
            }
        }
        quantities.set("interval_pressure_porosity_work_W", pp * scale);
        let fluid = quantities.get("interval_fluid_drag_work_W").unwrap_or_else(S::zero);
        let solid = quantities.get("interval_solid_drag_work_W").unwrap_or_else(S::zero);
        identities.set("drag_pair_plus_heat_error_W", fluid + solid + total(&drag.drag_dissipation_w));
        identities.set("drag_cell_to_nodal_work_error_W", solid - total(&drag.solid_drag_work_w));
        identities.set(
            "drag_cell_to_nodal_heat_error_W",
            total(&drag.shared_nodal_heat_w) - total(&drag.drag_dissipation_w),
        );
        let pre = &previous[self.ns..];
        let post = &drag.post;
        let nc = self.nc();
        let mut kinetic = Vec::with_capacity(nc);
        let mut force_power = Vec::with_capacity(nc);
        let mut mass_change = Vec::with_capacity(nc);
        let mut momentum_error = Vec::with_capacity(3 * nc);
        for c in 0..nc {
            let (m0, j0) = moments(&pre[c * Q..(c + 1) * Q]);
            let (m1, j1) = moments(&post[c * Q..(c + 1) * Q]);
            let d = &drag.cells[c].drag;
            let dot = (0..3).fold(S::zero(), |acc, a| acc + (j1[a] - j0[a]) * (j1[a] + j0[a]));
            let j00 = (0..3).fold(S::zero(), |acc, a| acc + j0[a] * j0[a]);
            kinetic.push((dot / m1 + j00 * (m0 - m1) / (m0 * m1)) * scale * 0.5);
            force_power.push(
                (0..3)
                    .fold(S::zero(), |acc, a| acc + d.total_fluid_force_density[a] * d.intrinsic_velocity[a])
                    * scale,
            );
            mass_change.push((m1 - m0).abs());
            for a in 0..3 {
                momentum_error.push((j1[a] - j0[a] - d.total_fluid_force_density[a]).abs());
            }
        }
        let err_cells: Vec<S> = kinetic.iter().zip(&force_power).map(|(k, f)| *k - *f).collect();
        identities.set("collision_raw_kinetic_rate_W", total(&kinetic));
        identities.set("collision_force_work_W", total(&force_power));
        identities.set("collision_raw_kinetic_identity_error_W", total(&err_cells));
        let abs_err: Vec<S> = err_cells.iter().map(|v| v.abs()).collect();
        identities.set("collision_max_abs_cell_kinetic_identity_error_W", vmax(&abs_err));
        identities
            .set("collision_max_abs_cell_mass_change_kg", vmax(&mass_change) * self.rho * self.h.powi(3));
        identities.set(
            "collision_max_abs_cell_momentum_error_N_s",
            vmax(&momentum_error) * self.rho * self.h.powi(4) / self.dt,
        );
        let _ = (n, current);
        (quantities, identities)
    }

    #[must_use]
    pub fn wall_quantities<S: Scalar>(&self, ledger: &Interval<S>) -> (Table<S>, Table<S>) {
        let mut q = Table::default();
        let mut e = Table::default();
        let Some(w) = &ledger.wall else { return (q, e) };
        let velocity: Vec<[S; 3]> = ledger
            .displacement
            .iter()
            .zip(&ledger.previous_displacement)
            .map(|(a, b)| std::array::from_fn(|k| (a[k] - b[k]) / self.dt))
            .collect();
        let dot = |f: &[[S; 3]], v: &[[S; 3]]| -> S {
            let mut acc = S::zero();
            for (a, b) in f.iter().zip(v) {
                for k in 0..3 {
                    acc += a[k] * b[k];
                }
            }
            acc
        };
        let datum: Vec<[S; 3]> = w.pressure_datum_nodal_force_n.iter().map(|d| d.map(S::from_f64)).collect();
        let solid_work = dot(&w.kinetic_solid_nodal_force_n, &velocity);
        let fluid_work = dot(&w.fluid_material_force_n, &w.wall_velocity_m_s);
        let datum_work = dot(&datum, &velocity);
        let total_work = dot(&w.solid_nodal_force_n, &velocity);
        let flat: Vec<S> = w
            .solid_nodal_force_n
            .iter()
            .zip(&velocity)
            .flat_map(|(f, v)| (0..3).map(move |k| f[k] * v[k]))
            .collect();
        let selected: Vec<S> = self.s.free_u.iter().map(|d| flat[*d]).collect();
        let transfer = dot(&w.solid_event_force_n, &w.wall_velocity_m_s);
        let vector: Vec<S> = w
            .solid_event_force_n
            .iter()
            .zip(&w.raw_fluid_momentum_rate_n)
            .zip(&w.mass_momentum_rate_n)
            .flat_map(|((a, b), c)| (0..3).map(move |k| (a[k] + b[k] - c[k]).abs()))
            .collect();
        q.set("interval_wall_solid_traction_work_W", solid_work);
        q.set("interval_wall_fluid_traction_work_W", fluid_work);
        q.set("interval_wall_pressure_datum_work_W", datum_work);
        q.set("interval_wall_total_solid_work_W", total_work);
        q.set("interval_wall_reference_mass_rate_kg_s", total(&w.reference_mass_rate_kg_s));
        q.set(
            "interval_wall_reference_caloric_power_W",
            ledger.caloric.wall_reference_caloric_power_into_w.unwrap_or_else(S::zero),
        );
        e.set("wall_traction_pair_work_error_W", solid_work + fluid_work);
        e.set("wall_actual_solid_work_balance_error_W", total_work + fluid_work - datum_work);
        e.set("wall_event_to_nodal_work_error_W", solid_work - transfer);
        e.set("wall_max_abs_event_momentum_balance_error_N", vmax(&vector));
        let free = total(&selected);
        e.set("wall_total_solid_free_dof_work_W", free);
        e.set("wall_total_solid_prescribed_dof_work_W", total_work - free);
        let disp: Vec<S> = ledger.displacement.iter().flatten().map(|v| v.abs()).collect();
        let inc: Vec<S> = velocity.iter().flatten().map(|v| v.abs()).collect();
        e.set("domain_max_abs_displacement_over_spacing", vmax(&disp) / self.h);
        e.set("domain_max_abs_increment_over_spacing", vmax(&inc) * self.dt / self.h);
        (q, e)
    }

    #[must_use]
    pub fn energy_quantities<S: Scalar>(
        &self,
        previous: &[S],
        current: &[S],
        ledger: &Interval<S>,
    ) -> (Table<S>, Table<S>) {
        let before = &previous[self.ns..];
        let after = &current[self.ns..];
        let post = &ledger.drag.post;
        let streamed = &ledger.streamed;
        let wall = &ledger.after_wall;
        let expected = &ledger.expected;
        let scale = self.rho * self.h.powi(5) / self.dt.powi(2);
        let stages: [&[S]; 6] = [before, post, streamed, wall, expected, after];
        let change = |a: &[S], b: &[S]| -> S { total(&raw_kinetic_change(a, b, scale)) / self.dt };
        let changes: Vec<S> = (0..5).map(|k| change(stages[k], stages[k + 1])).collect();
        let whole = change(before, after);
        let escape = total(&beam_energy(&ledger.escaped, scale)) / self.dt;
        let mut values = Table::default();
        let names = super::catalog::STAGES;
        let mut v = vec![whole];
        v.extend(changes.iter().copied());
        v.push(escape);
        v.push(changes[1] + escape);
        for ((name, _), value) in names.iter().zip(v) {
            values.set(&format!("interval_{name}_W"), value);
        }
        let diff: Vec<S> = streamed.iter().zip(post).map(|(a, b)| *a - *b).collect();
        let beam_change = total(&beam_energy(&diff, scale)) / self.dt;
        let mut collision_force = S::zero();
        for c in &ledger.drag.cells {
            for a in 0..3 {
                collision_force += c.drag.total_fluid_force_density[a] * c.drag.intrinsic_velocity[a];
            }
        }
        let collision_force = collision_force * scale / self.dt;
        let nc = self.nc();
        let phase: Vec<S> = ledger.phase_design[..nc].iter().map(|v| S::one() - *v).collect();
        let gap: Vec<S> = ledger.q.iter().zip(&phase).map(|(q, p)| *q - *p).collect();
        let mut extra = Table::default();
        extra.set("telescoping_kinetic_identity_error_W", whole - total(&changes));
        extra.set("streaming_beam_conservation_error_W", beam_change + escape);
        extra.set("collision_force_work_W", collision_force);
        extra.set("collision_raw_kinetic_identity_error_W", changes[0] - collision_force);
        let defect: Vec<S> = after.iter().zip(expected).map(|(a, b)| (*a - *b).abs()).collect();
        extra.set("maximum_abs_population_update_defect", vmax(&defect));
        let minima: Vec<S> = stages
            .iter()
            .map(|f| vmin(&(0..nc).map(|c| total(&f[c * Q..(c + 1) * Q])).collect::<Vec<S>>()))
            .collect();
        extra.set("minimum_stage_population_mass", vmin(&minima));
        let storage = |f: &[S]| -> S {
            let mut acc = S::zero();
            for c in 0..nc {
                let (m, j) = moments(&f[c * Q..(c + 1) * Q]);
                acc += (j[0] * j[0] + j[1] * j[1] + j[2] * j[2]) / m;
            }
            acc * scale * 0.5
        };
        extra.set("raw_kinetic_storage_start_J", storage(before));
        extra.set("raw_kinetic_storage_end_J", storage(after));
        extra
            .set("fluid_sensible_enthalpy_storage_rate_W", ledger.caloric.fluid_energy_increment_j / self.dt);
        extra.set("fluid_caloric_residual_sum_W", total(&ledger.caloric.nodal_residual_w));
        extra.set("actual_drag_heat_W", total(&ledger.drag.shared_nodal_heat_w));
        let abs_gap: Vec<S> = gap.iter().map(|v| v.abs()).collect();
        extra.set("maximum_abs_local_phase_storage_weight_gap", vmax(&abs_gap));
        extra.set("phase_storage_weight_gap_l1_m3", total(&abs_gap) * self.h.powi(3));
        if self.viscous_flag("heating") {
            let cells = self.viscous_cells(previous, ledger);
            let heat: Vec<S> = cells.iter().map(|c| c.heat).collect();
            extra.set("actual_selected_constitutive_viscous_heat_W", total(&heat));
        }
        (values, extra)
    }
}

fn moments<S: Scalar>(f: &[S]) -> (S, [S; 3]) {
    let m = total(f);
    let j = std::array::from_fn(|a| (0..Q).fold(S::zero(), |acc, i| acc + f[i] * f64::from(C[i][a])));
    (m, j)
}

fn raw_kinetic_change<S: Scalar>(before: &[S], after: &[S], scale: f64) -> Vec<S> {
    let nc = before.len() / Q;
    (0..nc)
        .map(|c| {
            let (m0, p0) = moments(&before[c * Q..(c + 1) * Q]);
            let (m1, p1) = moments(&after[c * Q..(c + 1) * Q]);
            let dot = (0..3).fold(S::zero(), |acc, a| acc + (p1[a] - p0[a]) * (p1[a] + p0[a]));
            let p00 = (0..3).fold(S::zero(), |acc, a| acc + p0[a] * p0[a]);
            (dot / m1 + p00 * (m0 - m1) / (m0 * m1)) * scale * 0.5
        })
        .collect()
}

fn beam_energy<S: Scalar>(f: &[S], scale: f64) -> Vec<S> {
    let nc = f.len() / Q;
    (0..nc)
        .map(|c| {
            let e = (0..Q).fold(S::zero(), |acc, i| {
                let c2 = f64::from(C[i][0] * C[i][0] + C[i][1] * C[i][1] + C[i][2] * C[i][2]);
                acc + f[c * Q + i] * c2
            });
            e * scale * 0.5
        })
        .collect()
}
