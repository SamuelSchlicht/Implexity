// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::error::{GResult, GeometryError, model_err};
use crate::fieldclass::FieldClass;
use crate::kinds::{StaticInfo, entry, kind_info};
use crate::lattice::config::{Channels, LatticeConfig};
use crate::lattice::controls::{CONTROL_SCHEMA, Controls, initial_controls, validate_control};
use crate::lattice::freeze::FrozenGeometry;
use crate::lattice::morphology::Opening;
use crate::lattice::numerics::{Grids, build_grids, trilerp_at, trilerp_const};
use crate::lattice::synth::{FieldAdjoints, SynthOptions, SynthState, synthesize};
use crate::node::{
    Attr, ConstructArgs, EvalCtx, Kernel, KernelBox, KernelInputs, KindInfo, Node, NodeOp, ParamSpec,
    Prepared,
};
use crate::occupancy::{CellFields, DerivedGrid, OccupancySource};
use crate::pyfmt::{PyObj, float_repr, str_repr};
use crate::scalar::Scalar;
use crate::value::{NdArray, ParamValue};

pub(crate) const MODULE: &str = "implexity.implicit.lattice.node";

#[derive(Clone, Debug, PartialEq)]
pub struct FixedRegion {
    pub id: String,
    pub lower_mm: [f64; 3],
    pub upper_mm: [f64; 3],
    pub occupancy: f64,
    pub phase_fraction: Option<f64>,
}

impl FixedRegion {
    fn py_obj(&self) -> PyObj {
        PyObj::Tuple(vec![
            PyObj::Str(self.id.clone()),
            PyObj::float_tuple(&self.lower_mm),
            PyObj::float_tuple(&self.upper_mm),
            PyObj::Float(self.occupancy),
            self.phase_fraction.map_or(PyObj::None, PyObj::Float),
        ])
    }

    fn doc_attr(&self) -> Attr {
        let mut d = vec![
            ("id".into(), Attr::Str(self.id.clone())),
            ("lower_mm".into(), floats_attr(&self.lower_mm)),
            ("upper_mm".into(), floats_attr(&self.upper_mm)),
            ("occupancy".into(), Attr::Float(self.occupancy)),
        ];
        if let Some(p) = self.phase_fraction {
            d.push(("phase_fraction".into(), Attr::Float(p)));
        }
        Attr::Dict(d)
    }

    fn contains_cell(&self, p: [f64; 3]) -> bool {
        (0..3).all(|a| p[a] >= self.lower_mm[a] && p[a] < self.upper_mm[a])
    }
}

pub(crate) fn floats_attr(v: &[f64]) -> Attr {
    Attr::List(v.iter().map(|x| Attr::Float(*x)).collect())
}

pub(crate) fn ints_attr(v: &[usize]) -> Attr {
    #[allow(clippy::cast_possible_wrap)]
    Attr::List(v.iter().map(|x| Attr::Int(*x as i64)).collect())
}

pub(crate) fn int_tuple(v: &[usize]) -> PyObj {
    #[allow(clippy::cast_possible_wrap)]
    PyObj::Tuple(v.iter().map(|x| PyObj::Int(*x as i64)).collect())
}

pub(crate) fn py_float(a: &Attr) -> GResult<f64> {
    match a {
        Attr::Int(i) => {
            #[allow(clippy::cast_precision_loss)]
            let v = *i as f64;
            Ok(v)
        }
        Attr::Float(f) => Ok(*f),
        Attr::Bool(b) => Ok(f64::from(u8::from(*b))),
        Attr::Str(s) => {
            let t = s.trim();
            let lower = t.to_ascii_lowercase();
            match lower.as_str() {
                "nan" | "+nan" | "-nan" => Ok(f64::NAN),
                "inf" | "+inf" | "infinity" | "+infinity" => Ok(f64::INFINITY),
                "-inf" | "-infinity" => Ok(f64::NEG_INFINITY),
                _ => t.replace('_', "").parse::<f64>().map_err(|_| {
                    GeometryError::Value(format!("could not convert string to float: {}", str_repr(s)))
                }),
            }
        }
        Attr::Array(arr) if arr.size() == 1 => Ok(arr.to_f64_vec()[0]),
        Attr::Null => Err(GeometryError::Value(
            "float() argument must be a string or a real number, not 'NoneType'".into(),
        )),
        _ => Err(GeometryError::Value(format!(
            "float() argument must be a string or a real number, not {}",
            str_repr(&a.py_obj().repr())
        ))),
    }
}

pub(crate) fn float_seq(a: &Attr) -> GResult<Vec<f64>> {
    match a {
        Attr::List(v) => v.iter().map(py_float).collect(),
        Attr::Array(arr) => Ok(arr.to_f64_vec()),
        Attr::Null => Err(GeometryError::Value("'NoneType' object is not iterable".into())),
        other => Err(GeometryError::Value(format!("{} is not iterable", other.py_obj().repr()))),
    }
}

