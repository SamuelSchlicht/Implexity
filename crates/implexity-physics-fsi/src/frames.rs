// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;

use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};

use implexity_core::contracts::FieldValue;
use implexity_core::{CaeError, CaeResult};
use implexity_physics_lbm::moving::carrier::LagrangianCarrier;
use implexity_physics_lbm::moving::psm::saturate;
use implexity_physics_lbm::moving::pushforward::{Endpoint, Frame, PointSet, occupancy};
use implexity_runtime::dynamic_frames::capture;
use implexity_runtime::dynamic_frames::manifest::{
    FieldKind, FieldSpec, FrameManifest, GridSpec, PaletteHint, Precision, Retention, SeriesSpec, TimeBase,
};
use implexity_solve::time_stepper::{StepParameters, TimeStepper};

use crate::model::FsiModel;
use crate::interface::FluidField;
use crate::moving_contact::solid_view::SolidView;
use implexity_solve::multirate_coupling::MultirateStepper;
use crate::run::DynamicResult;

fn array(values: Vec<f64>, shape: &[usize]) -> CaeResult<FieldValue> {
    ArrayD::from_shape_vec(IxDyn(shape), values)
        .map(FieldValue::Array)
        .map_err(|e| CaeError::contract(format!("internal frame shape error: {e}")))
}


pub fn occupancy_field(model: &FsiModel, u: &[f64], design: &[f64]) -> CaeResult<Vec<f64>> {
    let f = &model.problem.fluid;
    let c = &model.problem.coupling;
    let frame = Frame::new(f.shape, f.spacing_m, f.origin_m, f.periodic, c.kernel_width_cells)?;
    let cloud = LagrangianCarrier::points(model.carrier.as_ref(), u, design)?;
    let cell = f.spacing_m.powi(3);
    let xi: Vec<[f64; 3]> = cloud.positions.iter().map(|x| frame.to_lattice(*x)).collect();
    let a: Vec<f64> = cloud.weights.iter().map(|w| w / cell).collect();
    let end = Endpoint::new(&frame, PointSet { xi, a }, false)?;
    let v = vec![[0.0; 3]; end.points.a.len()];
    let occ = occupancy(&frame, [&end, &end], &v);
    let mut out = vec![0.0; frame.grid().cells()];
    for (r, &cell_index) in occ.cells.iter().enumerate() {
        out[cell_index as usize] = saturate(occ.d[0][r], c.saturation_width);
    }
    Ok(out)
}

fn equivalent_strain(f: &[[f64; 3]; 3]) -> f64 {
    let mut e = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            let c: f64 = (0..3).map(|k| f[k][i] * f[k][j]).sum();
            e[i][j] = 0.5 * (c - if i == j { 1.0 } else { 0.0 });
        }
    }
    let tr = (e[0][0] + e[1][1] + e[2][2]) / 3.0;
    let mut dd = 0.0;
    for (i, row) in e.iter().enumerate() {
        for (j, x) in row.iter().enumerate() {
            let d = x - if i == j { tr } else { 0.0 };
            dd += d * d;
        }
    }
    (2.0 / 3.0 * dd).sqrt()
}

fn von_mises(s: &[[f64; 3]; 3]) -> f64 {
    let d = (s[0][0] - s[1][1]).powi(2) + (s[1][1] - s[2][2]).powi(2) + (s[2][2] - s[0][0]).powi(2);
    (0.5 * d + 3.0 * (s[0][1].powi(2) + s[1][2].powi(2) + s[2][0].powi(2))).sqrt()
}

fn grid_of(model: &FsiModel, field: &str) -> (&'static str, Vec<usize>, f64, [f64; 3]) {
    let f = &model.problem.fluid;
    let g = &model.problem.solid.grid;
    match field {
        "solid_displacement" => ("solid_nodes", g.node_shape().to_vec(), g.element_size_m, g.origin_m),
        "von_mises" | "solid_strain" | "solid_density" | "removed_density" | "modifier_intensity" => (
            "solid_voxels",
            g.shape.to_vec(),
            g.element_size_m,
            g.origin_m.map(|o| o + 0.5 * g.element_size_m),
        ),
        _ => ("lattice", f.shape.to_vec(), f.spacing_m, f.origin_m.map(|o| o + 0.5 * f.spacing_m)),
    }
}

