// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::error::{GResult, GeometryError, model_err};
use crate::field_registration::GridRegistration;
use crate::fieldclass::FieldClass;
use crate::kinds::{StaticInfo, entry, kind_info};
use crate::lattice::controls::{CONTROL_SCHEMA, Controls, control_contract};
use crate::lattice::freeze::FrozenGeometry;
use crate::lattice::node::{
    ControlledLattice, FixedRegion, GeometryFields, LatticeSpec, derived_specs, descriptors, float_seq,
    floats_attr, int_tuple, ints_attr, occupancy_to_field, parse_grid, parse_regions, py_float, take_control,
};
use crate::linalg3::{inv, singular_values};
use crate::node::{
    Attr, ConstructArgs, EvalCtx, Kernel, KernelBox, KernelInputs, KindInfo, Node, NodeOp, ParamSpec,
    Prepared,
};
use crate::occupancy::{CellFields, DerivedGrid, OccupancySource};
use crate::pyfmt::{PyObj, str_repr};
use crate::scalar::{RevTape, Rv, Scalar};

pub const ASSEMBLY_SCHEMA: &str = "implexity-controlled-assembly/1";
pub const PHASE_POLICY: &str = "union_occupancy_weighted_intersection_difference_left_v1";
pub const MAX_VOLUMES: usize = 16;
pub const RENDER_DERIVED_MAX_CELLS: usize = 2_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Composition {
    Volume(String),
    Op {
        operation: String,
        left: Box<Composition>,
        right: Box<Composition>,
    },
}

impl Composition {
    fn parse(raw: Option<&Attr>, names: &[String], depth: usize, used: &mut Vec<String>) -> GResult<Self> {
        if depth > 32 {
            return model_err("assembly expression exceeds the bounded tree depth");
        }
        if let Some(Attr::Str(s)) = raw {
            if !names.contains(s) {
                return model_err(format!("assembly expression references unknown volume {}", str_repr(s)));
            }
            used.push(s.clone());
            return Ok(Self::Volume(s.clone()));
        }
        let bad = || {
            GeometryError::Model(
                "assembly expression requires a volume id or a binary Boolean operation".into(),
            )
        };
        let Some(Attr::Dict(m)) = raw else { return Err(bad()) };
        let keys: std::collections::BTreeSet<&str> = m.iter().map(|(k, _)| k.as_str()).collect();
        let get = |k: &str| m.iter().find(|(n, _)| n == k).map(|(_, v)| v);
        let op = get("operation").and_then(Attr::as_str).unwrap_or_default().to_string();
        if keys != ["left", "operation", "right"].into_iter().collect()
            || !["union", "intersection", "difference"].contains(&op.as_str())
        {
            return Err(bad());
        }
        let left = Self::parse(get("left"), names, depth + 1, used)?;
        let right = Self::parse(get("right"), names, depth + 1, used)?;
        Ok(Self::Op { operation: op, left: Box::new(left), right: Box::new(right) })
    }

    #[must_use]
    pub fn to_attr(&self) -> Attr {
        match self {
            Self::Volume(s) => Attr::Str(s.clone()),
            Self::Op { operation, left, right } => Attr::Dict(vec![
                ("operation".into(), Attr::Str(operation.clone())),
                ("left".into(), left.to_attr()),
                ("right".into(), right.to_attr()),
            ]),
        }
    }

    pub fn combine<S: Scalar>(&self, ids: &[String], fields: &[(S, S)]) -> (S, S) {
        match self {
            Self::Volume(s) => {
                let i = ids.iter().position(|x| x == s).unwrap_or(0);
                fields[i]
            }
            Self::Op { operation, left, right } => {
                let (ar, ap) = left.combine(ids, fields);
                let (br, bp) = right.combine(ids, fields);
                match operation.as_str() {
                    "union" => {
                        let rho = ar.max(br);
                        let support = ar + br;
                        let phase =
                            if support.val() > 0.0 { (ar * ap + br * bp) / support } else { (ap + bp) * 0.5 };
                        (rho, phase)
                    }
                    "intersection" => (ar.min(br), ap),
                    _ => (ar.min(-br + 1.0), ap),
                }
            }
        }
    }
}

