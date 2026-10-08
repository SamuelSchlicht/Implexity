// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::error::{GResult, GeometryError, model_err};
use crate::fieldclass::FieldClass;
use crate::kinds::{StaticInfo, entry, kind_info};
use crate::node::{
    Attr, ConstructArgs, EvalCtx, Kernel, KernelBox, KernelEntryList, KernelInputs, KindInfo, Node, NodeOp,
    ParamSpec,
};
use crate::scalar::{Scalar, reduce_max};
use crate::value::ParamValue;

const MODULE: &str = "implexity.implicit.interop";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampledKind {
    MeshSdf,
    GridField,
    CellGridField,
}

pub struct Sampled {
    pub kind: SampledKind,
    pub declared: Option<FieldClass>,
    pub source: Vec<(String, Attr)>,
    pub measurement: Vec<(String, Attr)>,
    pub visualization: Vec<(String, Attr)>,
}

fn common_params(
    samples_units: &str,
    samples_doc: &str,
    offset_units: &str,
    offset_doc: &str,
    origin_doc: &str,
) -> Vec<ParamSpec> {
    vec![
        ParamSpec::with("samples", None, samples_units, samples_doc),
        ParamSpec::with(
            "origin",
            Some(ParamValue::List(vec![
                ParamValue::Float(0.0),
                ParamValue::Float(0.0),
                ParamValue::Float(0.0),
            ])),
            "mm",
            origin_doc,
        ),
        ParamSpec::with(
            "spacing",
            Some(ParamValue::List(vec![
                ParamValue::Float(1.0),
                ParamValue::Float(1.0),
                ParamValue::Float(1.0),
            ])),
            "mm",
            "sample spacing per axis",
        ),
        ParamSpec::float("scale", 1.0, "-", "the sampled value is multiplied by this"),
        ParamSpec::float("offset", 0.0, offset_units, offset_doc),
    ]
}

const OFFSET_DOC: &str =
    "and this is SUBTRACTED after scaling: the iso-level, and for a distance field the offset";

static MESH: StaticInfo = StaticInfo::new();
static GRID: StaticInfo = StaticInfo::new();
static CELL: StaticInfo = StaticInfo::new();