fn describe(field: &str) -> (&'static str, &'static str, FieldKind, PaletteHint) {
    match field {
        "speed" => ("Flow speed", "m/s", FieldKind::Scalar, PaletteHint::Sequential),
        "pressure" => ("Gauge pressure", "Pa", FieldKind::Scalar, PaletteHint::Diverging),
        "occupancy" => ("Solid occupancy", "1", FieldKind::Occupancy, PaletteHint::Sequential),
        "solid_displacement" => ("Solid displacement", "m", FieldKind::Displacement, PaletteHint::Sequential),
        "solid_strain" => ("Equivalent strain", "1", FieldKind::Scalar, PaletteHint::Sequential),
        "solid_density" => ("Solid density", "1", FieldKind::Occupancy, PaletteHint::Sequential),
        "removed_density" => ("Removed material", "1", FieldKind::Scalar, PaletteHint::Sequential),
        "modifier_intensity" => ("Removal-zone modifier", "1", FieldKind::Scalar, PaletteHint::Sequential),
        _ => ("von Mises stress", "Pa", FieldKind::Scalar, PaletteHint::Sequential),
    }
}

struct Sampler<'a, 'm, B: SolidView<'m>> {
    native_lifetime: std::marker::PhantomData<&'m ()>,
    model: &'a FsiModel,
    stepper: &'a MultirateStepper<FluidField, B>,
    design: &'a [f64],
    c: f64,
}

impl<'m,B: SolidView<'m>> Sampler<'_, 'm, B> {
    fn values(
        &self,
        z: &[f64],
        names: &[String],
        p: StepParameters<'_>,
    ) -> CaeResult<Vec<(String, Vec<f64>)>> {
        let model = self.model;
        let layout = self.stepper.layout();
        let b = self.stepper.field_b();
        let history = b.solid_core().history(p)?;
        let hl = history.layout;
        let (rho_e, theta_e) = history.design();
        let f = &model.problem.fluid;
        let zb = b.solid_core().to_physical(&z[layout.field_b.clone()]);
        let za = &z[layout.field_a.clone()];
        let u = &zb[hl.u()..hl.u() + hl.n3];
        let needs_fluid = names.iter().any(|x| x == "speed" || x == "pressure");
        let (rho, vel) = if needs_fluid {
            self.stepper.field_a().inner().macroscopic(za)
        } else {
            (Vec::new(), Vec::new())
        };
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            let v = match name.as_str() {
                "speed" => {
                    vel.iter().map(|v| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt() * self.c).collect()
                }
                "pressure" => rho
                    .iter()
                    .map(|r| if *r > 0.0 { (r - 1.0) * f.density_kg_m3 * self.c * self.c / 3.0 } else { 0.0 })
                    .collect(),
                "occupancy" => occupancy_field(model, u, self.design)?,
                "solid_displacement" => u.to_vec(),
                "removed_density" => {
                    let n = model.problem.solid.grid.voxel_count();
                    match &model.problem.design.removal {
                        None => vec![0.0; n],
                        Some(r) => {
                            let mut per_voxel = vec![0.0_f64; n];
                            for (e, &voxel) in model.kuhn.owner.iter().enumerate() {
                                per_voxel[voxel] = per_voxel[voxel].max(r.reference[voxel] - rho_e[e]);
                            }
                            per_voxel
                        }
                    }
                }
                "modifier_intensity" => match &model.modifier {
                    None => vec![0.0; model.problem.solid.grid.voxel_count()],
                    Some(m) => m.intensity(&[rho_e.to_vec(), theta_e.to_vec()].concat()),
                },
                "solid_density" => {
                    let mut per_voxel = vec![0.0_f64; model.problem.solid.grid.voxel_count()];
                    for (e, &voxel) in model.kuhn.owner.iter().enumerate() {
                        per_voxel[voxel] = per_voxel[voxel].max(rho_e[e]);
                    }
                    per_voxel
                }
                "solid_strain" => {
                    let pres = &zb[hl.p()..hl.p() + hl.np];
                    let mut per_voxel = vec![0.0_f64; model.problem.solid.grid.voxel_count()];
                    for (e, &voxel) in model.kuhn.owner.iter().enumerate().take(model.soft.ne()) {
                        let d = model.soft.gather(e, u, pres);
                        let ue: [[f64; 3]; 4] =
                            core::array::from_fn(|a| core::array::from_fn(|i| d[3 * a + i]));
                        let f = model.soft.deformation_gradient(e, &ue);
                        per_voxel[voxel] = per_voxel[voxel].max(equivalent_strain(&f));
                    }
                    per_voxel
                }
                _ => {
                    let pres = &zb[hl.p()..hl.p() + hl.np];
                    let owner = &model.kuhn.owner;
                    let mut per_voxel = vec![0.0_f64; model.problem.solid.grid.voxel_count()];
                    for e in 0..model.soft.ne() {
                        let d = model.soft.gather(e, u, pres);
                        let mut hq = [0.0; 6];
                        if hl.ne_visc > 0 {
                            for i in 0..hl.nb {
                                let off = hl.q() + 6 * (e * hl.nb + i);
                                for (m, h) in hq.iter_mut().enumerate() {
                                    *h += zb[off + m];
                                }
                            }
                        }
                        let (s, _, _) = model.soft.element_stress(e, &d, rho_e[e], theta_e[e], &hq, 1.0);
                        let v = &mut per_voxel[owner[e]];
                        *v = v.max(von_mises(&s));
                    }
                    per_voxel
                }
            };
            out.push((name.clone(), v));
        }
        Ok(out)
    }
}