pub(crate) fn parse_grid(a: &Attr, name: &str) -> GResult<[usize; 3]> {
    let bad = || GeometryError::Model(format!("{name} must contain three integer counts >=2"));
    let vals: Vec<Attr> = match a {
        Attr::List(v) => v.clone(),
        Attr::Array(arr) => arr.to_f64_vec().into_iter().map(Attr::Float).collect(),
        _ => return Err(bad()),
    };
    if vals.len() != 3 {
        return Err(bad());
    }
    let mut out = [0usize; 3];
    for (o, v) in out.iter_mut().zip(&vals) {
        let n = match v {
            Attr::Int(i) => *i,
            #[allow(clippy::cast_possible_truncation)]
            Attr::Float(f) if f.fract() == 0.0 && f.is_finite() => *f as i64,
            _ => return Err(bad()),
        };
        if n < 2 {
            return Err(bad());
        }
        *o = usize::try_from(n).map_err(|_| bad())?;
    }
    Ok(out)
}

pub(crate) fn parse_regions(a: Option<&Attr>) -> GResult<Vec<FixedRegion>> {
    let items: Vec<Attr> = match a {
        None => Vec::new(),
        Some(Attr::List(v)) => v.clone(),
        Some(Attr::Null) => return Err(GeometryError::Value("'NoneType' object is not iterable".into())),
        Some(other) => {
            return Err(GeometryError::Value(format!("{} is not iterable", other.py_obj().repr())));
        }
    };
    let mut result: Vec<FixedRegion> = Vec::new();
    let allowed = ["id", "lower_mm", "upper_mm", "occupancy", "phase_fraction"];
    for r in &items {
        let Attr::Dict(m) = r else {
            return model_err(
                "fixed region must be an explicit named axis-aligned box with occupancy and optional phase fraction",
            );
        };
        if m.iter().any(|(k, _)| !allowed.contains(&k.as_str())) {
            return model_err(
                "fixed region must be an explicit named axis-aligned box with occupancy and optional phase fraction",
            );
        }
        let get = |k: &str| m.iter().find(|(n, _)| n == k).map(|(_, v)| v);
        let name = match get("id") {
            None => String::new(),
            Some(Attr::Str(s)) => s.clone(),
            Some(Attr::Float(f)) => float_repr(*f),
            Some(other) => other.py_obj().repr(),
        };
        if name.is_empty() || result.iter().any(|x| x.id == name) {
            return model_err("fixed region ids must be nonempty and unique");
        }
        let lo = get("lower_mm").map_or(Ok(Vec::new()), float_seq)?;
        let hi = get("upper_mm").map_or(Ok(Vec::new()), float_seq)?;
        let rho = get("occupancy").map_or(Ok(f64::NAN), py_float)?;
        let phase = match get("phase_fraction") {
            None | Some(Attr::Null) => None,
            Some(p) => Some(p),
        };
        let ordered = lo.len() == 3
            && hi.len() == 3
            && lo.iter().chain(&hi).all(|x| x.is_finite())
            && lo.iter().zip(&hi).all(|(a, b)| b > a);
        if !ordered || (rho != 0.0 && rho != 1.0) {
            return model_err("fixed region requires ordered finite bounds and occupancy 0 or 1");
        }
        let phase = match phase {
            None => None,
            Some(p) => {
                let v = py_float(p)?;
                if !v.is_finite() || !(0.0..=1.0).contains(&v) {
                    return model_err("fixed neutral phase must be in [0,1]");
                }
                Some(v)
            }
        };
        let lower_mm = [lo[0], lo[1], lo[2]];
        let upper_mm = [hi[0], hi[1], hi[2]];
        for other in &result {
            let overlap =
                (0..3).all(|a| upper_mm[a].min(other.upper_mm[a]) > lower_mm[a].max(other.lower_mm[a]));
            if overlap {
                return model_err("overlapping fixed regions are ambiguous; partition them explicitly");
            }
        }
        result.push(FixedRegion { id: name, lower_mm, upper_mm, occupancy: rho, phase_fraction: phase });
    }
    Ok(result)
}

#[derive(Clone, Debug, PartialEq)]
pub struct LatticeSpec {
    pub origin_mm: [f64; 3],
    pub domain_mm: [f64; 3],
    pub analysis_grid: [usize; 3],
    pub geometry_grid: Option<[usize; 3]>,
    pub control_grid: [usize; 3],
    pub period_mm: f64,
    pub thickness_range_mm: [f64; 2],
    pub interface_mm: f64,
    pub stretch_limit: f64,
    pub beta_mask: f64,
    pub beta_phase: f64,
    pub beta_topology: f64,
    pub fixed_regions: Vec<FixedRegion>,
    pub frozen_geometry: Option<FrozenGeometry>,
    pub minimum_wall_mm: Option<f64>,
}

pub const LATTICE_STRUCT: [&str; 15] = [
    "control_schema",
    "origin_mm",
    "domain_mm",
    "analysis_grid",
    "geometry_grid",
    "control_grid",
    "period_mm",
    "thickness_range_mm",
    "interface_mm",
    "stretch_limit",
    "beta_mask",
    "beta_phase",
    "beta_topology",
    "fixed_regions",
    "frozen_geometry",
];

pub struct GeometryFields {
    pub rho: Vec<f64>,
    pub phase_fraction: Vec<f64>,
    gate_rho: Vec<f64>,
    gate_phase: Vec<f64>,
    opening: Option<(Opening, Vec<f64>)>,
    state: SynthState,
}

impl GeometryFields {
    #[must_use]
    pub fn state(&self) -> &SynthState {
        &self.state
    }