impl Sampled {
    pub fn info_of(kind: SampledKind) -> &'static KindInfo {
        match kind {
            SampledKind::MeshSdf => MESH.get(|| {
                kind_info(
                    "mesh_sdf",
                    MODULE,
                    "A watertight triangle mesh as a node: its signed distance, sampled.",
                    "The MEASURED promise, or ``IMPLICIT`` if nobody measured.",
                    None,
                    false,
                    &[],
                    common_params(
                        "mm",
                        "signed distance at the sample points, NEGATIVE inside the part",
                        "mm",
                        "offset the surface outward by this; for a distance field f - r IS the offset, which is what EXACT and BOUND buy",
                        "position of sample [0, 0, 0]",
                    ),
                )
            }),
            SampledKind::GridField => GRID.get(|| {
                kind_info(
                    "grid_field",
                    MODULE,
                    "A sampled field somebody else computed: an optimiser's output, a",
                    "What the caller declared; ``IMPLICIT`` when nobody did.",
                    None,
                    false,
                    &[],
                    common_params("-", "the sampled field, shape (nx, ny, nz)", "-", OFFSET_DOC, "position of sample [0, 0, 0]"),
                )
            }),
            SampledKind::CellGridField => CELL.get(|| {
                kind_info(
                    "cell_grid_field",
                    MODULE,
                    "Cell-centred implicit field on its full physical cell domain.",
                    "What the caller declared; ``IMPLICIT`` when nobody did.",
                    None,
                    false,
                    &[],
                    common_params("-", "the sampled field, shape (nx, ny, nz)", "-", OFFSET_DOC, "lower boundary of the cell domain"),
                )
            }),
        }
    }

    fn dict_attr(a: Option<Attr>, kind: &str, what: &str) -> GResult<Vec<(String, Attr)>> {
        match a {
            None | Some(Attr::Null) => Ok(Vec::new()),
            Some(Attr::Dict(m)) => Ok(m),
            Some(other) if !other.truthy() => Ok(Vec::new()),
            Some(_) if what == "visualization" => {
                model_err(format!("{kind}: visualization must be a mapping when supplied"))
            }
            Some(other) => {
                Err(GeometryError::Value(format!("cannot convert {} to a dictionary", other.py_obj().repr())))
            }
        }
    }

    fn construct_with(kind: SampledKind, mut args: ConstructArgs) -> GResult<Node> {
        let kname = Self::info_of(kind).kind.clone();
        let fc = args.take_attr("field_class");
        let source = args.take_attr("source");
        let measurement = args.take_attr("measurement");
        let visualization = args.take_attr("visualization");
        args.attrs_into_params();

        Node::new(
            Arc::new(Self {
                kind,
                declared: None,
                source: Vec::new(),
                measurement: Vec::new(),
                visualization: Vec::new(),
            }),
            args.children.clone(),
            args.names.clone(),
            args.params.clone(),
        )?;
        let declared = match fc {
            None | Some(Attr::Null) => None,
            Some(Attr::FieldClass(f)) => Some(f),
            Some(Attr::Dict(d)) => {
                let j = Attr::Dict(d).to_json();
                Some(FieldClass::from_json(&j)?)
            }
            Some(other) => {
                let tn = match other {
                    Attr::Str(_) => "str",
                    Attr::Int(_) => "int",
                    Attr::Float(_) => "float",
                    Attr::Bool(_) => "bool",
                    Attr::List(_) => "list",
                    _ => "object",
                };
                return model_err(format!(
                    "{kname}: field_class must be a FieldClass or its dict form, got {}",
                    crate::pyfmt::str_repr(tn)
                ));
            }
        };
        let source = Self::dict_attr(source, &kname, "source")?;
        let measurement = Self::dict_attr(measurement, &kname, "measurement")?;
        let visualization = Self::dict_attr(visualization, &kname, "visualization")?;
        let op = Arc::new(Self { kind, declared, source, measurement, visualization });
        let mut node = Node::new(op, args.children, args.names, args.params)?;
        let Some(samples) = node.param("samples").cloned() else {
            return model_err(format!(
                "{kname} has no samples; it is a sampled field and there is nothing to interpolate"
            ));
        };
        let arr = samples.as_ndarray().map_err(|_| {
            GeometryError::Model(format!(
                "{kname}.samples must be a 3-D array with at least 2 points on each axis, got shape []"
            ))
        })?;
        if arr.ndim() != 3 || arr.shape().iter().copied().min().unwrap_or(0) < 2 {
            return model_err(format!(
                "{kname}.samples must be a 3-D array with at least 2 points on each axis, got shape {}",
                crate::value::json_to_pyobj(&serde_json::json!(arr.shape())).repr()
            ));
        }
        node.set_param_raw("samples", ParamValue::Array(Arc::new(arr)));
        for key in ["origin", "spacing"] {
            let v = node.param(key).cloned().unwrap_or(ParamValue::Float(0.0));
            let (_, mut data) = v
                .to_f64_array()
                .map_err(|_| GeometryError::Value(format!("could not convert {key} to float")))?;
            if data.len() == 1 {
                data = vec![data[0]; 3];
            }
            if data.len() != 3 {
                return model_err(format!("{kname}.{key} must be 3 numbers, got {}", data.len()));
            }
            node.set_param_raw(key, ParamValue::array_f64(vec![3], data).unwrap_or(ParamValue::Float(0.0)));
        }
        let h = node.param("spacing").and_then(|v| v.to_f64_array().ok()).map(|(_, d)| d).unwrap_or_default();
        if h.iter().any(|v| *v <= 0.0) {
            return model_err(format!("{kname}.spacing must be positive on every axis"));
        }
        Ok(node)
    }

    fn construct_mesh(args: ConstructArgs) -> GResult<Node> {
        Self::construct_with(SampledKind::MeshSdf, args)
    }
    fn construct_grid(args: ConstructArgs) -> GResult<Node> {
        Self::construct_with(SampledKind::GridField, args)
    }
    fn construct_cell(args: ConstructArgs) -> GResult<Node> {
        Self::construct_with(SampledKind::CellGridField, args)
    }


    pub fn sample_box(&self, node: &Node) -> GResult<([f64; 3], [f64; 3])> {
        let o = f3(node, "origin")?;
        let h = f3(node, "spacing")?;
        let shape = node
            .param("samples")
            .and_then(|v| v.as_ndarray().ok())
            .map(|a| a.shape().to_vec())
            .unwrap_or_default();
        if shape.len() != 3 {
            return Err(GeometryError::Value("samples are not three-dimensional".into()));
        }
        #[allow(clippy::cast_precision_loss)]
        let n: [f64; 3] = std::array::from_fn(|i| {
            if self.kind == SampledKind::CellGridField { shape[i] as f64 } else { shape[i] as f64 - 1.0 }
        });
        Ok((o, std::array::from_fn(|i| o[i] + h[i] * n[i])))
    }
}