fn series_unit(model: &FsiModel, name: &str) -> &'static str {
    let kind =
        model.problem.observables.kinds.iter().find(|(n, _)| n == name).map_or("sample", |(_, k)| k.as_str());
    crate::problem::observables::unit_of(kind)
}

fn declaration(model: &FsiModel, result: &DynamicResult, phases: &[f64]) -> Value {
    let mut fields = Map::new();
    for name in &model.problem.frames.fields {
        let (label, unit, kind, palette) = describe(name);
        let (grid, _, spacing, origin) = grid_of(model, name);
        fields.insert(
            name.clone(),
            json!({"label": label, "unit": unit, "kind": kind.as_str(), "palette": palette.as_str(),
                "grid": {"name": grid, "spacing": vec![spacing; 3], "origin": origin, "unit": "m"}}),
        );
    }
    let series: Map<String, Value> = result
        .sample_names
        .iter()
        .map(|n| (n.clone(), json!({"label": n, "unit": series_unit(model, n), "role": "response"})))
        .collect();
    json!({"period": result.period_s, "time_unit": "s", "frame_phases": phases, "fields": fields, "series": series})
}


pub fn capture<'m,B: SolidView<'m>>(
    model: &FsiModel,
    stepper: &MultirateStepper<FluidField, B>,
    design: &[f64],
    result: &DynamicResult,
) -> CaeResult<(BTreeMap<String, FieldValue>, Value)> {
    let spec = &model.problem.frames;
    let mut fields = BTreeMap::new();
    let rows = result.samples.nrows;
    if spec.count == 0 || rows == 0 {
        return Ok((fields, json!({"frames": 0})));
    }
    let count = spec.count.min(rows);
    let per_field = |name: &str| -> u64 {
        let (_, shape, _, _) = grid_of(model, name);
        let comps = if name == "solid_displacement" { 3 } else { 1 };
        8 * (shape.iter().product::<usize>() * comps) as u64
    };
    let per_frame: u64 = spec.fields.iter().map(|k| per_field(k)).sum();
    let fit = spec
        .byte_limit
        .checked_div(per_frame)
        .map_or(count, |f| usize::try_from(f).map_or(count, |f| f.min(count)));
    let targets: Vec<usize> = (0..fit).map(|k| ((k + 1) * rows).div_ceil(count).max(1)).collect();
    let p = StepParameters { design, time_scale: result.time_scale };
    let dt_f = result.step_s / model.problem.coupling.substeps as f64;
    let sampler = Sampler { native_lifetime: std::marker::PhantomData, model, stepper, design, c: model.problem.fluid.spacing_m / dt_f };
    let mut z = result.start_state.clone();
    let mut times = Vec::with_capacity(fit);
    let mut k = 0usize;
    for step in 1..=rows {
        if k >= targets.len() {
            break;
        }
        let rec = stepper.advance(result.first_step + step - 1, &z, p)?;
        z = rec.state;
        if step != targets[k] {
            continue;
        }
        times.push(step as f64 * result.step_s);
        for (name, values) in sampler.values(&z, &spec.fields, p)? {
            let (_, mut shape, _, _) = grid_of(model, &name);
            if name == "solid_displacement" {
                shape.push(3);
            }
            fields.insert(format!("frame_{k}_{name}"), array(values, &shape)?);
        }
        k += 1;
    }
    let n = times.len();
    let phases: Vec<f64> = times.iter().map(|t| t / result.period_s).collect();
    fields.insert("frame_times_s".into(), array(times, &[n])?);
    let record = json!({"frames": n, "requested": spec.count, "fields": spec.fields,
        "bytes_per_frame": per_frame, "byte_limit": spec.byte_limit,
        "truncated_by_byte_limit": fit < count,
        "declaration": declaration(model, result, &phases),
        "note": "deformed configurations are result renders of the pushed-forward occupancy, never the design"});
    Ok((fields, record))
}