    pub fn vjp(&self, adj_rho: &[f64], adj_phase: &[f64]) -> GResult<Vec<f64>> {
        let n = self.rho.len();
        if adj_rho.len() != n || adj_phase.len() != n {
            return Err(GeometryError::Value("field adjoint does not match the geometry grid".into()));
        }
        let mut rho: Vec<f64> = adj_rho.iter().zip(&self.gate_rho).map(|(a, g)| a * g).collect();
        if let Some((opening, gate)) = &self.opening {
            rho = opening.transpose(&rho).iter().zip(gate).map(|(a, g)| a * g).collect();
        }
        let phase: Vec<f64> = adj_phase.iter().zip(&self.gate_phase).map(|(a, g)| a * g).collect();
        self.state.vjp(&FieldAdjoints {
            rho: Some(rho),
            phase_fraction: Some(phase),
            ..FieldAdjoints::default()
        })
    }
}

impl LatticeSpec {

    pub fn from_attrs(attrs: &mut BTreeMap<String, Attr>) -> GResult<Self> {
        if let Some(s) = attrs.remove("control_schema")
            && s.as_str() != Some(CONTROL_SCHEMA)
        {
            return model_err("unsupported spatial control schema; explicit migration is required");
        }
        let frozen_geometry = FrozenGeometry::normalise(attrs.remove("frozen_geometry").as_ref())?;
        let fixed_regions = parse_regions(attrs.remove("fixed_regions").as_ref())?;
        let origin = attrs.remove("origin_mm").map_or(Ok(vec![0.0; 3]), |a| float_seq(&a))?;
        let domain = attrs.remove("domain_mm").map_or(Ok(vec![12.0; 3]), |a| float_seq(&a))?;
        let analysis_grid =
            attrs.remove("analysis_grid").map_or(Ok([12, 12, 12]), |a| parse_grid(&a, "analysis_grid"))?;
        let geometry_grid = match attrs.remove("geometry_grid") {
            None | Some(Attr::Null) => None,
            Some(a) => Some(parse_grid(&a, "geometry_grid")?),
        };
        let control_grid =
            attrs.remove("control_grid").map_or(Ok([3, 3, 3]), |a| parse_grid(&a, "control_grid"))?;
        let scalar = |attrs: &mut BTreeMap<String, Attr>, k: &str, d: f64| {
            attrs.remove(k).map_or(Ok(d), |a| py_float(&a))
        };
        let period_mm = scalar(attrs, "period_mm", 4.0)?;
        let thickness = match attrs.remove("thickness_range_mm") {
            None | Some(Attr::Null) => vec![0.05 * period_mm, 0.35 * period_mm],
            Some(a) if !a.truthy() => vec![0.05 * period_mm, 0.35 * period_mm],
            Some(a) => float_seq(&a)?,
        };
        let interface_mm = scalar(attrs, "interface_mm", 0.5)?;
        let stretch_limit = scalar(attrs, "stretch_limit", 0.12)?;
        let beta_mask = scalar(attrs, "beta_mask", 8.0)?;
        let beta_phase = scalar(attrs, "beta_phase", 8.0)?;
        let beta_topology = scalar(attrs, "beta_topology", 8.0)?;
        let minimum_wall = attrs.remove("minimum_wall_mm");
        if origin.len() != 3
            || domain.len() != 3
            || !origin.iter().chain(&domain).all(|v| v.is_finite())
            || domain.iter().copied().fold(f64::INFINITY, f64::min) <= 0.0
        {
            return model_err("origin/domain must be finite triples and domain extents positive");
        }
        let origin_mm = [origin[0], origin[1], origin[2]];
        let domain_mm = [domain[0], domain[1], domain[2]];
        for r in &fixed_regions {
            if (0..3).any(|a| r.lower_mm[a] < origin_mm[a] || r.upper_mm[a] > origin_mm[a] + domain_mm[a]) {
                return model_err(format!("fixed region {} lies outside the declared design volume", r.id));
            }
        }
        let scalars = [period_mm, interface_mm, stretch_limit, beta_mask, beta_phase, beta_topology];
        if !scalars.iter().all(|v| v.is_finite() && *v > 0.0) {
            return model_err(
                "period, interface width, stretch limit and sharpness must be positive finite values",
            );
        }
        if thickness.len() != 2
            || !thickness.iter().all(|v| v.is_finite())
            || !(0.0 < thickness[0] && thickness[0] < thickness[1])
        {
            return model_err("thickness_range_mm must be a strictly ordered positive pair");
        }
        let spec = Self {
            origin_mm,
            domain_mm,
            analysis_grid,
            geometry_grid,
            control_grid,
            period_mm,
            thickness_range_mm: [thickness[0], thickness[1]],
            interface_mm,
            stretch_limit,
            beta_mask,
            beta_phase,
            beta_topology,
            fixed_regions,
            frozen_geometry,
            minimum_wall_mm: None,
        };
        let mut spec = spec;
        let gs = spec.geometry_shape();
        #[allow(clippy::cast_precision_loss)]
        let min_cell = (0..3).map(|a| domain_mm[a] / gs[a] as f64).fold(f64::INFINITY, f64::min);
        if interface_mm < 0.5 * min_cell {
            return model_err(
                "interface_mm is below half a geometry cell; refine geometry_grid explicitly rather than silently change geometry",
            );
        }
        spec.minimum_wall_mm = match minimum_wall {
            None | Some(Attr::Null) => None,
            Some(a) => {
                const MSG: &str = "minimum_wall_mm must be a positive finite length or null";
                if matches!(a, Attr::Bool(_)) {
                    return model_err(MSG);
                }
                let w = py_float(&a).or_else(|_| model_err(MSG))?;
                if !(w.is_finite() && w > 0.0) {
                    return model_err(MSG);
                }
                #[allow(clippy::cast_precision_loss)]
                let max_cell = (0..3).map(|a| domain_mm[a] / gs[a] as f64).fold(f64::NEG_INFINITY, f64::max);
                if w < max_cell {
                    return model_err(
                        "minimum_wall_mm is below one geometry cell; refine geometry_grid explicitly rather than silently drop the wall guarantee",
                    );
                }
                Some(w)
            }
        };
        let prod = |g: [usize; 3]| g[0] * g[1] * g[2];
        if prod(analysis_grid) > 8_000_000 || prod(gs) > 8_000_000 || 20 * prod(control_grid) > 8_000_000 {
            return model_err("declared grids exceed the eight-million-value authoring limit");
        }
        Ok(spec)
    }