fn f3(node: &Node, key: &str) -> GResult<[f64; 3]> {
    let (_, d) = node
        .param(key)
        .ok_or_else(|| GeometryError::Value(format!("missing {key}")))?
        .to_f64_array()
        .map_err(|_| GeometryError::Value(format!("could not convert {key} to float")))?;
    if d.len() != 3 {
        return Err(GeometryError::Value(format!("{key} must be 3 numbers")));
    }
    Ok([d[0], d[1], d[2]])
}

pub fn trilerp_clamped<S: Scalar>(
    samples: &[S],
    shape: [usize; 3],
    origin: [S; 3],
    spacing: [S; 3],
    x: [S; 3],
) -> S {
    let mut lo = [0usize; 3];
    let mut fr = [S::cst(0.0); 3];
    for a in 0..3 {
        let g = (x[a] - origin[a]) / spacing[a];
        #[allow(clippy::cast_precision_loss)]
        let gi = g.clip_c(0.0, (shape[a] - 1) as f64);
        let f0 = gi.val().floor();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let i0 = (f0.max(0.0) as usize).min(shape[a].saturating_sub(2));
        lo[a] = i0;
        #[allow(clippy::cast_precision_loss)]
        {
            fr[a] = gi - i0 as f64;
        }
    }
    let (i, j, k) = (lo[0], lo[1], lo[2]);
    let (i1, j1, k1) = ((i + 1).min(shape[0] - 1), (j + 1).min(shape[1] - 1), (k + 1).min(shape[2] - 1));
    let at = |a: usize, b: usize, c: usize| samples[(a * shape[1] + b) * shape[2] + c];
    let (u, v, w) = (fr[0], fr[1], fr[2]);
    let one = S::cst(1.0);
    let c00 = at(i, j, k) * (one - u) + at(i1, j, k) * u;
    let c10 = at(i, j1, k) * (one - u) + at(i1, j1, k) * u;
    let c01 = at(i, j, k1) * (one - u) + at(i1, j, k1) * u;
    let c11 = at(i, j1, k1) * (one - u) + at(i1, j1, k1) * u;
    let c0 = c00 * (one - v) + c10 * v;
    let c1 = c01 * (one - v) + c11 * v;
    c0 * (one - w) + c1 * w
}

struct SampledK<S> {
    kind: SampledKind,
    samples: Vec<S>,
    shape: [usize; 3],
    o: [S; 3],
    h: [S; 3],
    scale: S,
    offset: S,
}

impl<S: Scalar> Kernel<S> for SampledK<S> {
    fn eval(&self, p: [S; 3]) -> S {
        let (o, h) = (self.o, self.h);
        #[allow(clippy::cast_precision_loss)]
        let nf: [f64; 3] = std::array::from_fn(|i| self.shape[i] as f64);
        if self.kind == SampledKind::CellGridField {
            let c: [S; 3] = std::array::from_fn(|i| o[i] + h[i] * 0.5);
            let value = trilerp_clamped(&self.samples, self.shape, c, h, p) * self.scale - self.offset;
            let q: [S; 3] =
                std::array::from_fn(|i| (p[i] - (o[i] + h[i] * (0.5 * nf[i]))).abs() - h[i] * (0.5 * nf[i]));
            let out: [S; 3] = std::array::from_fn(|i| q[i].max_c(0.0));
            let bx = (out[0] * out[0] + out[1] * out[1] + out[2] * out[2] + 1e-24).sqrt()
                + reduce_max(&q).min_c(0.0);
            return value.max(bx);
        }
        let hi: [S; 3] = std::array::from_fn(|i| o[i] + h[i] * (nf[i] - 1.0));
        let q: [S; 3] = std::array::from_fn(|i| p[i].max(o[i]).min(hi[i]));
        let inside = trilerp_clamped(&self.samples, self.shape, o, h, q) * self.scale - self.offset;
        let d2 = (p[0] - q[0]).val().powi(2) + (p[1] - q[1]).val().powi(2) + (p[2] - q[2]).val().powi(2);
        if d2.sqrt() > 0.0 {
            let r = ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt();
            match self.kind {
                SampledKind::MeshSdf => r.max(inside - r),
                _ => inside + r,
            }
        } else {
            inside
        }
    }
}