fn parse_affine(raw: Option<&Attr>) -> GResult<([[f64; 4]; 4], [[f64; 4]; 4])> {
    let bad = || {
        GeometryError::Model(
            "local_to_model requires a finite affine 4x4 matrix; translation is in mm".into(),
        )
    };
    let rows: Vec<Vec<f64>> = match raw {
        Some(Attr::List(rows)) => rows
            .iter()
            .map(|r| match r {
                Attr::List(v) => v
                    .iter()
                    .map(|x| match x {
                        Attr::Int(_) | Attr::Float(_) => py_float(x).ok(),
                        _ => None,
                    })
                    .collect::<Option<Vec<f64>>>(),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(bad)?,
        Some(Attr::Array(a)) if a.shape() == [4, 4] && matches!(a.dtype().kind(), 'i' | 'u' | 'f') => {
            a.to_f64_vec().chunks(4).map(<[f64]>::to_vec).collect()
        }
        _ => return Err(bad()),
    };
    if rows.len() != 4
        || rows.iter().any(|r| r.len() != 4)
        || !rows.iter().flatten().all(|v| v.is_finite())
        || rows[3] != [0.0, 0.0, 0.0, 1.0]
    {
        return Err(bad());
    }
    let m: [[f64; 4]; 4] = std::array::from_fn(|i| std::array::from_fn(|j| rows[i][j]));
    let lin: [[f64; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| m[i][j]));
    let sv = singular_values(&lin);
    if sv[2] <= 0.0 || sv[0] / sv[2] > 1.0e8 {
        return model_err("local_to_model has a singular or numerically unresolved inverse");
    }
    let inverse = inv(&m).ok_or_else(|| {
        GeometryError::Model("local_to_model has a singular or numerically unresolved inverse".into())
    })?;
    if !inverse.iter().flatten().all(|v| v.is_finite()) {
        return model_err("local_to_model inverse is nonfinite");
    }
    Ok((m, inverse))
}

fn triple(raw: Option<&Attr>, default: [f64; 3], label: &str, positive: bool) -> GResult<[f64; 3]> {
    let Some(a) = raw else { return Ok(default) };
    let bad = || {
        GeometryError::Model(format!(
            "{label} requires a finite {}xyz vector",
            if positive { "positive " } else { "" }
        ))
    };
    let v = match a {
        Attr::List(items) if items.iter().all(|x| matches!(x, Attr::Int(_) | Attr::Float(_))) => {
            float_seq(a)?
        }
        Attr::Array(arr) if matches!(arr.dtype().kind(), 'i' | 'u' | 'f') && arr.ndim() == 1 => {
            arr.to_f64_vec()
        }
        _ => return Err(bad()),
    };
    if v.len() != 3 || !v.iter().all(|x| x.is_finite()) || (positive && v.iter().any(|x| *x <= 0.0)) {
        return Err(bad());
    }
    Ok([v[0], v[1], v[2]])
}

#[derive(Clone, Debug)]
pub struct Volume {
    pub id: String,
    pub spec: LatticeSpec,
    pub local_to_model: [[f64; 4]; 4],
    pub inverse: [[f64; 4]; 4],
}

pub struct ControlledAssembly {
    pub origin_mm: [f64; 3],
    pub domain_mm: [f64; 3],
    pub analysis_grid: [usize; 3],
    pub control_grid: [usize; 3],
    pub interface_mm: f64,
    pub volumes: Vec<Volume>,
    pub composition: Composition,
    pub fixed_regions: Vec<FixedRegion>,
    pub frozen_geometry: Option<FrozenGeometry>,
}

static INFO: StaticInfo = StaticInfo::new();

fn matrix_attr(m: &[[f64; 4]; 4]) -> Attr {
    Attr::List(m.iter().map(|r| floats_attr(r)).collect())
}

impl ControlledAssembly {
    pub fn kind_info() -> &'static KindInfo {
        INFO.get(|| {
            kind_info(
                "lattice.controlled_assembly",
                "implexity.implicit.lattice.assembly",
                "",
                "",
                Some(FieldClass::implicit()),
                true,
                &[],
                vec![ParamSpec::float(
                    "control",
                    0.0,
                    "-",
                    "Named volume blocks, twenty local spatial controls each",
                )],
            )
        })
    }

    fn construct(mut args: ConstructArgs) -> GResult<Node> {
        let at = &mut args.attrs;
        if !args.children.is_empty() {
            return model_err(
                "controlled assembly has a single authoritative control array, not editable child copies",
            );
        }
        let schema_ok = |a: Option<Attr>, want: &str| a.is_none_or(|v| v.as_str() == Some(want));
        let s1 = schema_ok(at.remove("assembly_schema"), ASSEMBLY_SCHEMA);
        let s2 = schema_ok(at.remove("control_schema"), CONTROL_SCHEMA);
        let s3 = schema_ok(at.remove("phase_policy"), PHASE_POLICY);
        if !(s1 && s2 && s3) {
            return model_err("unsupported controlled-assembly schema or phase convention");
        }
        let origin_mm = triple(at.remove("origin_mm").as_ref(), [0.0; 3], "origin_mm", false)?;
        let domain_mm = triple(at.remove("domain_mm").as_ref(), [12.0; 3], "domain_mm", true)?;
        let analysis_grid =
            at.remove("analysis_grid").map_or(Ok([12, 12, 12]), |a| parse_grid(&a, "analysis_grid"))?;
        let control_grid =
            at.remove("control_grid").map_or(Ok([3, 3, 3]), |a| parse_grid(&a, "control_grid"))?;
        let interface_mm = match at.remove("interface_mm") {
            None => 0.5,
            Some(a @ (Attr::Int(_) | Attr::Float(_))) => py_float(&a)?,
            Some(Attr::Array(arr)) if arr.ndim() == 0 && arr.dtype().kind() != 'b' => arr.to_f64_vec()[0],
            Some(_) => return model_err("interface_mm must be a positive finite scalar"),
        };
        if !interface_mm.is_finite() || interface_mm <= 0.0 {
            return model_err("interface_mm must be a positive finite scalar");
        }
        let rows: Vec<Attr> = match at.remove("volumes") {
            None => Vec::new(),
            Some(Attr::List(v)) => v,
            Some(_) => return model_err(format!("one to {MAX_VOLUMES} explicitly named volumes required")),
        };
        if rows.is_empty() || rows.len() > MAX_VOLUMES {
            return model_err(format!("one to {MAX_VOLUMES} explicitly named volumes required"));
        }
        let prod = |g: [usize; 3]| g[0] * g[1] * g[2];
        if prod(analysis_grid) > 8_000_000 || 20 * rows.len() * prod(control_grid) > 8_000_000 {
            return model_err("assembly analysis/control grid exceeds its allocation bound");
        }
        let mut volumes: Vec<Volume> = Vec::new();
        let mut geometry_cells = 0usize;
        for row in &rows {
            let Attr::Dict(m) = row else {
                return model_err("a volume requires id, geometry and local_to_model");
            };
            let keys: std::collections::BTreeSet<&str> = m.iter().map(|(k, _)| k.as_str()).collect();
            if keys != ["geometry", "id", "local_to_model"].into_iter().collect() {
                return model_err("a volume requires id, geometry and local_to_model");
            }
            let get = |k: &str| m.iter().find(|(n, _)| n == k).map(|(_, v)| v);
            let ident = match get("id") {
                Some(Attr::Str(s)) if valid_ident(s) && !volumes.iter().any(|v| &v.id == s) => s.clone(),
                _ => return model_err("volume ids must be unique canonical snake_case identifiers"),
            };
            let Some(Attr::Dict(geom)) = get("geometry") else {
                return model_err("volume geometry must not contain a second control authority");
            };
            if geom.iter().any(|(k, _)| k == "control") {
                return model_err("volume geometry must not contain a second control authority");
            }
            let gargs = ConstructArgs {
                children: Vec::new(),
                names: None,
                params: BTreeMap::new(),
                attrs: geom.iter().cloned().collect(),
            };
            let node = ControlledLattice::entry().construct.as_ref()(gargs)?;
            let spec =
                node.op().as_any().downcast_ref::<ControlledLattice>().map(|l| l.spec.clone()).ok_or_else(
                    || GeometryError::Model("volume geometry is not a controlled lattice".into()),
                )?;
            if spec.control_grid != control_grid || spec.geometry_grid.is_none() {
                return model_err(
                    "volumes require the shared control-grid shape and an explicit independent geometry_grid",
                );
            }
            geometry_cells += prod(spec.geometry_shape());
            if geometry_cells > 8_000_000 {
                return model_err("aggregate geometry grid exceeds eight million cells");
            }
            let (local_to_model, inverse) = parse_affine(get("local_to_model"))?;
            volumes.push(Volume { id: ident, spec, local_to_model, inverse });
        }
        let ids: Vec<String> = volumes.iter().map(|v| v.id.clone()).collect();
        let mut used = Vec::new();
        let composition = Composition::parse(at.remove("composition").as_ref(), &ids, 0, &mut used)?;
        let used_set: std::collections::BTreeSet<&String> = used.iter().collect();
        if used.len() != ids.len() || used_set != ids.iter().collect() {
            return model_err("every declared volume must occur exactly once in the composition");
        }
        let frozen_geometry = FrozenGeometry::normalise(at.remove("frozen_geometry").as_ref())?;
        let fixed_regions = parse_regions(at.remove("fixed_regions").as_ref())?;
        for r in &fixed_regions {
            if (0..3).any(|a| r.lower_mm[a] < origin_mm[a] || r.upper_mm[a] > origin_mm[a] + domain_mm[a]) {
                return model_err(format!(
                    "fixed region {} is outside the assembly envelope",
                    str_repr(&r.id)
                ));
            }
        }
        args.attrs_into_params();
        take_control(&mut args.params, control_grid, volumes.len())?;
        let op = Self {
            origin_mm,
            domain_mm,
            analysis_grid,
            control_grid,
            interface_mm,
            volumes,
            composition,
            fixed_regions,
            frozen_geometry,
        };
        Node::new(Arc::new(op), args.children, args.names, args.params)
    }

    #[must_use]
    pub fn entry() -> crate::node::KindEntry {
        entry(Self::kind_info(), Self::construct)
    }

    #[must_use]
    pub fn control_shape(&self) -> [usize; 4] {
        [20 * self.volumes.len(), self.control_grid[0], self.control_grid[1], self.control_grid[2]]
    }

    fn volume_doc(v: &Volume) -> Attr {
        Attr::Dict(vec![
            ("id".into(), Attr::Str(v.id.clone())),
            ("geometry".into(), Attr::Dict(v.spec.doc_attrs())),
            ("local_to_model".into(), matrix_attr(&v.local_to_model)),
        ])
    }

    fn regions_attr(&self) -> Attr {
        let lattice_like = LatticeSpec {
            origin_mm: self.origin_mm,
            domain_mm: self.domain_mm,
            analysis_grid: self.analysis_grid,
            geometry_grid: None,
            control_grid: self.control_grid,
            period_mm: 1.0,
            thickness_range_mm: [0.1, 0.2],
            interface_mm: self.interface_mm,
            stretch_limit: 1.0,
            beta_mask: 1.0,
            beta_phase: 1.0,
            beta_topology: 1.0,
            fixed_regions: self.fixed_regions.clone(),
            frozen_geometry: None,
            minimum_wall_mm: None,
        };
        lattice_like
            .doc_attrs()
            .into_iter()
            .find(|(k, _)| k == "fixed_regions")
            .map_or(Attr::List(Vec::new()), |(_, v)| v)
    }

    fn struct_values(&self) -> Vec<(&'static str, Attr)> {
        vec![
            ("assembly_schema", Attr::Str(ASSEMBLY_SCHEMA.into())),
            ("control_schema", Attr::Str(CONTROL_SCHEMA.into())),
            ("origin_mm", floats_attr(&self.origin_mm)),
            ("domain_mm", floats_attr(&self.domain_mm)),
            ("analysis_grid", ints_attr(&self.analysis_grid)),
            ("control_grid", ints_attr(&self.control_grid)),
            ("interface_mm", Attr::Float(self.interface_mm)),
            ("volumes", Attr::List(self.volumes.iter().map(Self::volume_doc).collect())),
            ("composition", self.composition.to_attr()),
            ("phase_policy", Attr::Str(PHASE_POLICY.into())),
            ("fixed_regions", self.regions_attr()),
            ("frozen_geometry", self.frozen_geometry.as_ref().map_or(Attr::Null, FrozenGeometry::to_attr)),
        ]
    }

    pub fn fields_at<S: Scalar>(&self, rho: &[Vec<S>], phase: &[Vec<S>], p: [S; 3]) -> (S, S) {
        let ids: Vec<String> = self.volumes.iter().map(|v| v.id.clone()).collect();
        let per: Vec<(S, S)> = self
            .volumes
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let inv = &v.inverse;
                let local: [S; 3] = std::array::from_fn(|j| {
                    p[0] * inv[j][0] + p[1] * inv[j][1] + p[2] * inv[j][2] + inv[j][3]
                });
                v.spec.sample(&rho[i], &phase[i], local)
            })
            .collect();
        let (mut r, mut ph) = self.composition.combine(&ids, &per);
        if let Some(fr) = &self.frozen_geometry {
            (r, ph) = fr.apply_at(r, ph, p);
        }
        let pv = p.map(Scalar::val);
        let end: [f64; 3] = std::array::from_fn(|a| self.origin_mm[a] + self.domain_mm[a]);
        for fr in &self.fixed_regions {
            let inside = (0..3).all(|a| {
                pv[a] >= fr.lower_mm[a]
                    && (pv[a] < fr.upper_mm[a] || (fr.upper_mm[a] == end[a] && pv[a] <= fr.upper_mm[a]))
            });
            if inside {
                r = S::cst(fr.occupancy);
                if let Some(v) = fr.phase_fraction {
                    ph = S::cst(v);
                }
            }
        }
        if !(0..3).all(|a| pv[a] >= self.origin_mm[a] && pv[a] <= end[a]) {
            r = S::cst(0.0);
        }
        (r, ph)
    }


    pub fn volume_fields(&self, node: &Node) -> GResult<Vec<GeometryFields>> {
        let p = node
            .param("control")
            .ok_or_else(|| GeometryError::Model("controlled assembly has no control tensor".into()))?;
        let arr = p
            .as_ndarray()
            .map_err(|_| GeometryError::Model("spatial controls must be real and finite".into()))?;
        let data = arr.to_f64_vec();
        let n = 20 * self.control_grid.iter().product::<usize>();
        if data.len() != n * self.volumes.len() {
            return model_err(format!(
                "controlled assembly requires shape {}",
                int_tuple(&self.control_shape()).repr()
            ));
        }
        self.volumes
            .iter()
            .enumerate()
            .map(|(i, v)| {
                v.spec.geometry_fields(&Controls {
                    grid: self.control_grid,
                    data: data[i * n..(i + 1) * n].to_vec(),
                })
            })
            .collect()
    }

    fn eval_points(&self, node: &Node, pts: &[[f64; 3]]) -> GResult<(Vec<f64>, Vec<f64>)> {
        let vf = self.volume_fields(node)?;
        let rho: Vec<Vec<f64>> = vf.iter().map(|g| g.rho.clone()).collect();
        let phase: Vec<Vec<f64>> = vf.iter().map(|g| g.phase_fraction.clone()).collect();
        Ok(pts.iter().map(|p| self.fields_at(&rho, &phase, *p)).unzip())
    }

    fn points_vjp(
        &self,
        node: &Node,
        pts: &[[f64; 3]],
        adj_rho: &[f64],
        adj_phase: &[f64],
    ) -> GResult<Vec<f64>> {
        let vf = self.volume_fields(node)?;
        let mut tape = RevTape::begin()?;
        let lr: Vec<Vec<Rv>> = vf.iter().map(|g| g.rho.iter().map(|v| tape.leaf(*v)).collect()).collect();
        let lp: Vec<Vec<Rv>> =
            vf.iter().map(|g| g.phase_fraction.iter().map(|v| tape.leaf(*v)).collect()).collect();
        for ((p, ar), ap) in pts.iter().zip(adj_rho).zip(adj_phase) {
            let mark = tape.len();
            let (r, ph) = self.fields_at(&lr, &lp, p.map(Rv::cst));
            tape.sweep_to(r * *ar + ph * *ap, 1.0, mark);
        }
        tape.finish();
        let adj: Vec<(Vec<f64>, Vec<f64>)> = lr
            .iter()
            .zip(&lp)
            .map(|(r, p)| {
                (r.iter().map(|l| tape.adjoint(*l)).collect(), p.iter().map(|l| tape.adjoint(*l)).collect())
            })
            .collect();
        drop(tape);
        let mut out = Vec::new();
        for (g, (ar, ap)) in vf.iter().zip(adj) {
            out.extend(g.vjp(&ar, &ap)?);
        }
        Ok(out)
    }

    fn cell_points(&self, shape: [usize; 3]) -> Vec<[f64; 3]> {
        #[allow(clippy::cast_precision_loss)]
        let axes: [Vec<f64>; 3] = std::array::from_fn(|a| {
            (0..shape[a])
                .map(|i| (i as f64 + 0.5) * (self.domain_mm[a] / shape[a] as f64) + self.origin_mm[a])
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
        let mut finest = f64::INFINITY;
        for v in &self.volumes {
            let gs = v.spec.geometry_shape();
            for a in 0..3 {
                let col = crate::numpy::norm(&[
                    v.local_to_model[0][a],
                    v.local_to_model[1][a],
                    v.local_to_model[2][a],
                ]);
                #[allow(clippy::cast_precision_loss)]
                let cell = v.spec.domain_mm[a] / gs[a] as f64;
                finest = finest.min(col * cell);
            }
        }
        finest
    }

    #[must_use]
    pub fn render_derived_shape(&self) -> [usize; 3] {
        let mut spacing = self.sampling_spacing_mm();
        let shape_of = |s: f64| -> [usize; 3] {
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            std::array::from_fn(|a| ((self.domain_mm[a] / s - 1e-9).ceil().max(2.0)) as usize)
        };
        let mut shape = shape_of(spacing);
        while shape.iter().product::<usize>() > RENDER_DERIVED_MAX_CELLS {
            spacing *= 1.05;
            shape = shape_of(spacing);
        }
        shape
    }


    pub fn control_layout(&self) -> GResult<Value> {
        let base = control_contract();
        let comps = base["components"].as_array().cloned().unwrap_or_default();
        let mut components = Vec::new();
        for (i, v) in self.volumes.iter().enumerate() {
            let t = &v.local_to_model;
            #[allow(clippy::cast_precision_loss)]
            let d: [f64; 3] =
                std::array::from_fn(|a| v.spec.domain_mm[a] / (self.control_grid[a] as f64 - 1.0));
            let basis: [[f64; 3]; 3] = std::array::from_fn(|j| std::array::from_fn(|r| t[r][j] * d[j]));
            let origin: [f64; 3] = std::array::from_fn(|r| {
                t[r][0] * v.spec.origin_mm[0]
                    + t[r][1] * v.spec.origin_mm[1]
                    + t[r][2] * v.spec.origin_mm[2]
                    + t[r][3]
            });
            let registration =
                GridRegistration::new(self.control_grid, origin, basis, "node", "xyz", "model")?.to_wire();
            for (j, c) in comps.iter().enumerate() {
                let mut m = c.as_object().cloned().unwrap_or_default();
                let name = m.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
                let label = m.get("label").and_then(Value::as_str).unwrap_or_default().to_string();
                m.insert("index".into(), json!(20 * i + j));
                m.insert("control_index".into(), json!(j));
                m.insert("volume_id".into(), json!(v.id));
                m.insert("name".into(), json!(format!("{}:{name}", v.id)));
                m.insert("label".into(), json!(format!("{} / {label}", v.id)));
                m.insert("registration".into(), registration.clone());
                components.push(Value::Object(m));
            }
        }
        Ok(
            json!({"schema": "implexity-component-field-layout/2", "shape": self.control_shape(), "component_axis": 0,
            "spatial_axes": [1, 2, 3], "components": components, "spatial_registration": "per_component", "control_schema": CONTROL_SCHEMA}),
        )
    }

    #[must_use]
    pub fn sampling_evidence(&self) -> Value {
        let shapes: serde_json::Map<String, Value> =
            self.volumes.iter().map(|v| (v.id.clone(), json!(v.spec.geometry_shape()))).collect();
        json!({"geometry_sampling_mode": "independent_transformed_volume_grids", "volume_geometry_shapes": shapes,
            "phase_policy": PHASE_POLICY, "composition": self.composition.to_attr().to_json(), "affine_transforms_designable": false,
            "derivative_scope": "piecewise_control_derivative; fixed_masks_and_affines; ties_use_generalized_max_min"})
    }
}

fn valid_ident(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_lowercase()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

struct AssemblyK<S> {
    op: Arc<AssemblyView>,
    rho: Vec<Vec<S>>,
    phase: Vec<Vec<S>>,
}

struct AssemblyView(ControlledAssembly);

impl<S: Scalar> Kernel<S> for AssemblyK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        let (r, _) = self.op.0.fields_at(&self.rho, &self.phase, x);
        occupancy_to_field(r, self.op.0.interface_mm)
    }
}