    #[must_use]
    pub fn geometry_shape(&self) -> [usize; 3] {
        self.geometry_grid.unwrap_or(self.analysis_grid)
    }

    #[must_use]
    pub fn config(&self) -> LatticeConfig {
        #[allow(clippy::cast_precision_loss)]
        let control_dx =
            std::array::from_fn(|a| self.domain_mm[a] * 1e-3 / (self.control_grid[a] as f64 - 1.0));
        LatticeConfig {
            period: self.period_mm * 1e-3,
            t_min: self.thickness_range_mm[0] * 1e-3,
            t_max: self.thickness_range_mm[1] * 1e-3,
            control_dx,
            s_max: self.stretch_limit,
            ..LatticeConfig::default()
        }
    }

    #[must_use]
    pub fn grids(&self) -> Grids {
        let gs = self.geometry_shape();
        #[allow(clippy::cast_precision_loss)]
        let spacing = std::array::from_fn(|a| self.domain_mm[a] * 1e-3 / gs[a] as f64);
        build_grids(gs, spacing, &self.config())
    }

    #[must_use]
    pub fn synth_options(&self) -> SynthOptions {
        SynthOptions {
            interface_w: 1.0,
            beta_mask: self.beta_mask,
            t_offset: 0.0,
            beta_mat: self.beta_phase,
            beta_topo: self.beta_topology,
            channels: Channels::all(),
            interface_len: Some(self.interface_mm * 1e-3),
        }
    }


    pub fn geometry_fields(&self, controls: &Controls) -> GResult<GeometryFields> {
        let grids = self.grids();
        let state = synthesize(controls, &grids, &self.config(), &self.synth_options())?;
        let mut rho = state.fields.rho.clone();
        let mut phase = state.fields.phase_fraction.clone();
        let cells = rho.len();
        let mut gate_rho = vec![1.0; cells];
        let mut gate_phase = vec![1.0; cells];
        let xyz: Vec<[f64; 3]> = (0..cells)
            .map(|c| std::array::from_fn(|a| grids.xc[a][c] * 1000.0 + self.origin_mm[a]))
            .collect();
        for r in &self.fixed_regions {
            for (c, p) in xyz.iter().enumerate() {
                if r.contains_cell(*p) {
                    rho[c] = r.occupancy;
                    gate_rho[c] = 0.0;
                    if let Some(ph) = r.phase_fraction {
                        phase[c] = ph;
                        gate_phase[c] = 0.0;
                    }
                }
            }
        }
        let opening = if let Some(wall) = self.minimum_wall_mm {

            let spacing_mm: [f64; 3] = std::array::from_fn(|a| grids.spacing[a] * 1000.0);
            let opening = Opening::new(&rho, self.geometry_shape(), spacing_mm, 0.5 * wall);
            rho = opening.apply(&rho);
            for r in &self.fixed_regions {
                for (c, p) in xyz.iter().enumerate() {
                    if r.contains_cell(*p) {
                        rho[c] = r.occupancy;
                    }
                }
            }
            Some((opening, gate_rho.clone()))
        } else {
            None
        };
        if let Some(fr) = &self.frozen_geometry {
            if fr.matches_grid(self.geometry_shape(), self.origin_mm, self.domain_mm) {
                fr.apply_grid(&mut rho, &mut phase);
                for (g, m) in gate_rho.iter_mut().zip(&fr.occupancy_mask) {
                    if *m {
                        *g = 0.0;
                    }
                }
                for (g, m) in gate_phase.iter_mut().zip(&fr.phase_mask) {
                    if *m {
                        *g = 0.0;
                    }
                }
            } else {
                let [ar, ap] = fr.active();
                for (c, p) in xyz.iter().enumerate() {
                    let (wr, rr, wp, rp) = fr.weights_at::<f64>(*p);
                    if ar {
                        rho[c] = (1.0 - wr) * rho[c] + wr * rr;
                        gate_rho[c] *= 1.0 - wr;
                    }
                    if ap {
                        phase[c] = (1.0 - wp) * phase[c] + wp * rp;
                        gate_phase[c] *= 1.0 - wp;
                    }
                }
            }
        }
        Ok(GeometryFields { rho, phase_fraction: phase, gate_rho, gate_phase, opening, state })
    }

