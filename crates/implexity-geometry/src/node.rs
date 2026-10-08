// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, OnceLock, RwLock};

use sha2::{Digest, Sha256};

use crate::error::{GResult, GeometryError, model_err};
use crate::fieldclass::FieldClass;
use crate::pyfmt::{self, PyObj};
use crate::scalar::{Dual, Rv, Scalar};
use crate::value::{NdArray, ParamValue, json_f64};

pub type NodeRef = Arc<Node>;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ParamRef {
    pub path: Vec<String>,
    pub name: String,
}

impl ParamRef {
    #[must_use]
    pub fn new(path: Vec<String>, name: impl Into<String>) -> Self {
        Self { path, name: name.into() }
    }

    #[must_use]
    pub fn as_str(&self) -> String {
        format!("{}:{}", self.path.join("/"), self.name)
    }

    #[must_use]
    pub fn path_key(&self) -> String {
        self.path.join("/")
    }


    pub fn parse(s: &str) -> GResult<Self> {
        let (path, name) = match s.rfind(':') {
            Some(i) => (&s[..i], &s[i + 1..]),
            None => ("", s),
        };
        if name.is_empty() {
            return model_err(format!("parameter reference {}; expected 'a/b:name'", pyfmt::str_repr(s)));
        }
        Ok(Self {
            path: path.split('/').filter(|p| !p.is_empty()).map(str::to_string).collect(),
            name: name.to_string(),
        })
    }
}

impl fmt::Display for ParamRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let p = self.path.join("/");
        write!(f, "ParamRef({}.{})", if p.is_empty() { "<root>" } else { &p }, self.name)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Attr {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Attr>),
    Dict(Vec<(String, Attr)>),
    Ref(ParamRef),
    FieldClass(FieldClass),
    Array(Arc<NdArray>),
}