impl Clone for ControlledAssembly {
    fn clone(&self) -> Self {
        Self {
            origin_mm: self.origin_mm,
            domain_mm: self.domain_mm,
            analysis_grid: self.analysis_grid,
            control_grid: self.control_grid,
            interface_mm: self.interface_mm,
            volumes: self.volumes.clone(),
            composition: self.composition.clone(),
            fixed_regions: self.fixed_regions.clone(),
            frozen_geometry: self.frozen_geometry.clone(),
        }
    }
}

impl NodeOp for ControlledAssembly {
    fn info(&self) -> &KindInfo {
        Self::kind_info()
    }
    fn struct_tokens(&self) -> Vec<String> {
        self.struct_values()
            .into_iter()
            .map(|(k, v)| format!("{k}={}", assembly_repr(k, &v).repr()))
            .collect()
    }
    fn struct_json(&self) -> Vec<(String, Value)> {
        self.struct_values().into_iter().map(|(k, v)| (k.to_string(), v.to_json())).collect()
    }
    fn doc_attrs(&self) -> Vec<(String, Attr)> {
        let mut out = Vec::new();
        if let Some(f) = &self.frozen_geometry {
            out.push(("frozen_geometry".to_string(), f.to_attr()));
        }
        for (k, v) in self.struct_values() {
            if k != "frozen_geometry" {
                out.push((k.to_string(), v));
            }
        }
        out
    }
    fn field_class(&self, _n: &Node, _k: &[FieldClass]) -> GResult<FieldClass> {
        Ok(FieldClass::implicit())
    }
    fn aabb(&self, _n: &Node) -> GResult<Option<([f64; 3], [f64; 3])>> {
        let lo = self.origin_mm;
        Ok(Some((lo, std::array::from_fn(|a| lo[a] + self.domain_mm[a]))))
    }
    fn prepare(&self, node: &Node, want_vjp: bool) -> GResult<Prepared> {
        let vf = self.volume_fields(node)?;
        let mut derived = Vec::new();
        for (v, g) in self.volumes.iter().zip(&vf) {
            let shape = v.spec.geometry_shape().to_vec();
            derived.push((shape.clone(), g.rho.clone()));
            derived.push((shape, g.phase_fraction.clone()));
        }
        let vjp: Option<crate::node::DerivedVjp> = if want_vjp {
            Some(Box::new(move |adj: &[Vec<f64>]| {
                let mut out = Vec::new();
                for (i, g) in vf.iter().enumerate() {
                    let zero = vec![0.0; g.rho.len()];
                    let ar = adj.get(2 * i).unwrap_or(&zero);
                    let ap = adj.get(2 * i + 1).unwrap_or(&zero);
                    out.extend(g.vjp(ar, ap)?);
                }
                Ok(vec![("control".to_string(), out)])
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
        let mut rho = Vec::new();
        let mut phase = Vec::new();
        for i in 0..self.volumes.len() {
            rho.push(inp.derived(2 * i)?.data.clone());
            phase.push(inp.derived(2 * i + 1)?.data.clone());
        }
        Ok(Box::new(AssemblyK { op: Arc::new(AssemblyView(self.clone())), rho, phase }))
    }
    fn render_sampling_spacing_mm(&self, _n: &Node) -> Option<f64> {
        Some(self.sampling_spacing_mm())
    }
    fn occupancy_source(&self) -> Option<&dyn OccupancySource> {
        Some(self)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn assembly_repr(key: &str, v: &Attr) -> PyObj {
    match (key, v) {
        ("origin_mm" | "domain_mm" | "analysis_grid" | "control_grid", Attr::List(items)) => {
            PyObj::Tuple(items.iter().map(Attr::py_obj).collect())
        }
        ("volumes", Attr::List(items)) => PyObj::Tuple(items.iter().map(Attr::py_obj).collect()),
        ("fixed_regions", Attr::List(items)) => PyObj::Tuple(
            items
                .iter()
                .map(|r| {
                    let get = |k: &str| r.get(k).cloned().unwrap_or(Attr::Null);
                    let tup = |a: Attr| match a {
                        Attr::List(v) => PyObj::Tuple(v.iter().map(Attr::py_obj).collect()),
                        other => other.py_obj(),
                    };
                    PyObj::Tuple(vec![
                        get("id").py_obj(),
                        tup(get("lower_mm")),
                        tup(get("upper_mm")),
                        get("occupancy").py_obj(),
                        get("phase_fraction").py_obj(),
                    ])
                })
                .collect(),
        ),
        _ => v.py_obj(),
    }
}

impl OccupancySource for ControlledAssembly {
    fn derived_field_specs(&self) -> Vec<Value> {
        derived_specs()
    }
    fn render_field_descriptors(&self, _node: &Node) -> GResult<Vec<Value>> {
        let shape = self.render_derived_shape();
        #[allow(clippy::cast_precision_loss)]
        let spacing = std::array::from_fn(|a| self.domain_mm[a] / shape[a] as f64);
        Ok(descriptors(self.origin_mm, spacing, "composed "))
    }
    fn render_derived_fields(
        &self,
        node: &Node,
        points_mm: &[[f64; 3]],
    ) -> GResult<BTreeMap<String, Vec<f64>>> {
        let (rho, phase) = self.eval_points(node, points_mm)?;
        Ok(BTreeMap::from([("occupancy".to_string(), rho), ("phase_fraction".to_string(), phase)]))
    }
    fn render_derived_grids(&self, node: &Node, names: &[String]) -> GResult<Vec<DerivedGrid>> {
        let unknown: Vec<&String> =
            names.iter().filter(|n| *n != "phase_fraction" && *n != "occupancy").collect();
        if !unknown.is_empty() {
            let list = PyObj::List(unknown.iter().map(|s| PyObj::Str((*s).clone())).collect());
            return model_err(format!("controlled assembly declares no derived field {}", list.repr()));
        }
        let shape = self.render_derived_shape();
        let (rho, phase) = self.eval_points(node, &self.cell_points(shape))?;
        Ok(names
            .iter()
            .map(|n| (n.clone(), shape, if n == "occupancy" { rho.clone() } else { phase.clone() }))
            .collect())
    }
    fn analysis_fields(&self, node: &Node) -> GResult<CellFields> {
        let (rho, phase_fraction) = self.eval_points(node, &self.cell_points(self.analysis_grid))?;
        Ok(CellFields { shape: self.analysis_grid, rho, phase_fraction })
    }
    fn analysis_vjp(&self, node: &Node, adj_rho: &[f64], adj_phase: &[f64]) -> GResult<Vec<f64>> {
        let pts = self.cell_points(self.analysis_grid);
        if adj_rho.len() != pts.len() || adj_phase.len() != pts.len() {
            return Err(GeometryError::Value("field adjoint does not match the analysis grid".into()));
        }
        self.points_vjp(node, &pts, adj_rho, adj_phase)
    }
}