    pub fn sample<S: Scalar>(&self, rho: &[S], phase: &[S], x: [S; 3]) -> (S, S) {
        let gs = self.geometry_shape();
        #[allow(clippy::cast_precision_loss)]
        let ids: [S; 3] =
            std::array::from_fn(|a| (x[a] - self.origin_mm[a]) / (self.domain_mm[a] / gs[a] as f64) - 0.5);
        let mut r = trilerp_at(rho, gs, ids);
        let mut ph = trilerp_at(phase, gs, ids);
        self.overlay(&mut r, &mut ph, x);
        (r, ph)
    }

    pub fn sample_const<S: Scalar>(&self, rho: &[f64], phase: &[f64], x: [S; 3]) -> (S, S) {
        let gs = self.geometry_shape();
        #[allow(clippy::cast_precision_loss)]
        let ids: [S; 3] =
            std::array::from_fn(|a| (x[a] - self.origin_mm[a]) / (self.domain_mm[a] / gs[a] as f64) - 0.5);
        let mut r = trilerp_const(rho, gs, ids);
        let mut ph = trilerp_const(phase, gs, ids);
        self.overlay(&mut r, &mut ph, x);
        (r, ph)
    }

    fn overlay<S: Scalar>(&self, r: &mut S, ph: &mut S, x: [S; 3]) {
        let xv = x.map(Scalar::val);
        if self.geometry_grid.is_some() {
            for fr in &self.fixed_regions {
                let protected = (0..3).all(|a| {
                    let upper = fr.upper_mm[a];
                    let domain_upper = self.origin_mm[a] + self.domain_mm[a];
                    xv[a] >= fr.lower_mm[a] && (xv[a] < upper || (upper == domain_upper && xv[a] <= upper))
                });
                if protected {
                    *r = S::cst(fr.occupancy);
                    if let Some(p) = fr.phase_fraction {
                        *ph = S::cst(p);
                    }
                }
            }
        }
        let inside = (0..3).all(|a| {
            let l = xv[a] - self.origin_mm[a];
            l >= 0.0 && l <= self.domain_mm[a]
        });
        if !inside {
            *r = S::cst(0.0);
        }
    }

    #[must_use]
    pub fn struct_tokens(&self) -> Vec<String> {
        let mut out: Vec<String> =
            self.struct_values().into_iter().map(|(k, v)| format!("{k}={}", v.repr())).collect();

        if let Some(w) = self.minimum_wall_mm {
            out.push(format!("minimum_wall_mm={}", PyObj::Float(w).repr()));
        }
        out
    }

    fn struct_values(&self) -> Vec<(&'static str, PyObj)> {
        vec![
            ("control_schema", PyObj::Str(CONTROL_SCHEMA.into())),
            ("origin_mm", PyObj::float_tuple(&self.origin_mm)),
            ("domain_mm", PyObj::float_tuple(&self.domain_mm)),
            ("analysis_grid", int_tuple(&self.analysis_grid)),
            ("geometry_grid", self.geometry_grid.map_or(PyObj::None, |g| int_tuple(&g))),
            ("control_grid", int_tuple(&self.control_grid)),
            ("period_mm", PyObj::Float(self.period_mm)),
            ("thickness_range_mm", PyObj::float_tuple(&self.thickness_range_mm)),
            ("interface_mm", PyObj::Float(self.interface_mm)),
            ("stretch_limit", PyObj::Float(self.stretch_limit)),
            ("beta_mask", PyObj::Float(self.beta_mask)),
            ("beta_phase", PyObj::Float(self.beta_phase)),
            ("beta_topology", PyObj::Float(self.beta_topology)),
            ("fixed_regions", PyObj::Tuple(self.fixed_regions.iter().map(FixedRegion::py_obj).collect())),
            ("frozen_geometry", self.frozen_geometry.as_ref().map_or(PyObj::None, FrozenGeometry::py_obj)),
        ]
    }

    #[must_use]
    pub fn struct_json(&self) -> Vec<(String, Value)> {
        let regions: Vec<Value> = self
            .fixed_regions
            .iter()
            .map(|r| json!([r.id, r.lower_mm, r.upper_mm, r.occupancy, r.phase_fraction]))
            .collect();
        vec![
            ("control_schema".into(), json!(CONTROL_SCHEMA)),
            ("origin_mm".into(), json!(self.origin_mm)),
            ("domain_mm".into(), json!(self.domain_mm)),
            ("analysis_grid".into(), json!(self.analysis_grid)),
            ("geometry_grid".into(), json!(self.geometry_grid)),
            ("control_grid".into(), json!(self.control_grid)),
            ("period_mm".into(), json!(self.period_mm)),
            ("thickness_range_mm".into(), json!(self.thickness_range_mm)),
            ("interface_mm".into(), json!(self.interface_mm)),
            ("stretch_limit".into(), json!(self.stretch_limit)),
            ("beta_mask".into(), json!(self.beta_mask)),
            ("beta_phase".into(), json!(self.beta_phase)),
            ("beta_topology".into(), json!(self.beta_topology)),
            ("fixed_regions".into(), Value::Array(regions)),
            (
                "frozen_geometry".into(),
                self.frozen_geometry.as_ref().map_or(Value::Null, FrozenGeometry::to_json),
            ),
        ]
        .into_iter()
        .chain(self.minimum_wall_mm.map(|w| ("minimum_wall_mm".into(), json!(w))))
        .collect()
    }

