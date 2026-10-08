// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use serde_json::{Map, Value, json};

use implexity_ad::Scalar;
use implexity_core::orchestration::AddInCategory;
use implexity_core::packages::InstallContext;
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_physics_base::core_bridge::{ComponentOptions, PhysicsComponent, register_strict_component};
use implexity_solve::nonmatching_interface::{InterfaceProjection, Values};

fn fail<T>(message: impl Into<String>) -> CaeResult<T> {
    Err(CaeError::contract(message))
}

#[derive(Debug, Clone, Copy)]
pub enum Samples<'a> {
    Scalar(f64),
    Vector(&'a [f64]),
}

impl From<f64> for Samples<'_> {
    fn from(v: f64) -> Self {
        Self::Scalar(v)
    }
}

impl<'a> From<&'a [f64]> for Samples<'a> {
    fn from(v: &'a [f64]) -> Self {
        Self::Vector(v)
    }
}

fn vector(
    value: Samples<'_>,
    size: usize,
    name: &str,
    positive: bool,
    nonnegative: bool,
) -> CaeResult<Vec<f64>> {
    let a = match value {
        Samples::Scalar(v) => vec![v; size],
        Samples::Vector(v) => v.to_vec(),
    };
    if a.len() != size || a.iter().any(|v| !v.is_finite()) {
        return fail(format!("{name}: finite scalar or vector of size {size} required"));
    }
    if (positive && a.iter().any(|v| *v <= 0.0)) || (nonnegative && a.iter().any(|v| *v < 0.0)) {
        return fail(format!("{name}: invalid sign"));
    }
    Ok(a)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThermalContactResidual {
    pub left_outward_power_w: Vec<f64>,
    pub right_outward_power_w: Vec<f64>,
    pub temperature_jump_residual_k: Vec<f64>,
}


#[derive(Debug, Clone, Copy, Default)]
pub struct ConservativeThermalContact;

pub const THERMAL_CONTACT_LIMITATIONS: [&str; 3] = [
    "Requires a shared host field solve and a flux state per interface sample.",
    "Trace rank / inf-sup stability and interface geometry are owned by the host discretisation.",
    "Not a fluid momentum, coolant energy, boiling or CHF solver.",
];

pub const FLUID_TRACTION_LIMITATIONS: [&str; 3] = [
    "Host must supply solved fluid pressure/shear and actual interface geometry.",
    "No pressure reconstruction from pumping pressure drop alone.",
    "Reciprocal force transfer is not moving-domain FSI or a fluid solver.",
];

fn scalar_values(v: &Values) -> CaeResult<Vec<f64>> {
    if v.components != 1 {
        return fail("interface temperature must be scalar");
    }
    Ok(v.data.clone())
}

impl ConservativeThermalContact {
    pub const COMPONENT_KIND: &'static str = "thermal_interface_residual";
    pub const IMPLEMENTATION: &'static str =
        "implexity.physics_library.conservative_interfaces.ConservativeThermalContact";

    pub fn eliminated_conductive_flux<S: Scalar>(left: S, right: S, areal_resistance: S) -> S {
        (left - right) / areal_resistance
    }


    pub fn residual(
        &self,
        projection: &InterfaceProjection,
        left_temperature: &[f64],
        right_temperature: &[f64],
        heat_flux: Samples<'_>,
        resistance: Samples<'_>,
    ) -> CaeResult<ThermalContactResidual> {
        let n = projection.interface_measure().len();
        let q = vector(heat_flux, n, "heat flux", false, false)?;
        let r = vector(resistance, n, "thermal resistance", false, true)?;
        let jump = projection
            .jump(&Values::scalar(left_temperature.to_vec()), &Values::scalar(right_temperature.to_vec()))?;
        let jump = scalar_values(&jump)?;
        if left_temperature.iter().chain(right_temperature).any(|t| *t <= 0.0) {
            return fail("absolute interface temperatures must be positive");
        }
        let (left, right) = projection.conservative_exchange(&Values::scalar(q.clone()))?;
        Ok(ThermalContactResidual {
            left_outward_power_w: left.data,
            right_outward_power_w: right.data,
            temperature_jump_residual_k: jump.iter().zip(&r).zip(&q).map(|((j, r), q)| j - r * q).collect(),
        })
    }


    pub fn jacobian(
        &self,
        projection: &InterfaceProjection,
        resistance: Samples<'_>,
    ) -> CaeResult<CsrMatrix> {
        let l = projection.left_to_interface();
        let r = projection.right_to_interface();
        let m = projection.interface_measure();
        let n = l.nrows();
        let res = vector(resistance, n, "thermal resistance", false, true)?;
        let (nl, nr) = (l.ncols(), r.ncols());
        let (mut rows, mut cols, mut vals) = (Vec::new(), Vec::new(), Vec::new());
        for i in 0..n {
            let (idx, val) = l.row(i);
            for (j, v) in idx.iter().zip(val) {

                rows.push(*j);
                cols.push(nl + nr + i);
                vals.push(v * m[i]);
                rows.push(nl + nr + i);
                cols.push(*j);
                vals.push(*v);
            }
            let (idx, val) = r.row(i);
            for (j, v) in idx.iter().zip(val) {
                rows.push(nl + j);
                cols.push(nl + nr + i);
                vals.push(-v * m[i]);
                rows.push(nl + nr + i);
                cols.push(nl + j);
                vals.push(-v);
            }
            rows.push(nl + nr + i);
            cols.push(nl + nr + i);
            vals.push(-res[i]);
        }
        let size = nl + nr + n;
        CsrMatrix::from_triplets(size, size, &rows, &cols, &vals)
            .map_err(|e| CaeError::contract(e.to_string()))
    }


    #[allow(clippy::too_many_arguments)]
    pub fn jvp(
        &self,
        projection: &InterfaceProjection,
        left_temperature: &[f64],
        right_temperature: &[f64],
        heat_flux: Samples<'_>,
        resistance: Samples<'_>,
        dleft_temperature: &[f64],
        dright_temperature: &[f64],
        dheat_flux: Samples<'_>,
        dresistance: Samples<'_>,
        dleft: Option<&CsrMatrix>,
        dright: Option<&CsrMatrix>,
        dmeasure: Option<&[f64]>,
    ) -> CaeResult<ThermalContactResidual> {
        self.residual(projection, left_temperature, right_temperature, heat_flux, resistance)?;
        let n = projection.interface_measure().len();
        let q = vector(heat_flux, n, "heat flux", false, false)?;
        let dq = vector(dheat_flux, n, "heat flux derivative", false, false)?;
        let r = vector(resistance, n, "thermal resistance", false, true)?;
        let dr = vector(dresistance, n, "resistance derivative", false, false)?;
        let (fl, fr) = projection.exchange_jvp(
            &Values::scalar(q.clone()),
            &Values::scalar(dq.clone()),
            dleft,
            dright,
            dmeasure,
        )?;
        let jump = projection.jump(
            &Values::scalar(dleft_temperature.to_vec()),
            &Values::scalar(dright_temperature.to_vec()),
        )?;
        if jump.components != 1 || jump.count != n {
            return fail("interface temperature derivative must be scalar");
        }
        let mut jump = jump.data;
        let lin = |e: implexity_linalg::LinalgError| CaeError::contract(e.to_string());
        if let Some(d) = dleft {
            for (j, v) in jump.iter_mut().zip(d.matvec(left_temperature).map_err(lin)?) {
                *j += v;
            }
        }
        if let Some(d) = dright {
            for (j, v) in jump.iter_mut().zip(d.matvec(right_temperature).map_err(lin)?) {
                *j -= v;
            }
        }
        Ok(ThermalContactResidual {
            left_outward_power_w: fl.data,
            right_outward_power_w: fr.data,
            temperature_jump_residual_k: (0..n).map(|i| jump[i] - dr[i] * q[i] - r[i] * dq[i]).collect(),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TractionLoads {
    pub fluid_absolute_pressure_pa: Vec<f64>,
    pub differential_wall_pressure_pa: Vec<f64>,
    pub traction_on_solid_pa: Vec<[f64; 3]>,
    pub solid_load_n: Values,
    pub fluid_reaction_n: Values,
}

#[derive(Debug, Clone, Copy)]
pub struct TractionInputs<'a> {
    pub pressure_gauge_pa: Samples<'a>,
    pub pressure_reference_absolute_pa: Samples<'a>,
    pub exterior_pressure_absolute_pa: Samples<'a>,
    pub solid_outward_normals: &'a [[f64; 3]],
    pub viscous_traction_pa: &'a [[f64; 3]],
}

#[derive(Debug, Clone, Copy)]
pub struct TractionTangents<'a> {
    pub dpressure_gauge_pa: Samples<'a>,
    pub dpressure_reference_absolute_pa: Samples<'a>,
    pub dexterior_pressure_absolute_pa: Samples<'a>,
    pub dnormals: Option<&'a [[f64; 3]]>,
    pub dviscous_traction_pa: Option<&'a [[f64; 3]]>,
    pub dleft: Option<&'a CsrMatrix>,
    pub dright: Option<&'a CsrMatrix>,
    pub dmeasure: Option<&'a [f64]>,
}

impl Default for TractionTangents<'_> {
    fn default() -> Self {
        Self {
            dpressure_gauge_pa: Samples::Scalar(0.0),
            dpressure_reference_absolute_pa: Samples::Scalar(0.0),
            dexterior_pressure_absolute_pa: Samples::Scalar(0.0),
            dnormals: None,
            dviscous_traction_pa: None,
            dleft: None,
            dright: None,
            dmeasure: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ConservativeFluidTraction;

type Checked = (Vec<f64>, Vec<f64>, Vec<[f64; 3]>, Vec<[f64; 3]>);

impl ConservativeFluidTraction {
    pub const COMPONENT_KIND: &'static str = "fluid_structure_interface_load";
    pub const IMPLEMENTATION: &'static str =
        "implexity.physics_library.conservative_interfaces.ConservativeFluidTraction";

    pub fn traction<S: Scalar>(
        pressure_absolute: S,
        solid_outward_normal: &[S; 3],
        viscous_traction: &[S; 3],
    ) -> [S; 3] {
        std::array::from_fn(|a| -(pressure_absolute * solid_outward_normal[a]) + viscous_traction[a])
    }

    fn inputs(projection: &InterfaceProjection, i: &TractionInputs<'_>) -> CaeResult<Checked> {
        let n = projection.interface_measure().len();
        let gauge = vector(i.pressure_gauge_pa, n, "gauge pressure", false, false)?;
        let reference =
            vector(i.pressure_reference_absolute_pa, n, "absolute pressure reference", false, true)?;
        let exterior = vector(i.exterior_pressure_absolute_pa, n, "absolute external pressure", false, true)?;
        let absolute: Vec<f64> = gauge.iter().zip(&reference).map(|(g, r)| g + r).collect();
        if absolute.iter().any(|p| *p <= 0.0) {
            return fail("fluid absolute pressure must be positive");
        }
        let finite = |v: &[[f64; 3]]| v.iter().flatten().all(|x| x.is_finite());
        if i.solid_outward_normals.len() != n
            || i.viscous_traction_pa.len() != n
            || !finite(i.solid_outward_normals)
            || !finite(i.viscous_traction_pa)
        {
            return fail("finite normals and viscous traction with shape (interface samples,3) required");
        }
        if i.solid_outward_normals
            .iter()
            .any(|v| ((v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt() - 1.0).abs() > 1e-10)
        {
            return fail("solid-outward normals must have unit length; do not include area in normals");
        }
        let difference = absolute.iter().zip(&exterior).map(|(a, e)| a - e).collect();
        Ok((absolute, difference, i.solid_outward_normals.to_vec(), i.viscous_traction_pa.to_vec()))
    }

    fn vector_values(v: &[[f64; 3]]) -> Values {
        Values { count: v.len(), components: 3, data: v.iter().flatten().copied().collect() }
    }


    pub fn loads(
        &self,
        projection: &InterfaceProjection,
        inputs: &TractionInputs<'_>,
    ) -> CaeResult<TractionLoads> {
        let (absolute, pressure, normals, shear) = Self::inputs(projection, inputs)?;
        let traction: Vec<[f64; 3]> = (0..absolute.len())
            .map(|i| std::array::from_fn(|a| -absolute[i] * normals[i][a] + shear[i][a]))
            .collect();
        let (solid, fluid) = projection.conservative_exchange(&Self::vector_values(&traction))?;
        Ok(TractionLoads {
            fluid_absolute_pressure_pa: absolute,
            differential_wall_pressure_pa: pressure,
            traction_on_solid_pa: traction,
            solid_load_n: solid,
            fluid_reaction_n: fluid,
        })
    }


    pub fn jvp(
        &self,
        projection: &InterfaceProjection,
        inputs: &TractionInputs<'_>,
        tangents: &TractionTangents<'_>,
    ) -> CaeResult<TractionLoads> {
        let n = projection.interface_measure().len();
        let (absolute, _, normals, shear) = Self::inputs(projection, inputs)?;
        let dg = vector(tangents.dpressure_gauge_pa, n, "gauge derivative", false, false)?;
        let dref = vector(tangents.dpressure_reference_absolute_pa, n, "reference derivative", false, false)?;
        let dext =
            vector(tangents.dexterior_pressure_absolute_pa, n, "external pressure derivative", false, false)?;
        let dp_abs: Vec<f64> = dg.iter().zip(&dref).map(|(a, b)| a + b).collect();
        let dp: Vec<f64> = dp_abs.iter().zip(&dext).map(|(a, b)| a - b).collect();
        let dn = tangents.dnormals.map_or_else(|| vec![[0.0; 3]; n], <[[f64; 3]]>::to_vec);
        let ds = tangents.dviscous_traction_pa.map_or_else(|| vec![[0.0; 3]; n], <[[f64; 3]]>::to_vec);
        let finite = |v: &[[f64; 3]]| v.iter().flatten().all(|x| x.is_finite());
        if dn.len() != n || ds.len() != n || !finite(&dn) || !finite(&ds) {
            return fail("invalid normal/viscous traction derivative");
        }
        if dn.iter().zip(&normals).any(|(d, v)| (d[0] * v[0] + d[1] * v[1] + d[2] * v[2]).abs() > 1e-10) {
            return fail("normal derivative must be tangent to the unit sphere");
        }
        let traction: Vec<[f64; 3]> =
            (0..n).map(|i| std::array::from_fn(|a| -absolute[i] * normals[i][a] + shear[i][a])).collect();
        let dt: Vec<[f64; 3]> = (0..n)
            .map(|i| std::array::from_fn(|a| -dp_abs[i] * normals[i][a] - absolute[i] * dn[i][a] + ds[i][a]))
            .collect();
        let (solid, fluid) = projection.exchange_jvp(
            &Self::vector_values(&traction),
            &Self::vector_values(&dt),
            tangents.dleft,
            tangents.dright,
            tangents.dmeasure,
        )?;
        Ok(TractionLoads {
            fluid_absolute_pressure_pa: dp_abs,
            differential_wall_pressure_pa: dp,
            traction_on_solid_pa: dt,
            solid_load_n: solid,
            fluid_reaction_n: fluid,
        })
    }
}

fn support(limitations: &[&str]) -> Map<String, Value> {
    json!({"status": "field_component", "history": false, "limitations": limitations})
        .as_object()
        .cloned()
        .unwrap_or_default()
}

impl PhysicsComponent for ConservativeThermalContact {
    fn implementation(&self) -> String {
        Self::IMPLEMENTATION.into()
    }
    fn component_kind(&self) -> Option<String> {
        Some(Self::COMPONENT_KIND.into())
    }
    fn runtime_support(&self) -> Option<Map<String, Value>> {
        Some(support(&THERMAL_CONTACT_LIMITATIONS))
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl PhysicsComponent for ConservativeFluidTraction {
    fn implementation(&self) -> String {
        Self::IMPLEMENTATION.into()
    }
    fn component_kind(&self) -> Option<String> {
        Some(Self::COMPONENT_KIND.into())
    }
    fn runtime_support(&self) -> Option<Map<String, Value>> {
        Some(support(&FLUID_TRACTION_LIMITATIONS))
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}


pub fn register_components(ctx: &InstallContext<'_>) -> CaeResult<()> {
    let rows: [(&str, Arc<dyn PhysicsComponent>, &str, &[&str]); 2] = [
        (
            "conservative_thermal_contact",
            Arc::new(ConservativeThermalContact),
            ConservativeThermalContact::COMPONENT_KIND,
            &THERMAL_CONTACT_LIMITATIONS,
        ),
        (
            "conservative_fluid_traction",
            Arc::new(ConservativeFluidTraction),
            ConservativeFluidTraction::COMPONENT_KIND,
            &FLUID_TRACTION_LIMITATIONS,
        ),
    ];
    for (name, component, quantity, notes) in rows {
        register_strict_component(
            ctx,
            name,
            component,
            quantity,
            &ComponentOptions {
                category: AddInCategory::Interface,
                domain: "interface".into(),
                notes: notes.iter().map(|s| (*s).to_string()).collect(),
                ..ComponentOptions::default()
            },
        )?;
    }
    Ok(())
}