fn manifest(model: &FsiModel, result: &DynamicResult) -> FrameManifest {
    let spec = &model.problem.frames;
    let mut grids: Vec<GridSpec> = Vec::new();
    let mut fields = Vec::new();
    for name in &spec.fields {
        let (grid, shape, spacing, origin) = grid_of(model, name);
        if !grids.iter().any(|g| g.name == grid) {
            grids.push(GridSpec {
                name: grid.into(),
                shape,
                spacing: vec![spacing; 3],
                origin: origin.to_vec(),
                unit: "m".into(),
            });
        }
        let (label, unit, kind, palette) = describe(name);
        fields.push(FieldSpec {
            name: name.clone(),
            label: label.into(),
            unit: unit.into(),
            grid: grid.into(),
            kind,
            palette,
        });
    }
    let series = result
        .sample_names
        .iter()
        .map(|n| SeriesSpec {
            name: n.clone(),
            label: n.clone(),
            unit: series_unit(model, n).into(),
            role: "response".into(),
        })
        .collect();
    let mut provenance = Map::new();
    provenance.insert("provider".into(), json!(crate::provider::NAME));
    provenance.insert("problem_identity".into(), json!(model.problem.identity()));
    provenance.insert("regime".into(), json!(result.regime));
    FrameManifest {
        grids,
        fields,
        series,
        time: TimeBase {
            unit: "s".into(),
            step: result.step_s,
            period: model.problem.time.periodic().then_some(result.period_s),
            phase_origin: 0.0,
        },
        retention: Retention {
            byte_limit: spec.byte_limit,
            phase_bins: spec.count.max(12),
            retain_segments: 4,
            segment_length: None,
            precision: Precision::F32,
        },
        provenance,
        meshes: Vec::new(),
    }
}


pub fn stream<'m,B: SolidView<'m>>(
    model: &FsiModel,
    stepper: &MultirateStepper<FluidField, B>,
    design: &[f64],
    result: &DynamicResult,
    named: Option<&implexity_optim::design::NamedArrays>,
) -> CaeResult<Option<Value>> {
    if model.problem.frames.fields.is_empty() || result.samples.nrows == 0 {
        return Ok(None);
    }

    let writer = match named {
        Some(d) => capture::begin_for_design(manifest(model, result), crate::provider::NAME, d)?,
        None => capture::begin(manifest(model, result), crate::provider::NAME)?,
    };
    let Some(mut writer) = writer else {
        return Ok(None);
    };
    let p = StepParameters { design, time_scale: result.time_scale };
    let dt_f = result.step_s / model.problem.coupling.substeps as f64;
    let sampler = Sampler { native_lifetime: std::marker::PhantomData, model, stepper, design, c: model.problem.fluid.spacing_m / dt_f };
    let names = &model.problem.frames.fields;
    let mut z = result.start_state.clone();
    let mut run = || -> CaeResult<()> {
        for step in 1..=result.samples.nrows {
            let rec = stepper.advance(result.first_step + step - 1, &z, p)?;
            z = rec.state;
            let t = step as f64 * result.step_s;
            writer.push_series(t, &rec.samples)?;
            if writer.wants_frame(t) {
                let values = sampler.values(&z, names, p)?;
                let refs: Vec<(&str, &[f64])> =
                    values.iter().map(|(n, v)| (n.as_str(), v.as_slice())).collect();
                writer.push_frame(t, None, &refs)?;
            }
        }
        Ok(())
    };
    let outcome = run();
    let summary = writer.finish(outcome.is_ok())?;
    outcome?;
    Ok(Some(json!({"dir": summary.dir.display().to_string(), "status": summary.status})))
}