    #[must_use]
    pub fn doc_attrs(&self) -> Vec<(String, Attr)> {
        let mut out: Vec<(String, Attr)> = vec![
            ("control_schema".into(), Attr::Str(CONTROL_SCHEMA.into())),
            ("origin_mm".into(), floats_attr(&self.origin_mm)),
            ("domain_mm".into(), floats_attr(&self.domain_mm)),
            ("analysis_grid".into(), ints_attr(&self.analysis_grid)),
        ];
        if let Some(g) = self.geometry_grid {
            out.push(("geometry_grid".into(), ints_attr(&g)));
        }
        out.extend([
            ("control_grid".into(), ints_attr(&self.control_grid)),
            ("period_mm".into(), Attr::Float(self.period_mm)),
            ("thickness_range_mm".into(), floats_attr(&self.thickness_range_mm)),
            ("interface_mm".into(), Attr::Float(self.interface_mm)),
            ("stretch_limit".into(), Attr::Float(self.stretch_limit)),
            ("beta_mask".into(), Attr::Float(self.beta_mask)),
            ("beta_phase".into(), Attr::Float(self.beta_phase)),
            ("beta_topology".into(), Attr::Float(self.beta_topology)),
        ]);
        if let Some(f) = &self.frozen_geometry {
            out.push(("frozen_geometry".into(), f.to_attr()));
        }
        if let Some(w) = self.minimum_wall_mm {
            out.push(("minimum_wall_mm".into(), Attr::Float(w)));
        }
        out.push((
            "fixed_regions".into(),
            Attr::List(self.fixed_regions.iter().map(FixedRegion::doc_attr).collect()),
        ));
        out
    }

    #[must_use]
    pub fn cell_points(&self, shape: [usize; 3]) -> Vec<[f64; 3]> {
        #[allow(clippy::cast_precision_loss)]
        let axes: [Vec<f64>; 3] = std::array::from_fn(|a| {
            (0..shape[a])
                .map(|i| (i as f64 + 0.5) * self.domain_mm[a] / shape[a] as f64 + self.origin_mm[a])
                .collect()
        });
        let mut out = Vec::with_capacity(shape.iter().product());
        for x in &axes[0] {
            for y in &axes[1] {
                for z in &axes[2] {
                    out.push([*x, *y, *z]);
                }
            }
        }
        out
    }

    #[must_use]
    pub fn sampling_spacing_mm(&self) -> f64 {
        let gs = self.geometry_shape();
        #[allow(clippy::cast_precision_loss)]
        (0..3).map(|a| self.domain_mm[a] / gs[a] as f64).fold(f64::INFINITY, f64::min)
    }
}


pub fn node_controls(node: &Node, grid: [usize; 3]) -> GResult<Controls> {
    let p = node
        .param("control")
        .ok_or_else(|| GeometryError::Model("lattice.controlled has no control tensor".into()))?;
    let arr = p
        .as_ndarray()
        .map_err(|_| GeometryError::Model("spatial controls must be real and finite".into()))?;
    validate_control(arr.shape(), &arr.to_f64_vec(), arr.dtype().kind(), Some(grid))
        .map_err(|e| GeometryError::Model(e.to_string()))
}

pub(crate) fn controls_param(c: &Controls) -> ParamValue {
    let shape = vec![20, c.grid[0], c.grid[1], c.grid[2]];
    ParamValue::Array(Arc::new(
        NdArray::from_f64(shape, c.data.clone()).unwrap_or_else(|| NdArray::scalar(0.0)),
    ))
}

pub(crate) fn take_control(
    params: &mut BTreeMap<String, ParamValue>,
    grid: [usize; 3],
    blocks: usize,
) -> GResult<()> {
    match params.get("control") {
        None => {
            let one = initial_controls(grid);
            let mut data = Vec::with_capacity(blocks * one.data.len());
            for _ in 0..blocks {
                data.extend_from_slice(&one.data);
            }
            let shape = vec![20 * blocks, grid[0], grid[1], grid[2]];
            params.insert(
                "control".into(),
                ParamValue::Array(Arc::new(
                    NdArray::from_f64(shape, data).unwrap_or_else(|| NdArray::scalar(0.0)),
                )),
            );
        }
        Some(v) => {
            let arr = v
                .as_ndarray()
                .map_err(|_| GeometryError::Model("spatial controls must be real and finite".into()))?;
            let data = arr.to_f64_vec();
            if blocks == 1 {
                let c = validate_control(arr.shape(), &data, arr.dtype().kind(), Some(grid))
                    .map_err(|e| GeometryError::Model(e.to_string()))?;
                params.insert("control".into(), controls_param(&c));
            } else {
                let want = [20 * blocks, grid[0], grid[1], grid[2]];
                if arr.shape() != want {
                    return model_err(format!(
                        "controlled assembly requires shape {}",
                        int_tuple(&want).repr()
                    ));
                }
                let n = 20 * grid.iter().product::<usize>();
                for b in 0..blocks {
                    validate_control(
                        &[20, grid[0], grid[1], grid[2]],
                        &data[b * n..(b + 1) * n],
                        arr.dtype().kind(),
                        Some(grid),
                    )
                    .map_err(|e| GeometryError::Model(e.to_string()))?;
                }
                let out = NdArray::from_f64(want.to_vec(), data).unwrap_or_else(|| NdArray::scalar(0.0));
                params.insert("control".into(), ParamValue::Array(Arc::new(out)));
            }
        }
    }
    Ok(())
}