impl Attr {
    #[must_use]
    pub fn from_json(v: &serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Bool(b) => Self::Bool(*b),
            serde_json::Value::Number(n) => {
                n.as_i64().map_or_else(|| Self::Float(n.as_f64().unwrap_or(f64::NAN)), Self::Int)
            }
            serde_json::Value::String(s) => Self::Str(s.clone()),
            serde_json::Value::Array(a) => Self::List(a.iter().map(Self::from_json).collect()),
            serde_json::Value::Object(m) => {
                Self::Dict(m.iter().map(|(k, v)| (k.clone(), Self::from_json(v))).collect())
            }
        }
    }

    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Null => serde_json::Value::Null,
            Self::Bool(b) => serde_json::Value::from(*b),
            Self::Int(i) => serde_json::Value::from(*i),
            Self::Float(f) => json_f64(*f),
            Self::Str(s) => serde_json::Value::from(s.clone()),
            Self::List(v) => serde_json::Value::Array(v.iter().map(Self::to_json).collect()),
            Self::Dict(m) => {
                serde_json::Value::Object(m.iter().map(|(k, v)| (k.clone(), v.to_json())).collect())
            }
            Self::Ref(r) => serde_json::json!({"$ref": r.as_str()}),
            Self::FieldClass(fc) => serde_json::json!({"$fieldclass": fc.as_json()}),
            Self::Array(a) => a.to_json_list(),
        }
    }

    #[must_use]
    pub fn py_obj(&self) -> PyObj {
        match self {
            Self::Null => PyObj::None,
            Self::Bool(b) => PyObj::Bool(*b),
            Self::Int(i) => PyObj::Int(*i),
            Self::Float(f) => PyObj::Float(*f),
            Self::Str(s) => PyObj::Str(s.clone()),
            Self::List(v) => PyObj::List(v.iter().map(Self::py_obj).collect()),
            Self::Dict(m) => PyObj::Dict(m.iter().map(|(k, v)| (k.clone(), v.py_obj())).collect()),
            Self::Ref(r) => PyObj::Str(r.to_string()),
            Self::FieldClass(fc) => PyObj::Str(fc.repr()),
            Self::Array(a) => crate::value::json_to_pyobj(&a.to_json_list()),
        }
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Float(f) => Some(*f),
            #[allow(clippy::cast_precision_loss)]
            Self::Int(i) => Some(*i as f64),
            Self::Bool(b) => Some(f64::from(u8::from(*b))),
            _ => None,
        }
    }

    #[must_use]
    pub fn truthy(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(b) => *b,
            Self::Int(i) => *i != 0,
            Self::Float(f) => *f != 0.0,
            Self::Str(s) => !s.is_empty(),
            Self::List(v) => !v.is_empty(),
            Self::Dict(m) => !m.is_empty(),
            Self::Ref(_) | Self::FieldClass(_) => true,
            Self::Array(a) => a.size() > 0,
        }
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Attr> {
        match self {
            Self::Dict(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ParamSpec {
    pub name: String,
    pub default: Option<ParamValue>,
    pub units: String,
    pub doc: String,
}

impl ParamSpec {
    #[must_use]
    pub fn float(name: &str, default: f64, units: &str, doc: &str) -> Self {
        Self {
            name: name.into(),
            default: Some(ParamValue::Float(default)),
            units: units.into(),
            doc: doc.into(),
        }
    }

    #[must_use]
    pub fn with(name: &str, default: Option<ParamValue>, units: &str, doc: &str) -> Self {
        Self { name: name.into(), default, units: units.into(), doc: doc.into() }
    }
}

#[derive(Clone, Debug)]
pub struct KindInfo {
    pub kind: String,
    pub params: Vec<ParamSpec>,
    pub discrete: Vec<String>,
    pub module: String,
    pub doc: String,
    pub field_class_doc: String,
    pub leaf_class: Option<FieldClass>,
    pub is_struct: bool,
    pub glsl_refusal: Option<String>,
}

impl KindInfo {
    #[must_use]
    pub fn param(&self, name: &str) -> Option<&ParamSpec> {
        self.params.iter().find(|p| p.name == name)
    }

    #[must_use]
    pub fn sorted_param_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.params.iter().map(|p| p.name.clone()).collect();
        v.sort();
        v
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Exact,
    Smooth,
}

impl Mode {

    pub fn parse(s: &str) -> GResult<Self> {
        match s {
            "exact" => Ok(Self::Exact),
            "smooth" => Ok(Self::Smooth),
            _ => Err(GeometryError::Value(format!(
                "mode {}; expected 'exact' or 'smooth'",
                pyfmt::str_repr(s)
            ))),
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Smooth => "smooth",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SmoothKind {
    Poly,
    Exp,
}

impl SmoothKind {

    pub fn parse(s: &str) -> GResult<Self> {
        match s {
            "poly" => Ok(Self::Poly),
            "exp" => Ok(Self::Exp),
            _ => Err(GeometryError::Value(format!(
                "smooth_kind {}; expected 'poly' or 'exp'",
                pyfmt::str_repr(s)
            ))),
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Poly => "poly",
            Self::Exp => "exp",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EvalCtx {
    pub mode: Mode,
    pub smooth_kind: SmoothKind,
    pub smooth_r: f64,
}

impl EvalCtx {
    #[must_use]
    pub fn smooth(&self) -> bool {
        self.mode == Mode::Smooth
    }
}

pub trait Kernel<S: Scalar>: Send + Sync {
    fn eval(&self, x: [S; 3]) -> S;

    fn eval_revolved_profile(&self, r: S, z: S) -> S {
        self.eval([r, z, S::cst(0.0)])
    }
}

pub type KernelBox<S> = Box<dyn Kernel<S>>;

#[derive(Clone, Debug)]
pub struct ParamS<S> {
    pub shape: Vec<usize>,
    pub data: Vec<S>,
}

impl<S: Scalar> ParamS<S> {

    pub fn scalar(&self, name: &str) -> GResult<S> {
        if self.data.len() == 1 {
            Ok(self.data[0])
        } else {
            Err(GeometryError::Value(format!(
                "parameter {} is an array of shape {}, where a scalar is required",
                pyfmt::str_repr(name),
                pyfmt::shape_str(&self.shape)
            )))
        }
    }
}

#[derive(Clone, Debug)]
pub struct KernelInputs<S> {
    pub params: Vec<(String, ParamS<S>)>,
    pub derived: Vec<ParamS<S>>,
}

impl<S: Scalar> KernelInputs<S> {

    pub fn array(&self, name: &str) -> GResult<&ParamS<S>> {
        self.params
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, p)| p)
            .ok_or_else(|| GeometryError::Model(format!("no parameter {}", pyfmt::str_repr(name))))
    }


    pub fn scalar(&self, name: &str) -> GResult<S> {
        self.array(name)?.scalar(name)
    }


    pub fn derived(&self, i: usize) -> GResult<&ParamS<S>> {
        self.derived.get(i).ok_or_else(|| GeometryError::Model(format!("missing derived array {i}")))
    }
}

pub type DerivedVjp = Box<dyn Fn(&[Vec<f64>]) -> GResult<Vec<(String, Vec<f64>)>> + Send + Sync>;

#[derive(Default)]
pub struct Prepared {
    pub derived: Vec<(Vec<usize>, Vec<f64>)>,
    pub vjp: Option<DerivedVjp>,
}

pub trait NodeOp: Send + Sync + 'static {
    fn info(&self) -> &KindInfo;

    fn struct_tokens(&self) -> Vec<String> {
        Vec::new()
    }

    fn struct_json(&self) -> Vec<(String, serde_json::Value)> {
        Vec::new()
    }


    fn field_class(&self, node: &Node, kids: &[FieldClass]) -> GResult<FieldClass>;


    fn field_class_smooth(&self, node: &Node, kids: &[FieldClass]) -> GResult<FieldClass> {
        self.field_class(node, kids)
    }


    fn aabb(&self, _node: &Node) -> GResult<Option<([f64; 3], [f64; 3])>> {
        Ok(None)
    }


    fn validate(&self, _node: &Node) -> GResult<Option<String>> {
        Ok(None)
    }

    fn doc_attrs(&self) -> Vec<(String, Attr)> {
        Vec::new()
    }


    fn prepare(&self, _node: &Node, _want_vjp: bool) -> GResult<Prepared> {
        Ok(Prepared::default())
    }


    fn kernel<S: Scalar>(
        &self,
        node: &Node,
        inputs: &KernelInputs<S>,
        kids: Vec<KernelBox<S>>,
        ctx: &EvalCtx,
    ) -> GResult<KernelBox<S>>
    where
        Self: Sized;

    fn render_sampling_scale(&self, _node: &Node) -> Option<f64> {
        None
    }

    fn render_sampling_spacing_mm(&self, _node: &Node) -> Option<f64> {
        None
    }

    fn glsl_refusal(&self) -> Option<String> {
        None
    }

    fn occupancy_source(&self) -> Option<&dyn crate::occupancy::OccupancySource> {
        None
    }


    fn render_field_descriptors(&self, node: &Node) -> Option<GResult<Vec<serde_json::Value>>> {
        self.occupancy_source().map(|s| s.render_field_descriptors(node))
    }


    fn render_derived_grids(
        &self,
        node: &Node,
        names: &[String],
    ) -> Option<GResult<Vec<crate::occupancy::DerivedGrid>>> {
        self.occupancy_source().map(|s| s.render_derived_grids(node, names))
    }

    fn as_any(&self) -> &dyn Any;
}

pub trait ErasedOp: Send + Sync + 'static {
    fn info(&self) -> &KindInfo;
    fn struct_tokens(&self) -> Vec<String>;
    fn struct_json(&self) -> Vec<(String, serde_json::Value)>;

    fn field_class(&self, node: &Node, kids: &[FieldClass]) -> GResult<FieldClass>;

    fn field_class_smooth(&self, node: &Node, kids: &[FieldClass]) -> GResult<FieldClass>;

    fn aabb(&self, node: &Node) -> GResult<Option<([f64; 3], [f64; 3])>>;

    fn validate(&self, node: &Node) -> GResult<Option<String>>;
    fn doc_attrs(&self) -> Vec<(String, Attr)>;

    fn prepare(&self, node: &Node, want_vjp: bool) -> GResult<Prepared>;

    fn kernel_f64(
        &self,
        node: &Node,
        inputs: &KernelInputs<f64>,
        kids: Vec<KernelBox<f64>>,
        ctx: &EvalCtx,
    ) -> GResult<KernelBox<f64>>;

    fn kernel_dual3(
        &self,
        node: &Node,
        inputs: &KernelInputs<Dual<3>>,
        kids: Vec<KernelBox<Dual<3>>>,
        ctx: &EvalCtx,
    ) -> GResult<KernelBox<Dual<3>>>;

    fn kernel_dual4(
        &self,
        node: &Node,
        inputs: &KernelInputs<Dual<4>>,
        kids: Vec<KernelBox<Dual<4>>>,
        ctx: &EvalCtx,
    ) -> GResult<KernelBox<Dual<4>>>;

    fn kernel_rv(
        &self,
        node: &Node,
        inputs: &KernelInputs<Rv>,
        kids: Vec<KernelBox<Rv>>,
        ctx: &EvalCtx,
    ) -> GResult<KernelBox<Rv>>;
    fn render_sampling_scale(&self, node: &Node) -> Option<f64>;
    fn render_sampling_spacing_mm(&self, node: &Node) -> Option<f64>;
    fn glsl_refusal(&self) -> Option<String>;
    fn occupancy_source(&self) -> Option<&dyn crate::occupancy::OccupancySource>;
    fn render_field_descriptors(&self, node: &Node) -> Option<GResult<Vec<serde_json::Value>>>;
    fn render_derived_grids(
        &self,
        node: &Node,
        names: &[String],
    ) -> Option<GResult<Vec<crate::occupancy::DerivedGrid>>>;
    fn as_any(&self) -> &dyn Any;
}

impl<T: NodeOp> ErasedOp for T {
    fn info(&self) -> &KindInfo {
        NodeOp::info(self)
    }
    fn struct_tokens(&self) -> Vec<String> {
        NodeOp::struct_tokens(self)
    }
    fn struct_json(&self) -> Vec<(String, serde_json::Value)> {
        NodeOp::struct_json(self)
    }
    fn field_class(&self, node: &Node, kids: &[FieldClass]) -> GResult<FieldClass> {
        NodeOp::field_class(self, node, kids)
    }
    fn field_class_smooth(&self, node: &Node, kids: &[FieldClass]) -> GResult<FieldClass> {
        NodeOp::field_class_smooth(self, node, kids)
    }
    fn aabb(&self, node: &Node) -> GResult<Option<([f64; 3], [f64; 3])>> {
        NodeOp::aabb(self, node)
    }
    fn validate(&self, node: &Node) -> GResult<Option<String>> {
        NodeOp::validate(self, node)
    }
    fn doc_attrs(&self) -> Vec<(String, Attr)> {
        NodeOp::doc_attrs(self)
    }
    fn prepare(&self, node: &Node, want_vjp: bool) -> GResult<Prepared> {
        NodeOp::prepare(self, node, want_vjp)
    }
    fn kernel_f64(
        &self,
        node: &Node,
        inputs: &KernelInputs<f64>,
        kids: Vec<KernelBox<f64>>,
        ctx: &EvalCtx,
    ) -> GResult<KernelBox<f64>> {
        NodeOp::kernel::<f64>(self, node, inputs, kids, ctx)
    }
    fn kernel_dual3(
        &self,
        node: &Node,
        inputs: &KernelInputs<Dual<3>>,
        kids: Vec<KernelBox<Dual<3>>>,
        ctx: &EvalCtx,
    ) -> GResult<KernelBox<Dual<3>>> {
        NodeOp::kernel::<Dual<3>>(self, node, inputs, kids, ctx)
    }
    fn kernel_dual4(
        &self,
        node: &Node,
        inputs: &KernelInputs<Dual<4>>,
        kids: Vec<KernelBox<Dual<4>>>,
        ctx: &EvalCtx,
    ) -> GResult<KernelBox<Dual<4>>> {
        NodeOp::kernel::<Dual<4>>(self, node, inputs, kids, ctx)
    }
    fn kernel_rv(
        &self,
        node: &Node,
        inputs: &KernelInputs<Rv>,
        kids: Vec<KernelBox<Rv>>,
        ctx: &EvalCtx,
    ) -> GResult<KernelBox<Rv>> {
        NodeOp::kernel::<Rv>(self, node, inputs, kids, ctx)
    }
    fn render_sampling_scale(&self, node: &Node) -> Option<f64> {
        NodeOp::render_sampling_scale(self, node)
    }
    fn render_sampling_spacing_mm(&self, node: &Node) -> Option<f64> {
        NodeOp::render_sampling_spacing_mm(self, node)
    }
    fn glsl_refusal(&self) -> Option<String> {
        NodeOp::glsl_refusal(self)
    }
    fn occupancy_source(&self) -> Option<&dyn crate::occupancy::OccupancySource> {
        NodeOp::occupancy_source(self)
    }
    fn render_field_descriptors(&self, node: &Node) -> Option<GResult<Vec<serde_json::Value>>> {
        NodeOp::render_field_descriptors(self, node)
    }
    fn render_derived_grids(
        &self,
        node: &Node,
        names: &[String],
    ) -> Option<GResult<Vec<crate::occupancy::DerivedGrid>>> {
        NodeOp::render_derived_grids(self, node, names)
    }
    fn as_any(&self) -> &dyn Any {
        NodeOp::as_any(self)
    }
}

pub trait KernelScalar: Scalar {

    fn build(
        op: &dyn ErasedOp,
        node: &Node,
        inputs: &KernelInputs<Self>,
        kids: Vec<KernelBox<Self>>,
        ctx: &EvalCtx,
    ) -> GResult<KernelBox<Self>>;
}

impl KernelScalar for f64 {
    fn build(
        op: &dyn ErasedOp,
        node: &Node,
        inputs: &KernelInputs<Self>,
        kids: Vec<KernelBox<Self>>,
        ctx: &EvalCtx,
    ) -> GResult<KernelBox<Self>> {
        op.kernel_f64(node, inputs, kids, ctx)
    }
}
impl KernelScalar for Dual<3> {
    fn build(
        op: &dyn ErasedOp,
        node: &Node,
        inputs: &KernelInputs<Self>,
        kids: Vec<KernelBox<Self>>,
        ctx: &EvalCtx,
    ) -> GResult<KernelBox<Self>> {
        op.kernel_dual3(node, inputs, kids, ctx)
    }
}
impl KernelScalar for Dual<4> {
    fn build(
        op: &dyn ErasedOp,
        node: &Node,
        inputs: &KernelInputs<Self>,
        kids: Vec<KernelBox<Self>>,
        ctx: &EvalCtx,
    ) -> GResult<KernelBox<Self>> {
        op.kernel_dual4(node, inputs, kids, ctx)
    }
}
impl KernelScalar for Rv {
    fn build(
        op: &dyn ErasedOp,
        node: &Node,
        inputs: &KernelInputs<Self>,
        kids: Vec<KernelBox<Self>>,
        ctx: &EvalCtx,
    ) -> GResult<KernelBox<Self>> {
        op.kernel_rv(node, inputs, kids, ctx)
    }
}

pub struct Node {
    op: Arc<dyn ErasedOp>,
    children: Vec<NodeRef>,
    names: Vec<String>,
    params: BTreeMap<String, ParamValue>,
}

impl fmt::Debug for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{} {}>", self.kind(), self.structure_id())
    }
}

impl Node {

    pub fn new(
        op: Arc<dyn ErasedOp>,
        children: Vec<NodeRef>,
        names: Option<Vec<String>>,
        mut params: BTreeMap<String, ParamValue>,
    ) -> GResult<Self> {
        let info = op.info();
        let names = names.unwrap_or_else(|| (0..children.len()).map(|i| format!("c{i}")).collect());
        if names.len() != children.len() {
            return model_err(format!(
                "{}: {} child name(s) for {} child(ren)",
                info.kind,
                names.len(),
                children.len()
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        if !names.iter().all(|n| seen.insert(n.clone())) {
            let tuple = PyObj::Tuple(names.iter().map(|n| PyObj::Str(n.clone())).collect());
            return model_err(format!("{}: duplicate child names {}", info.kind, tuple.repr()));
        }
        let mut unknown: Vec<String> = params.keys().filter(|k| info.param(k).is_none()).cloned().collect();
        if !unknown.is_empty() {
            unknown.sort();
            let have = info.sorted_param_names();
            return model_err(format!(
                "{} has no parameter {}; it has {}",
                info.kind,
                unknown.join(", "),
                if have.is_empty() { "none".to_string() } else { have.join(", ") }
            ));
        }
        for spec in &info.params {
            if !params.contains_key(&spec.name) {

                let v = spec.default.clone().unwrap_or(ParamValue::Str(String::new()));
                if spec.default.is_some() {
                    params.insert(spec.name.clone(), v);
                }
            }
        }
        Ok(Self { op, children, names, params })
    }

    #[must_use]
    pub fn with_params(&self, params: BTreeMap<String, ParamValue>) -> Self {
        Self { op: Arc::clone(&self.op), children: self.children.clone(), names: self.names.clone(), params }
    }

    #[must_use]
    pub fn with_children(&self, children: Vec<NodeRef>) -> Self {
        Self { op: Arc::clone(&self.op), children, names: self.names.clone(), params: self.params.clone() }
    }

    pub fn set_param_raw(&mut self, name: &str, value: ParamValue) {
        self.params.insert(name.to_string(), value);
    }

    #[must_use]
    pub fn op(&self) -> &dyn ErasedOp {
        self.op.as_ref()
    }

    #[must_use]
    pub fn op_arc(&self) -> Arc<dyn ErasedOp> {
        Arc::clone(&self.op)
    }

    #[must_use]
    pub fn kind(&self) -> &str {
        &self.op.info().kind
    }

    #[must_use]
    pub fn info(&self) -> &KindInfo {
        self.op.info()
    }

    #[must_use]
    pub fn children(&self) -> &[NodeRef] {
        &self.children
    }

    #[must_use]
    pub fn child_names(&self) -> &[String] {
        &self.names
    }

    pub fn named_children(&self) -> impl Iterator<Item = (&String, &NodeRef)> {
        self.names.iter().zip(self.children.iter())
    }

    #[must_use]
    pub fn params(&self) -> &BTreeMap<String, ParamValue> {
        &self.params
    }

    #[must_use]
    pub fn param(&self, name: &str) -> Option<&ParamValue> {
        self.params.get(name)
    }


    pub fn pf(&self, name: &str) -> GResult<f64> {
        self.params.get(name).and_then(ParamValue::scalar_f64).ok_or_else(|| {
            GeometryError::Value("only length-1 arrays can be converted to Python scalars".to_string())
        })
    }

    #[must_use]
    pub fn walk(self: &Arc<Self>) -> Vec<(Vec<String>, NodeRef)> {
        let mut out = Vec::new();
        fn rec(n: &NodeRef, path: &mut Vec<String>, out: &mut Vec<(Vec<String>, NodeRef)>) {
            out.push((path.clone(), Arc::clone(n)));
            for (name, c) in n.named_children() {
                path.push(name.clone());
                rec(c, path, out);
                path.pop();
            }
        }
        rec(self, &mut Vec::new(), &mut out);
        out
    }


    pub fn at(self: &Arc<Self>, path: &[String]) -> GResult<NodeRef> {
        let mut n = Arc::clone(self);
        for step in path {
            let Some(i) = n.names.iter().position(|x| x == step) else {
                return model_err(format!(
                    "no child {} under {} (has {})",
                    pyfmt::str_repr(step),
                    n.kind(),
                    if n.names.is_empty() { "no children".to_string() } else { n.names.join(", ") }
                ));
            };
            n = Arc::clone(&n.children[i]);
        }
        Ok(n)
    }

    #[must_use]
    pub fn param_refs(self: &Arc<Self>) -> Vec<ParamRef> {
        let mut out = Vec::new();
        for (path, node) in self.walk() {
            for name in node.info().sorted_param_names() {
                out.push(ParamRef::new(path.clone(), name));
            }
        }
        out
    }


    pub fn get_param(self: &Arc<Self>, r: &ParamRef) -> GResult<ParamValue> {
        let node = self.at(&r.path)?;
        if node.info().param(&r.name).is_none() {
            let have = node.info().sorted_param_names();
            return model_err(format!(
                "{} has no parameter {}; it has {}",
                node.kind(),
                pyfmt::str_repr(&r.name),
                if have.is_empty() { "none".to_string() } else { have.join(", ") }
            ));
        }
        Ok(node.params.get(&r.name).cloned().unwrap_or(ParamValue::Str(String::new())))
    }

    fn structure_tokens(&self, out: &mut Vec<String>) {
        out.push(self.kind().to_string());
        out.extend(self.op.struct_tokens());
        for (name, c) in self.named_children() {
            out.push(format!("({name}"));
            c.structure_tokens(out);
            out.push(")".into());
        }
    }

    #[must_use]
    pub fn structure_id(&self) -> String {
        let mut tokens = Vec::new();
        self.structure_tokens(&mut tokens);
        let mut h = Sha256::new();
        for t in &tokens {
            h.update(t.as_bytes());
            h.update([0x1f]);
        }
        hex::encode(h.finalize())[..16].to_string()
    }

    #[must_use]
    pub fn content_id(self: &Arc<Self>) -> String {
        let mut h = Sha256::new();
        h.update(self.structure_id().as_bytes());
        for (path, node) in self.walk() {
            h.update(path.join("/").as_bytes());
            for (k, v) in &node.params {
                h.update(k.as_bytes());
                h.update(v.value_bytes());
            }
        }
        hex::encode(h.finalize())[..16].to_string()
    }

    #[must_use]
    pub fn describe(&self) -> serde_json::Value {
        let mut params = serde_json::Map::new();
        for (k, v) in &self.params {
            params.insert(k.clone(), jsonable(v));
        }
        let children: Vec<serde_json::Value> = self
            .named_children()
            .map(|(n, c)| serde_json::json!({"name": n, "node": c.describe()}))
            .collect();
        let mut out = serde_json::Map::new();
        out.insert("kind".into(), serde_json::Value::from(self.kind()));
        out.insert("params".into(), serde_json::Value::Object(params));
        out.insert("children".into(), serde_json::Value::Array(children));
        let st = self.op.struct_json();
        if !st.is_empty() {
            out.insert("struct".into(), serde_json::Value::Object(st.into_iter().collect()));
        }
        serde_json::Value::Object(out)
    }
}

#[must_use]
pub fn jsonable(v: &ParamValue) -> serde_json::Value {
    match v.as_ndarray() {
        Ok(a) if a.ndim() == 0 => match v {
            ParamValue::Array(_) => {
                let d = a.to_f64_vec();
                match a.dtype() {
                    crate::value::DType::F64 | crate::value::DType::F32 => json_f64(d[0]),
                    crate::value::DType::Bool => serde_json::Value::from(d[0] != 0.0),
                    #[allow(clippy::cast_possible_truncation)]
                    _ => serde_json::Value::from(d[0] as i64),
                }
            }
            other => other.to_json(),
        },
        Ok(a) => {
            let d = a.to_f64_vec();
            let (mn, mx) = d.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), x| {
                if x.is_nan() || lo.is_nan() { (f64::NAN, f64::NAN) } else { (lo.min(*x), hi.max(*x)) }
            });
            #[allow(clippy::cast_precision_loss)]
            let mean = crate::numpy::sum(&d) / d.len() as f64;
            serde_json::json!({
                "shape": a.shape(),
                "dtype": a.dtype().name(),
                "min": json_f64(mn),
                "max": json_f64(mx),
                "mean": json_f64(mean),
            })
        }
        Err(_) => match v {
            ParamValue::Str(s) => serde_json::Value::from(s.clone()),
            other => other.to_json(),
        },
    }
}

pub struct ConstructArgs {
    pub children: Vec<NodeRef>,
    pub names: Option<Vec<String>>,
    pub params: BTreeMap<String, ParamValue>,
    pub attrs: BTreeMap<String, Attr>,
}

impl ConstructArgs {
    pub fn take_attr(&mut self, name: &str) -> Option<Attr> {
        self.attrs.remove(name)
    }

    pub fn attrs_into_params(&mut self) {
        let attrs = std::mem::take(&mut self.attrs);
        for (k, v) in attrs {
            let pv = attr_to_param(&v);
            self.params.insert(k, pv);
        }
    }
}

#[must_use]
pub fn attr_to_param(a: &Attr) -> ParamValue {
    match a {
        Attr::Null => ParamValue::Str(String::new()),
        Attr::Bool(b) => ParamValue::Bool(*b),
        Attr::Int(i) => ParamValue::Int(*i),
        Attr::Float(f) => ParamValue::Float(*f),
        Attr::Str(s) => ParamValue::Str(s.clone()),
        Attr::List(v) => ParamValue::List(v.iter().map(attr_to_param).collect()),
        Attr::Array(a) => ParamValue::Array(Arc::clone(a)),
        other => ParamValue::Str(other.py_obj().repr()),
    }
}

pub type KernelEntryList = Vec<KindEntry>;

pub type Constructor = Arc<dyn Fn(ConstructArgs) -> GResult<Node> + Send + Sync>;

#[derive(Clone)]
pub struct KindEntry {
    pub info: Arc<KindInfo>,
    pub construct: Constructor,
}

#[derive(Clone, Default)]
pub struct Registry {
    kinds: BTreeMap<String, KindEntry>,
}

impl Registry {
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }


    pub fn register(&mut self, entry: KindEntry) -> GResult<()> {
        let kind = entry.info.kind.clone();
        if self.kinds.contains_key(&kind) {
            return model_err(format!("node kind {} is already registered", pyfmt::str_repr(&kind)));
        }
        self.kinds.insert(kind, entry);
        Ok(())
    }

    pub fn unregister(&mut self, kind: &str) -> bool {
        self.kinds.remove(kind).is_some()
    }

    #[must_use]
    pub fn get(&self, kind: &str) -> Option<&KindEntry> {
        self.kinds.get(kind)
    }


    pub fn require(&self, kind: &str) -> GResult<&KindEntry> {
        self.kinds.get(kind).ok_or_else(|| {
            GeometryError::Model(format!(
                "no node kind {}; known kinds are {}",
                pyfmt::str_repr(kind),
                if self.kinds.is_empty() { "none".to_string() } else { self.names().join(", ") }
            ))
        })
    }

    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.kinds.keys().cloned().collect()
    }

    #[must_use]
    pub fn contains(&self, kind: &str) -> bool {
        self.kinds.contains_key(kind)
    }


    pub fn construct(&self, kind: &str, args: ConstructArgs) -> GResult<Node> {
        (self.require(kind)?.construct)(args)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &KindEntry)> {
        self.kinds.iter()
    }
}

fn global() -> &'static RwLock<Registry> {
    static REG: OnceLock<RwLock<Registry>> = OnceLock::new();
    REG.get_or_init(|| RwLock::new(crate::kernel_registry()))
}

#[must_use]
pub fn registry() -> Registry {
    global().read().map_or_else(|p| p.into_inner().clone(), |r| r.clone())
}


pub fn register_kind(entry: KindEntry) -> GResult<()> {
    let mut w = global().write().map_err(|_| GeometryError::Model("node registry lock poisoned".into()))?;
    w.register(entry)
}

#[must_use]
pub fn unregister_kind(kind: &str) -> bool {
    global().write().is_ok_and(|mut w| w.unregister(kind))
}


pub fn make(
    kind: &str,
    children: Vec<NodeRef>,
    names: Option<Vec<&str>>,
    params: &[(&str, ParamValue)],
    attrs: &[(&str, Attr)],
) -> GResult<NodeRef> {
    let args = ConstructArgs {
        children,
        names: names.map(|n| n.into_iter().map(str::to_string).collect()),
        params: params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect(),
        attrs: attrs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect(),
    };
    registry().construct(kind, args).map(Arc::new)
}