impl NodeOp for Sampled {
    fn info(&self) -> &KindInfo {
        Self::info_of(self.kind)
    }
    fn doc_attrs(&self) -> Vec<(String, Attr)> {
        let mut out = Vec::new();
        if let Some(fc) = &self.declared {
            out.push(("field_class".into(), Attr::FieldClass(fc.clone())));
        }
        if !self.source.is_empty() {
            out.push(("source".into(), Attr::Dict(self.source.clone())));
        }
        if !self.measurement.is_empty() {
            out.push(("measurement".into(), Attr::Dict(self.measurement.clone())));
        }
        if !self.visualization.is_empty() {
            out.push(("visualization".into(), Attr::Dict(self.visualization.clone())));
        }
        out
    }
    fn field_class(&self, _n: &Node, _k: &[FieldClass]) -> GResult<FieldClass> {
        Ok(self.declared.clone().unwrap_or_else(FieldClass::implicit))
    }
    fn aabb(&self, n: &Node) -> GResult<Option<([f64; 3], [f64; 3])>> {
        if self.kind == SampledKind::MeshSdf {
            let (lo, hi) = self.sample_box(n)?;
            if (0..3).any(|axis| !lo[axis].is_finite() || !hi[axis].is_finite() || hi[axis] <= lo[axis]) {
                return Err(GeometryError::Value("mesh sample bounds must be finite with positive extents".into()));
            }
            Ok(Some((lo, hi)))
        } else if self.kind == SampledKind::CellGridField {
            self.sample_box(n).map(Some)
        } else {
            Ok(None)
        }
    }
    fn render_sampling_spacing_mm(&self, n: &Node) -> Option<f64> {
        f3(n, "spacing").ok().map(|h| h[0].min(h[1]).min(h[2]))
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        _k: Vec<KernelBox<S>>,
        _c: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        let s = inp.array("samples")?;
        if s.shape.len() != 3 {
            return model_err("samples must be three-dimensional");
        }
        let o = inp.array("origin")?;
        let h = inp.array("spacing")?;
        if o.data.len() != 3 || h.data.len() != 3 {
            return model_err("origin and spacing must be 3 numbers");
        }
        Ok(Box::new(SampledK {
            kind: self.kind,
            samples: s.data.clone(),
            shape: [s.shape[0], s.shape[1], s.shape[2]],
            o: [o.data[0], o.data[1], o.data[2]],
            h: [h.data[0], h.data[1], h.data[2]],
            scale: inp.scalar("scale")?,
            offset: inp.scalar("offset")?,
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}


pub fn grid_field(
    samples: crate::value::NdArray,
    origin: [f64; 3],
    spacing: [f64; 3],
    field_class: Option<FieldClass>,
    source: Vec<(String, Attr)>,
) -> GResult<Node> {
    let mut params = BTreeMap::new();
    params.insert("samples".to_string(), ParamValue::Array(Arc::new(samples)));
    params.insert(
        "origin".to_string(),
        ParamValue::List(origin.iter().map(|v| ParamValue::Float(*v)).collect()),
    );
    params.insert(
        "spacing".to_string(),
        ParamValue::List(spacing.iter().map(|v| ParamValue::Float(*v)).collect()),
    );
    let mut attrs = BTreeMap::new();
    if let Some(fc) = field_class {
        attrs.insert("field_class".to_string(), Attr::FieldClass(fc));
    }
    if !source.is_empty() {
        attrs.insert("source".to_string(), Attr::Dict(source));
    }
    Sampled::construct_with(
        SampledKind::GridField,
        ConstructArgs { children: Vec::new(), names: None, params, attrs },
    )
}

#[must_use]
pub fn entries() -> KernelEntryList {
    vec![
        entry(Sampled::info_of(SampledKind::CellGridField), Sampled::construct_cell),
        entry(Sampled::info_of(SampledKind::GridField), Sampled::construct_grid),
        entry(Sampled::info_of(SampledKind::MeshSdf), Sampled::construct_mesh),
    ]
}