pub struct ControlledLattice {
    pub spec: LatticeSpec,
}

static INFO: StaticInfo = StaticInfo::new();

impl ControlledLattice {
    pub fn kind_info() -> &'static KindInfo {
        INFO.get(|| {
            kind_info(
                "lattice.controlled",
                MODULE,
                "",
                "",
                Some(FieldClass::implicit()),
                true,
                &[],
                vec![ParamSpec::float(
                    "control",
                    0.0,
                    "-",
                    "Twenty spatial fields; see implexity-spatial-controls/1",
                )],
            )
        })
    }

    fn construct(mut args: ConstructArgs) -> GResult<Node> {
        let spec = LatticeSpec::from_attrs(&mut args.attrs)?;
        if !args.children.is_empty() {
            return model_err("lattice.controlled is a leaf; compose fixed regions explicitly");
        }
        args.attrs_into_params();
        take_control(&mut args.params, spec.control_grid, 1)?;
        Node::new(Arc::new(Self { spec }), args.children, args.names, args.params)
    }

    #[must_use]
    pub fn entry() -> crate::node::KindEntry {
        entry(Self::kind_info(), Self::construct)
    }


    pub fn geometry_fields(&self, node: &Node) -> GResult<GeometryFields> {
        self.spec.geometry_fields(&node_controls(node, self.spec.control_grid)?)
    }
}

pub fn occupancy_to_field<S: Scalar>(rho: S, interface_mm: f64) -> S {
    let r = rho.clip_c(1e-14, 1.0 - 1e-14);
    -(r.ln() - (-r).ln_1p()) * interface_mm
}

struct OwnedLatticeK<S> {
    spec: Arc<LatticeSpec>,
    rho: Vec<S>,
}

impl<S: Scalar> Kernel<S> for OwnedLatticeK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        let gs = self.spec.geometry_shape();
        #[allow(clippy::cast_precision_loss)]
        let ids: [S; 3] = std::array::from_fn(|a| {
            (x[a] - self.spec.origin_mm[a]) / (self.spec.domain_mm[a] / gs[a] as f64) - 0.5
        });
        let mut r = trilerp_at(&self.rho, gs, ids);
        let mut unused = S::cst(0.0);
        self.spec.overlay(&mut r, &mut unused, x);
        occupancy_to_field(r, self.spec.interface_mm)
    }
}

impl NodeOp for ControlledLattice {
    fn info(&self) -> &KindInfo {
        Self::kind_info()
    }
    fn struct_tokens(&self) -> Vec<String> {
        self.spec.struct_tokens()
    }
    fn struct_json(&self) -> Vec<(String, Value)> {
        self.spec.struct_json()
    }
    fn doc_attrs(&self) -> Vec<(String, Attr)> {
        self.spec.doc_attrs()
    }
    fn field_class(&self, _n: &Node, _k: &[FieldClass]) -> GResult<FieldClass> {
        Ok(FieldClass::implicit())
    }
    fn aabb(&self, _n: &Node) -> GResult<Option<([f64; 3], [f64; 3])>> {
        let lo = self.spec.origin_mm;
        Ok(Some((lo, std::array::from_fn(|a| lo[a] + self.spec.domain_mm[a]))))
    }
    fn prepare(&self, node: &Node, want_vjp: bool) -> GResult<Prepared> {
        let gf = self.geometry_fields(node)?;
        let shape = self.spec.geometry_shape().to_vec();
        let derived = vec![(shape, gf.rho.clone())];
        let vjp: Option<crate::node::DerivedVjp> = if want_vjp {
            let zero = vec![0.0; gf.rho.len()];
            Some(Box::new(move |adj: &[Vec<f64>]| {
                let a = adj.first().map_or(&zero, |v| v);
                Ok(vec![("control".to_string(), gf.vjp(a, &zero)?)])
            }))
        } else {
            None
        };
        Ok(Prepared { derived, vjp })
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        _k: Vec<KernelBox<S>>,
        _c: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        let rho = inp.derived(0)?.data.clone();
        Ok(Box::new(OwnedLatticeK { spec: Arc::new(self.spec.clone()), rho }))
    }
    fn render_sampling_spacing_mm(&self, _n: &Node) -> Option<f64> {
        Some(self.spec.sampling_spacing_mm())
    }
    fn occupancy_source(&self) -> Option<&dyn OccupancySource> {
        Some(self)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub(crate) fn derived_specs() -> Vec<Value> {
    vec![
        json!({"id": "occupancy", "label": "Solid occupancy", "units": "-"}),
        json!({"id": "phase_fraction", "label": "Neutral phase fraction", "units": "-"}),
    ]
}

pub(crate) fn descriptors(origin: [f64; 3], spacing: [f64; 3], what: &str) -> Vec<Value> {
    let reg = |mut v: Value| {
        if let Value::Object(m) = &mut v {
            m.insert("origin_mm".into(), json!(origin));
            m.insert("spacing_mm".into(), json!(spacing));
            m.insert("centering".into(), json!("cell"));
        }
        v
    };
    vec![
        reg(json!({"derived": "phase_fraction", "label": "Neutral phase fraction",
            "description": format!("Derived {what}phase fraction in [0, 1]{}", if what.is_empty() { " of the geometry grid." } else { ", resampled on the envelope." }),
            "units": "-", "suggested_palette": "sequential"})),
        reg(json!({"derived": "occupancy", "label": "Solid occupancy",
            "description": format!("Derived {what}solid occupancy in [0, 1]{}", if what.is_empty() { " of the geometry grid." } else { ", resampled on the envelope." }),
            "units": "-", "suggested_palette": "sequential"})),
    ]
}

impl OccupancySource for ControlledLattice {
    fn derived_field_specs(&self) -> Vec<Value> {
        derived_specs()
    }
    fn render_field_descriptors(&self, _node: &Node) -> GResult<Vec<Value>> {
        let gs = self.spec.geometry_shape();
        #[allow(clippy::cast_precision_loss)]
        let spacing = std::array::from_fn(|a| self.spec.domain_mm[a] / gs[a] as f64);
        Ok(descriptors(self.spec.origin_mm, spacing, ""))
    }
    fn render_derived_fields(
        &self,
        node: &Node,
        points_mm: &[[f64; 3]],
    ) -> GResult<BTreeMap<String, Vec<f64>>> {
        let gf = self.geometry_fields(node)?;
        let (rho, phase): (Vec<f64>, Vec<f64>) =
            points_mm.iter().map(|p| self.spec.sample_const::<f64>(&gf.rho, &gf.phase_fraction, *p)).unzip();
        Ok(BTreeMap::from([("occupancy".to_string(), rho), ("phase_fraction".to_string(), phase)]))
    }
    fn render_derived_grids(&self, node: &Node, names: &[String]) -> GResult<Vec<DerivedGrid>> {
        let gf = self.geometry_fields(node)?;
        let gs = self.spec.geometry_shape();
        names
            .iter()
            .map(|name| match name.as_str() {
                "phase_fraction" => Ok((name.clone(), gs, gf.phase_fraction.clone())),
                "occupancy" => Ok((name.clone(), gs, gf.rho.clone())),
                other => model_err(format!("lattice.controlled declares no derived field {other}")),
            })
            .collect()
    }
    fn analysis_fields(&self, node: &Node) -> GResult<CellFields> {
        let gf = self.geometry_fields(node)?;
        if self.spec.analysis_grid == self.spec.geometry_shape() {
            return Ok(CellFields {
                shape: self.spec.analysis_grid,
                rho: gf.rho,
                phase_fraction: gf.phase_fraction,
            });
        }
        let pts = self.spec.cell_points(self.spec.analysis_grid);
        let (rho, phase_fraction) =
            pts.iter().map(|p| self.spec.sample_const::<f64>(&gf.rho, &gf.phase_fraction, *p)).unzip();
        Ok(CellFields { shape: self.spec.analysis_grid, rho, phase_fraction })
    }
    fn analysis_vjp(&self, node: &Node, adj_rho: &[f64], adj_phase: &[f64]) -> GResult<Vec<f64>> {
        let gf = self.geometry_fields(node)?;
        if self.spec.analysis_grid == self.spec.geometry_shape() {
            return gf.vjp(adj_rho, adj_phase);
        }
        let pts = self.spec.cell_points(self.spec.analysis_grid);
        if adj_rho.len() != pts.len() || adj_phase.len() != pts.len() {
            return Err(GeometryError::Value("field adjoint does not match the analysis grid".into()));
        }

        let (gr, gp) = sample_transpose(&self.spec, &gf.rho, &gf.phase_fraction, &pts, adj_rho, adj_phase)?;
        gf.vjp(&gr, &gp)
    }
}

pub(crate) fn sample_transpose(
    spec: &LatticeSpec,
    rho: &[f64],
    phase: &[f64],
    pts: &[[f64; 3]],
    adj_rho: &[f64],
    adj_phase: &[f64],
) -> GResult<(Vec<f64>, Vec<f64>)> {
    use crate::scalar::{RevTape, Rv};
    let mut tape = RevTape::begin()?;
    let lr: Vec<Rv> = rho.iter().map(|v| tape.leaf(*v)).collect();
    let lp: Vec<Rv> = phase.iter().map(|v| tape.leaf(*v)).collect();
    for ((p, ar), ap) in pts.iter().zip(adj_rho).zip(adj_phase) {
        let mark = tape.len();
        let (r, ph) = spec.sample(&lr, &lp, p.map(Rv::cst));
        let y = r * *ar + ph * *ap;
        tape.sweep_to(y, 1.0, mark);
    }
    tape.finish();
    Ok((lr.iter().map(|l| tape.adjoint(*l)).collect(), lp.iter().map(|l| tape.adjoint(*l)).collect()))
}
