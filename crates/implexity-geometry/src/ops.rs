// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::sync::Arc;

use crate::error::{GResult, GeometryError, model_err};
use crate::eval::{SAFE_EPS, aabb_of, disjoint};
use crate::fieldclass::{ClassKind, FieldClass};
use crate::kinds::{StaticInfo, entry, kind_info, smooth_max, smooth_min, vlen2};
use crate::node::{
    Attr, ConstructArgs, ErasedOp, EvalCtx, Kernel, KernelBox, KernelEntryList, KernelInputs, KindEntry,
    KindInfo, Node, NodeOp, ParamSpec,
};
use crate::pyfmt::{self, PyObj};
use crate::scalar::{Scalar, reduce_min};

const MODULE: &str = "implexity.implicit.ops";

pub const ROOT_HALF: f64 = std::f64::consts::FRAC_1_SQRT_2;

type Bounds = Option<([f64; 3], [f64; 3])>;


pub fn weakest(kids: &[FieldClass], floor: Option<ClassKind>) -> GResult<FieldClass> {
    let Some(first) = kids.first() else {
        return Err(GeometryError::Value("tuple index out of range".into()));
    };
    let mut out = first.clone();
    for c in &kids[1..] {
        out = out.weaker_of(c);
    }
    if let Some(f) = floor
        && out.rank() < f.rank()
    {
        out = out.demoted(f, "")?;
    }
    Ok(out)
}


pub fn times(fc: &FieldClass, factor: f64, note: &str) -> GResult<FieldClass> {
    if fc.kind() == ClassKind::Implicit || (factor - 1.0).abs() < 1e-12 {
        return Ok(fc.clone());
    }
    fc.scaled(factor, note)
}

fn first(kids: &[FieldClass]) -> GResult<FieldClass> {
    kids.first().cloned().ok_or_else(|| GeometryError::Value("tuple index out of range".into()))
}

fn hull(a: Bounds, b: Bounds) -> Bounds {
    let (a, b) = (a?, b?);
    Some((std::array::from_fn(|i| a.0[i].min(b.0[i])), std::array::from_fn(|i| a.1[i].max(b.1[i]))))
}

fn build_op(op: Arc<dyn ErasedOp>, mut args: ConstructArgs, arity: Option<usize>) -> GResult<Node> {
    args.attrs_into_params();
    let node = Node::new(op, args.children, args.names, args.params)?;
    if let Some(a) = arity
        && node.children().len() != a
    {
        return model_err(format!("{} takes {} child(ren), got {}", node.kind(), a, node.children().len()));
    }
    if let Some(msg) = node.op().validate(&node)?
        && !msg.is_empty()
    {
        return model_err(format!("{}: {}", node.kind(), msg));
    }
    Ok(node)
}

macro_rules! op_info {
    ($static:ident, $kind:expr, $doc:expr, $fcdoc:expr, $leaf:expr, [$($d:expr),*], [$($p:expr),* $(,)?]) => {{
        static $static: StaticInfo = StaticInfo::new();
        $static.get(|| kind_info($kind, MODULE, $doc, $fcdoc, $leaf, true, &[$($d),*], vec![$($p),*]))
    }};
}

#[allow(clippy::unnecessary_wraps)]
fn ok_msg() -> GResult<Option<String>> {
    Ok(Some(String::new()))
}

pub struct WithFields;

impl WithFields {
    fn info() -> &'static KindInfo {
        op_info!(
            I,
            "with_fields",
            "Attach named auxiliary scalar fields without changing the geometry.",
            "The geometry child's class, unchanged: auxiliary fields do not touch",
            None,
            [],
            []
        )
    }
    fn construct(args: ConstructArgs) -> GResult<Node> {
        build_op(Arc::new(Self), args, None)
    }
    fn entry() -> KindEntry {
        entry(Self::info(), Self::construct)
    }
}

struct FirstK<S> {
    a: KernelBox<S>,
}
impl<S: Scalar> Kernel<S> for FirstK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        self.a.eval(x)
    }
}

impl NodeOp for WithFields {
    fn info(&self) -> &KindInfo {
        Self::info()
    }
    fn validate(&self, n: &Node) -> GResult<Option<String>> {
        Ok(Some(if n.children().len() < 2 {
            "requires geometry and at least one auxiliary field".into()
        } else {
            String::new()
        }))
    }
    fn field_class(&self, _n: &Node, kids: &[FieldClass]) -> GResult<FieldClass> {
        first(kids)
    }
    fn aabb(&self, n: &Node) -> GResult<Bounds> {
        Ok(n.children().first().and_then(|c| aabb_of(c)))
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        _i: &KernelInputs<S>,
        kids: Vec<KernelBox<S>>,
        _c: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        let a = kids
            .into_iter()
            .next()
            .ok_or_else(|| GeometryError::Model("with_fields has no geometry child".into()))?;
        Ok(Box::new(FirstK { a }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoolOp {
    Union,
    Intersect,
    Difference,
}

impl BoolOp {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Union => "union",
            Self::Intersect => "intersect",
            Self::Difference => "difference",
        }
    }
}

pub struct Boolean(pub BoolOp);

const K_SCALE_DOC: &str =
    "this node's smoothing radius as a multiple of the context's; 1 anneals with the model";

impl Boolean {
    fn info_of(op: BoolOp) -> &'static KindInfo {
        match op {
            BoolOp::Union => op_info!(
                IU,
                "union",
                "``min(a, b)``, or ``smooth_min`` in smooth mode.",
                "The weakest child's own class when the two are PROVEN disjoint, else",
                None,
                [],
                [ParamSpec::float("k_scale", 1.0, "-", K_SCALE_DOC)]
            ),
            BoolOp::Intersect => op_info!(
                II,
                "intersect",
                "``max(a, b)``, or ``smooth_max`` in smooth mode.",
                "At most ``BOUND``; no disjointness upgrade (see the class",
                None,
                [],
                [ParamSpec::float("k_scale", 1.0, "-", K_SCALE_DOC)]
            ),
            BoolOp::Difference => op_info!(
                ID,
                "difference",
                "``max(a, -b)``: ``a`` with ``b`` removed.",
                "``a``'s own class when ``b`` is PROVEN disjoint from it, else at",
                None,
                [],
                [ParamSpec::float("k_scale", 1.0, "-", K_SCALE_DOC)]
            ),
        }
    }
    fn construct_u(args: ConstructArgs) -> GResult<Node> {
        build_op(Arc::new(Self(BoolOp::Union)), args, Some(2))
    }
    fn construct_i(args: ConstructArgs) -> GResult<Node> {
        build_op(Arc::new(Self(BoolOp::Intersect)), args, Some(2))
    }
    fn construct_d(args: ConstructArgs) -> GResult<Node> {
        build_op(Arc::new(Self(BoolOp::Difference)), args, Some(2))
    }
}

struct BoolK<S> {
    op: BoolOp,
    a: KernelBox<S>,
    b: KernelBox<S>,
    smooth: Option<(S, bool)>,
}
impl<S: Scalar> Kernel<S> for BoolK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        let a = self.a.eval(x);
        let b = self.b.eval(x);
        match (self.op, self.smooth) {
            (BoolOp::Union, None) => a.min(b),
            (BoolOp::Intersect, None) => a.max(b),
            (BoolOp::Difference, None) => a.max(-b),
            (BoolOp::Union, Some((k, e))) => smooth_min(a, b, k, e),
            (BoolOp::Intersect, Some((k, e))) => smooth_max(a, b, k, e),
            (BoolOp::Difference, Some((k, e))) => smooth_max(a, -b, k, e),
        }
    }
}

fn two<S>(kids: Vec<KernelBox<S>>, kind: &str) -> GResult<(KernelBox<S>, KernelBox<S>)> {
    let mut it = kids.into_iter();
    match (it.next(), it.next()) {
        (Some(a), Some(b)) => Ok((a, b)),
        _ => model_err(format!("{kind} needs two children")),
    }
}

impl NodeOp for Boolean {
    fn info(&self) -> &KindInfo {
        Self::info_of(self.0)
    }
    fn validate(&self, _n: &Node) -> GResult<Option<String>> {
        ok_msg()
    }
    fn field_class(&self, n: &Node, kids: &[FieldClass]) -> GResult<FieldClass> {
        let c = n.children();
        match self.0 {
            BoolOp::Union => {
                if c.len() >= 2 && disjoint(&c[0], &c[1], 0.0) {
                    weakest(kids, None)
                } else {
                    weakest(kids, Some(ClassKind::Bound))
                }
            }
            BoolOp::Intersect => weakest(kids, Some(ClassKind::Bound)),
            BoolOp::Difference => {
                if c.len() >= 2 && disjoint(&c[0], &c[1], 0.0) {
                    first(kids)
                } else {
                    weakest(kids, Some(ClassKind::Bound))
                }
            }
        }
    }
    fn field_class_smooth(&self, n: &Node, kids: &[FieldClass]) -> GResult<FieldClass> {
        match self.0 {
            BoolOp::Intersect => NodeOp::field_class(self, n, kids),
            _ => weakest(kids, Some(ClassKind::Bound)),
        }
    }
    fn aabb(&self, n: &Node) -> GResult<Bounds> {
        let c = n.children();
        let ba = c.first().and_then(|x| aabb_of(x));
        let bb = c.get(1).and_then(|x| aabb_of(x));
        Ok(match self.0 {
            BoolOp::Union => hull(ba, bb),
            BoolOp::Intersect => match (ba, bb) {
                (None, None) => None,
                (None, Some(b)) => Some(b),
                (Some(a), None) => Some(a),
                (Some(a), Some(b)) => {
                    let lo: [f64; 3] = std::array::from_fn(|i| a.0[i].max(b.0[i]));
                    let hi: [f64; 3] = std::array::from_fn(|i| a.1[i].min(b.1[i]));
                    if (0..3).any(|i| hi[i] < lo[i]) { Some((lo, lo)) } else { Some((lo, hi)) }
                }
            },
            BoolOp::Difference => ba,
        })
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        kids: Vec<KernelBox<S>>,
        ctx: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        let (a, b) = two(kids, self.0.name())?;
        let smooth = if ctx.smooth() {
            Some((inp.scalar("k_scale")? * ctx.smooth_r, ctx.smooth_kind == crate::node::SmoothKind::Exp))
        } else {
            None
        };
        Ok(Box::new(BoolK { op: self.0, a, b, smooth }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendKind {
    Fillet,
    Chamfer,
}

pub struct Blend {
    pub kind: BlendKind,
    pub op: BoolOp,
}

impl Blend {
    fn info_of(k: BlendKind) -> &'static KindInfo {
        match k {
            BlendKind::Fillet => op_info!(
                IF,
                "fillet",
                "A rounded blend of radius ``r``: the polynomial smooth min/max.",
                "At most ``BOUND``: the blend's gradient is a convex combination of",
                None,
                [],
                [ParamSpec::float("radius_mm", 0.5, "mm", "blend radius")]
            ),
            BlendKind::Chamfer => op_info!(
                IC,
                "chamfer",
                "A flat blend: ``min(a, b, (a + b - r)/sqrt2)`` and its duals.",
                "The weakest child at floor ``BOUND``, scaled by ``sqrt2`` for the",
                None,
                [],
                [ParamSpec::float("radius_mm", 0.5, "mm", "blend radius")]
            ),
        }
    }
    fn construct_with(kind: BlendKind, mut args: ConstructArgs) -> GResult<Node> {
        let op = match args.take_attr("op") {
            None => BoolOp::Union,
            Some(Attr::Str(s)) if s == "union" => BoolOp::Union,
            Some(Attr::Str(s)) if s == "intersect" => BoolOp::Intersect,
            Some(Attr::Str(s)) if s == "difference" => BoolOp::Difference,
            Some(other) => {
                return model_err(format!(
                    "{} op {}; expected union, intersect or difference",
                    Self::info_of(kind).kind,
                    other.py_obj().repr()
                ));
            }
        };
        build_op(Arc::new(Self { kind, op }), args, Some(2))
    }
    fn construct_f(args: ConstructArgs) -> GResult<Node> {
        Self::construct_with(BlendKind::Fillet, args)
    }
    fn construct_c(args: ConstructArgs) -> GResult<Node> {
        Self::construct_with(BlendKind::Chamfer, args)
    }
}

struct BlendK<S> {
    kind: BlendKind,
    op: BoolOp,
    r: S,
    exp: bool,
    a: KernelBox<S>,
    b: KernelBox<S>,
}
impl<S: Scalar> Kernel<S> for BlendK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        let a = self.a.eval(x);
        let b = self.b.eval(x);
        let r = self.r;
        match (self.kind, self.op) {
            (BlendKind::Fillet, BoolOp::Union) => smooth_min(a, b, r, self.exp),
            (BlendKind::Fillet, BoolOp::Intersect) => smooth_max(a, b, r, self.exp),
            (BlendKind::Fillet, BoolOp::Difference) => smooth_max(a, -b, r, self.exp),
            (BlendKind::Chamfer, BoolOp::Union) => a.min(b).min((a + b - r) * ROOT_HALF),
            (BlendKind::Chamfer, BoolOp::Intersect) => a.max(b).max((a + b + r) * ROOT_HALF),
            (BlendKind::Chamfer, BoolOp::Difference) => a.max(-b).max((a - b + r) * ROOT_HALF),
        }
    }
}

impl NodeOp for Blend {
    fn info(&self) -> &KindInfo {
        Self::info_of(self.kind)
    }
    fn struct_tokens(&self) -> Vec<String> {
        vec![format!("op={}", pyfmt::str_repr(self.op.name()))]
    }
    fn struct_json(&self) -> Vec<(String, serde_json::Value)> {
        vec![("op".into(), self.op.name().into())]
    }
    fn doc_attrs(&self) -> Vec<(String, Attr)> {
        if self.op == BoolOp::Union {
            Vec::new()
        } else {
            vec![("op".into(), Attr::Str(self.op.name().into()))]
        }
    }
    fn validate(&self, n: &Node) -> GResult<Option<String>> {
        let r = n.pf("radius_mm")?;
        if !(r > 0.0) || !r.is_finite() {
            return Ok(Some(format!("radius_mm must be finite and positive, got {}", pyfmt::g(r))));
        }
        ok_msg()
    }
    fn field_class(&self, _n: &Node, kids: &[FieldClass]) -> GResult<FieldClass> {
        let w = weakest(kids, Some(ClassKind::Bound))?;
        match self.kind {
            BlendKind::Fillet => Ok(w),
            BlendKind::Chamfer => times(&w, 2f64.sqrt(), "chamfer: the (a+b)/sqrt2 branch"),
        }
    }
    fn aabb(&self, n: &Node) -> GResult<Bounds> {
        let c = n.children();
        let ba = c.first().and_then(|x| aabb_of(x));
        let bb = c.get(1).and_then(|x| aabb_of(x));
        let r = n.pf("radius_mm")?;
        if self.op == BoolOp::Difference {
            return Ok(ba);
        }
        let (Some(a), Some(b)) = (ba, bb) else { return Ok(None) };
        if self.op == BoolOp::Union {
            return Ok(Some((
                std::array::from_fn(|i| a.0[i].min(b.0[i]) - r),
                std::array::from_fn(|i| a.1[i].max(b.1[i]) + r),
            )));
        }
        let lo: [f64; 3] = std::array::from_fn(|i| a.0[i].max(b.0[i]) - r);
        let hi: [f64; 3] = std::array::from_fn(|i| a.1[i].min(b.1[i]) + r);
        Ok(Some((lo, std::array::from_fn(|i| hi[i].max(lo[i])))))
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        kids: Vec<KernelBox<S>>,
        ctx: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        let (a, b) = two(kids, &Self::info_of(self.kind).kind)?;
        Ok(Box::new(BlendK {
            kind: self.kind,
            op: self.op,
            r: inp.scalar("radius_mm")?,
            exp: ctx.smooth_kind == crate::node::SmoothKind::Exp,
            a,
            b,
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryKind {
    Offset,
    Shell,
    Negate,
    RemapAffine,
    RemapClamp,
    RemapSoftClamp,
}

pub struct Unary(pub UnaryKind);

impl Unary {
    fn info_of(k: UnaryKind) -> &'static KindInfo {
        match k {
            UnaryKind::Offset => op_info!(
                IO,
                "offset",
                "``f = child - r``: grow by ``r``, or shrink by ``-r``.",
                "The child's class, unchanged.",
                None,
                [],
                [ParamSpec::float("distance_mm", 0.5, "mm", "outward offset; negative shrinks")]
            ),
            UnaryKind::Shell => op_info!(
                IS,
                "shell",
                "``f = |child| - t``: the wall of thickness ``2t`` about the child's surface.",
                "``EXACT`` becomes ``BOUND`` (the medial-axis crease); every other",
                None,
                [],
                [ParamSpec::float("thickness_mm", 0.4, "mm", "half wall thickness")]
            ),
            UnaryKind::Negate => op_info!(
                IN,
                "negate",
                "``f = -child``: the complement.",
                "The child's class, unchanged.  No ``aabb`` override: the complement",
                None,
                [],
                []
            ),
            UnaryKind::RemapAffine => op_info!(
                IA,
                "remap.affine",
                "``f = a * child + b``.",
                "The child's class with its Lipschitz constant multiplied by",
                None,
                [],
                [
                    ParamSpec::float("scale", 1.0, "-", "multiplier on the field value"),
                    ParamSpec::float("shift_mm", 0.0, "mm", "added after scaling")
                ]
            ),
            UnaryKind::RemapClamp => op_info!(
                ICL,
                "remap.clamp",
                "``f = clip(child, lo, hi)``.",
                "``EXACT`` becomes ``BOUND``; every other class passes through.",
                None,
                [],
                [
                    ParamSpec::float("hi_mm", 5.0, "mm", "upper clip"),
                    ParamSpec::float("lo_mm", -5.0, "mm", "lower clip")
                ]
            ),
            UnaryKind::RemapSoftClamp => op_info!(
                ISC,
                "remap.soft_clamp",
                "``f = L tanh(child / L)``: a clip with a continuous derivative.",
                "``EXACT`` becomes ``BOUND``; every other class passes through.",
                None,
                [],
                [ParamSpec::float("limit_mm", 5.0, "mm", "the value the field saturates at")]
            ),
        }
    }
}

struct UnaryK<S> {
    kind: UnaryKind,
    p: [S; 2],
    a: KernelBox<S>,
}
impl<S: Scalar> Kernel<S> for UnaryK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        let c = self.a.eval(x);
        match self.kind {
            UnaryKind::Offset => c - self.p[0],
            UnaryKind::Shell => c.abs() - self.p[0],
            UnaryKind::Negate => -c,
            UnaryKind::RemapAffine => self.p[0] * c + self.p[1],
            UnaryKind::RemapClamp => c.clip(self.p[0], self.p[1]),
            UnaryKind::RemapSoftClamp => self.p[0] * (c / self.p[0]).tanh(),
        }
    }
}

fn demote_exact(kids: &[FieldClass], note: &str) -> GResult<FieldClass> {
    let fc = first(kids)?;
    if fc.kind() == ClassKind::Exact { fc.demoted(ClassKind::Bound, note) } else { Ok(fc) }
}

impl NodeOp for Unary {
    fn info(&self) -> &KindInfo {
        Self::info_of(self.0)
    }
    fn validate(&self, n: &Node) -> GResult<Option<String>> {
        match self.0 {
            UnaryKind::Shell => {
                let t = n.pf("thickness_mm")?;
                if !(t > 0.0) {
                    return Ok(Some(format!("thickness_mm must be positive, got {}", pyfmt::g(t))));
                }
            }
            UnaryKind::RemapAffine => {
                if n.pf("scale")? == 0.0 {
                    return Ok(Some("scale 0 collapses the field to a constant; use `constant`".into()));
                }
            }
            UnaryKind::RemapClamp => {
                let (lo, hi) = (n.pf("lo_mm")?, n.pf("hi_mm")?);
                if !(lo < 0.0 && 0.0 < hi) {
                    return Ok(Some(format!(
                        "lo_mm {} and hi_mm {} must straddle zero, or the clip destroys the zero set and the node has no surface",
                        pyfmt::g(lo),
                        pyfmt::g(hi)
                    )));
                }
            }
            UnaryKind::RemapSoftClamp => {
                let l = n.pf("limit_mm")?;
                if !(l > 0.0) {
                    return Ok(Some(format!("limit_mm must be positive, got {}", pyfmt::g(l))));
                }
            }
            UnaryKind::Offset | UnaryKind::Negate => {}
        }
        ok_msg()
    }
    fn field_class(&self, n: &Node, kids: &[FieldClass]) -> GResult<FieldClass> {
        match self.0 {
            UnaryKind::Offset | UnaryKind::Negate => first(kids),
            UnaryKind::Shell => demote_exact(kids, "shell creases at the medial axis"),
            UnaryKind::RemapAffine => times(&first(kids)?, n.pf("scale")?.abs(), ""),
            UnaryKind::RemapClamp => demote_exact(kids, "clipped beyond +/- the clip"),
            UnaryKind::RemapSoftClamp => demote_exact(kids, "saturates at +/- limit_mm"),
        }
    }
    fn aabb(&self, n: &Node) -> GResult<Bounds> {
        let b = n.children().first().and_then(|c| aabb_of(c));
        Ok(match self.0 {
            UnaryKind::Offset => {
                let Some(b) = b else { return Ok(None) };
                let r = n.pf("distance_mm")?.max(0.0);
                let lo: [f64; 3] = std::array::from_fn(|i| b.0[i] - r);
                Some((lo, std::array::from_fn(|i| (b.1[i] + r).max(lo[i]))))
            }
            UnaryKind::Shell => {
                let Some(b) = b else { return Ok(None) };
                let t = n.pf("thickness_mm")?;
                Some((std::array::from_fn(|i| b.0[i] - t), std::array::from_fn(|i| b.1[i] + t)))
            }
            UnaryKind::Negate | UnaryKind::RemapAffine => None,
            UnaryKind::RemapClamp | UnaryKind::RemapSoftClamp => b,
        })
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        kids: Vec<KernelBox<S>>,
        _c: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        let a = kids.into_iter().next().ok_or_else(|| GeometryError::Model("missing child".into()))?;
        let z = S::cst(0.0);
        let p = match self.0 {
            UnaryKind::Offset => [inp.scalar("distance_mm")?, z],
            UnaryKind::Shell => [inp.scalar("thickness_mm")?, z],
            UnaryKind::Negate => [z, z],
            UnaryKind::RemapAffine => [inp.scalar("scale")?, inp.scalar("shift_mm")?],
            UnaryKind::RemapClamp => [inp.scalar("lo_mm")?, inp.scalar("hi_mm")?],
            UnaryKind::RemapSoftClamp => [inp.scalar("limit_mm")?, z],
        };
        Ok(Box::new(UnaryK { kind: self.0, p, a }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct Interpolate;

struct InterpK<S> {
    t: S,
    a: KernelBox<S>,
    b: KernelBox<S>,
}
impl<S: Scalar> Kernel<S> for InterpK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        (-self.t + 1.0) * self.a.eval(x) + self.t * self.b.eval(x)
    }
}

impl Interpolate {
    fn info() -> &'static KindInfo {
        op_info!(
            I,
            "interpolate",
            "``f = (1-t) a + t b``: morph between two fields.",
            "The first child's class at ``t <= 0``, the second's at ``t >= 1``,",
            None,
            [],
            [ParamSpec::float("t", 0.5, "-", "0 gives the first child, 1 the second")]
        )
    }
    fn construct(args: ConstructArgs) -> GResult<Node> {
        build_op(Arc::new(Self), args, Some(2))
    }
}

impl NodeOp for Interpolate {
    fn info(&self) -> &KindInfo {
        Self::info()
    }
    fn validate(&self, n: &Node) -> GResult<Option<String>> {
        let t = n.pf("t")?;
        if !((-1e-12..=1.0 + 1e-12).contains(&t)) {
            return Ok(Some(format!(
                "t {} is outside [0, 1]; extrapolating is not a convex combination and the 1-Lipschitz bound would not hold",
                pyfmt::g(t)
            )));
        }
        ok_msg()
    }
    fn field_class(&self, n: &Node, kids: &[FieldClass]) -> GResult<FieldClass> {
        let t = n.pf("t")?;
        if t <= 0.0 {
            return first(kids);
        }
        if t >= 1.0 {
            return kids
                .get(1)
                .cloned()
                .ok_or_else(|| GeometryError::Value("tuple index out of range".into()));
        }
        weakest(kids, Some(ClassKind::Bound))
    }
    fn aabb(&self, n: &Node) -> GResult<Bounds> {
        let c = n.children();
        Ok(hull(c.first().and_then(|x| aabb_of(x)), c.get(1).and_then(|x| aabb_of(x))))
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        kids: Vec<KernelBox<S>>,
        _c: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        let (a, b) = two(kids, "interpolate")?;
        Ok(Box::new(InterpK { t: inp.scalar("t")?, a, b }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct Mask;

struct MaskK<S> {
    inside: KernelBox<S>,
    outside: KernelBox<S>,
    sel: KernelBox<S>,
}
impl<S: Scalar> Kernel<S> for MaskK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        let s = self.sel.eval(x);
        let a = self.inside.eval(x);
        let b = self.outside.eval(x);
        if s.val() < 0.0 { a } else { b }
    }
}

impl Mask {
    fn info() -> &'static KindInfo {
        op_info!(
            I,
            "mask",
            "``f = a`` where the selector ``s < 0``, ``f = b`` elsewhere.",
            "``IMPLICIT``, unconditionally: the jump at the selector's zero set",
            Some(FieldClass::implicit()),
            [],
            []
        )
    }
    fn construct(mut args: ConstructArgs) -> GResult<Node> {
        if args.names.is_none() {
            args.names = Some(vec!["inside".into(), "outside".into(), "selector".into()]);
        }
        build_op(Arc::new(Self), args, Some(3))
    }
}

impl NodeOp for Mask {
    fn info(&self) -> &KindInfo {
        Self::info()
    }
    fn validate(&self, _n: &Node) -> GResult<Option<String>> {
        ok_msg()
    }
    fn field_class(&self, _n: &Node, _kids: &[FieldClass]) -> GResult<FieldClass> {
        Ok(FieldClass::implicit())
    }
    fn aabb(&self, n: &Node) -> GResult<Bounds> {
        let c = n.children();
        Ok(hull(c.first().and_then(|x| aabb_of(x)), c.get(1).and_then(|x| aabb_of(x))))
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        _i: &KernelInputs<S>,
        kids: Vec<KernelBox<S>>,
        _c: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        let mut it = kids.into_iter();
        match (it.next(), it.next(), it.next()) {
            (Some(inside), Some(outside), Some(sel)) => Ok(Box::new(MaskK { inside, outside, sel })),
            _ => model_err("mask needs three children"),
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn rot_matrix<S: Scalar>(rx: S, ry: S, rz: S) -> [[S; 3]; 3] {
    let d = std::f64::consts::PI / 180.0;
    let (cx, sx) = ((rx * d).cos(), (rx * d).sin());
    let (cy, sy) = ((ry * d).cos(), (ry * d).sin());
    let (cz, sz) = ((rz * d).cos(), (rz * d).sin());
    let (one, zero) = (S::cst(1.0), S::cst(0.0));
    let mx = [[one, zero, zero], [zero, cx, -sx], [zero, sx, cx]];
    let my = [[cy, zero, sy], [zero, one, zero], [-sy, zero, cy]];
    let mz = [[cz, -sz, zero], [sz, cz, zero], [zero, zero, one]];
    matmul(&matmul(&mz, &my), &mx)
}

fn rot_matrix_np(rx: f64, ry: f64, rz: f64) -> [[f64; 3]; 3] {
    let d = std::f64::consts::PI / 180.0;
    let (cx, sx) = ((rx * d).cos(), (rx * d).sin());
    let (cy, sy) = ((ry * d).cos(), (ry * d).sin());
    let (cz, sz) = ((rz * d).cos(), (rz * d).sin());
    let mx = [[1.0, 0.0, 0.0], [0.0, cx, -sx], [0.0, sx, cx]];
    let my = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
    let mz = [[cz, -sz, 0.0], [sz, cz, 0.0], [0.0, 0.0, 1.0]];
    let mm = |a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]| -> [[f64; 3]; 3] {
        std::array::from_fn(|i| {
            std::array::from_fn(|j| a[i][2].mul_add(b[2][j], a[i][1].mul_add(b[1][j], a[i][0] * b[0][j])))
        })
    };
    mm(&mm(&mz, &my), &mx)
}

fn matmul<S: Scalar>(a: &[[S; 3]; 3], b: &[[S; 3]; 3]) -> [[S; 3]; 3] {
    std::array::from_fn(|i| {
        std::array::from_fn(|j| a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j])
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XformKind {
    Translate,
    Rotate,
    ScaleUniform,
    ScaleNonUniform,
    ScaleNonUniformSafe,
}

pub struct Xform(pub XformKind);

const SXYZ: [&str; 3] = ["sx", "sy", "sz"];

impl Xform {
    fn info_of(k: XformKind) -> &'static KindInfo {
        match k {
            XformKind::Translate => op_info!(
                IT,
                "translate",
                "Rigid translation.  PRESERVES the class: ``sigma(J) = 1``.",
                "The child's class, unchanged.",
                None,
                [],
                [
                    ParamSpec::float("dx_mm", 0.0, "mm", "translation x"),
                    ParamSpec::float("dy_mm", 0.0, "mm", "translation y"),
                    ParamSpec::float("dz_mm", 0.0, "mm", "translation z"),
                ]
            ),
            XformKind::Rotate => op_info!(
                IR,
                "rotate",
                "Rigid rotation about the origin, intrinsic X-Y-Z in degrees.",
                "The child's class, unchanged.",
                None,
                [],
                [
                    ParamSpec::float("rx_deg", 0.0, "deg", "rotation about x, applied first"),
                    ParamSpec::float("ry_deg", 0.0, "deg", "rotation about y"),
                    ParamSpec::float("rz_deg", 0.0, "deg", "rotation about z, applied last"),
                ]
            ),
            XformKind::ScaleUniform => op_info!(
                ISU,
                "scale.uniform",
                "``f = s * child(x / s)``.",
                "The child's class, unchanged.",
                None,
                [],
                [ParamSpec::float("scale", 1.0, "-", "uniform scale factor")]
            ),
            XformKind::ScaleNonUniform => op_info!(
                ISN,
                "scale.nonuniform",
                "``f = child(x / s)`` with a per-axis ``s``.",
                "The child's class with its constant multiplied by ``1 / min(sx, sy,",
                None,
                [],
                [
                    ParamSpec::float("sx", 1.0, "-", "scale along x"),
                    ParamSpec::float("sy", 1.0, "-", "scale along y"),
                    ParamSpec::float("sz", 1.0, "-", "scale along z"),
                ]
            ),
            XformKind::ScaleNonUniformSafe => op_info!(
                ISS,
                "scale.nonuniform_safe",
                "``f = min(s) * child(x / s)``: the conservative non-uniform scale.",
                "``EXACT`` becomes ``BOUND``; every other class passes through",
                None,
                [],
                [
                    ParamSpec::float("sx", 1.0, "-", "scale along x"),
                    ParamSpec::float("sy", 1.0, "-", "scale along y"),
                    ParamSpec::float("sz", 1.0, "-", "scale along z"),
                ]
            ),
        }
    }
    fn s3(n: &Node) -> GResult<[f64; 3]> {
        Ok([n.pf("sx")?, n.pf("sy")?, n.pf("sz")?])
    }
}

enum XK<S> {
    Translate([S; 3]),
    Rotate([[S; 3]; 3]),
    Uniform(S),
    NonUniform([S; 3]),
    Safe([S; 3], S),
}

struct XformK<S> {
    k: XK<S>,
    a: KernelBox<S>,
}
impl<S: Scalar> Kernel<S> for XformK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        match &self.k {
            XK::Translate(d) => self.a.eval([x[0] - d[0], x[1] - d[1], x[2] - d[2]]),
            XK::Rotate(r) => {
                let y: [S; 3] = std::array::from_fn(|j| x[0] * r[0][j] + x[1] * r[1][j] + x[2] * r[2][j]);
                self.a.eval(y)
            }
            XK::Uniform(s) => *s * self.a.eval([x[0] / *s, x[1] / *s, x[2] / *s]),
            XK::NonUniform(s) => self.a.eval([x[0] / s[0], x[1] / s[1], x[2] / s[2]]),
            XK::Safe(s, m) => *m * self.a.eval([x[0] / s[0], x[1] / s[1], x[2] / s[2]]),
        }
    }
}

impl NodeOp for Xform {
    fn info(&self) -> &KindInfo {
        Self::info_of(self.0)
    }
    fn validate(&self, n: &Node) -> GResult<Option<String>> {
        match self.0 {
            XformKind::ScaleUniform => {
                let s = n.pf("scale")?;
                if !(s > 0.0) || !s.is_finite() {
                    return Ok(Some(format!("scale must be finite and positive, got {}", pyfmt::g(s))));
                }
            }
            XformKind::ScaleNonUniform | XformKind::ScaleNonUniformSafe => {
                let s = Self::s3(n)?;
                if s[0].min(s[1]).min(s[2]) <= 0.0
                    || !s.iter().all(|v| v.is_finite())
                    || s.iter().any(|v| v.is_nan())
                {
                    return Ok(Some(format!(
                        "every scale must be finite and positive, got {}",
                        PyObj::List(s.iter().map(|v| PyObj::Float(*v)).collect()).repr()
                    )));
                }
            }
            XformKind::Translate | XformKind::Rotate => {}
        }
        ok_msg()
    }
    fn field_class(&self, n: &Node, kids: &[FieldClass]) -> GResult<FieldClass> {
        match self.0 {
            XformKind::Translate | XformKind::Rotate | XformKind::ScaleUniform => first(kids),
            XformKind::ScaleNonUniform => {
                let s = Self::s3(n)?;
                times(&first(kids)?, 1.0 / s[0].min(s[1]).min(s[2]), "non-uniform scale: 1/sigma_min(J)")
            }
            XformKind::ScaleNonUniformSafe => demote_exact(kids, "conservative non-uniform scale"),
        }
    }
    fn aabb(&self, n: &Node) -> GResult<Bounds> {
        let Some(b) = n.children().first().and_then(|c| aabb_of(c)) else { return Ok(None) };
        Ok(match self.0 {
            XformKind::Translate => {
                let d = [n.pf("dx_mm")?, n.pf("dy_mm")?, n.pf("dz_mm")?];
                Some((std::array::from_fn(|i| b.0[i] + d[i]), std::array::from_fn(|i| b.1[i] + d[i])))
            }
            XformKind::Rotate => {
                let r = rot_matrix_np(n.pf("rx_deg")?, n.pf("ry_deg")?, n.pf("rz_deg")?);
                let mut lo = [f64::INFINITY; 3];
                let mut hi = [f64::NEG_INFINITY; 3];
                for a in [b.0[0], b.1[0]] {
                    for bb in [b.0[1], b.1[1]] {
                        for d in [b.0[2], b.1[2]] {
                            let c = [a, bb, d];
                            for j in 0..3 {

                                let w = c[2].mul_add(r[j][2], c[1].mul_add(r[j][1], c[0] * r[j][0]));
                                lo[j] = lo[j].min(w);
                                hi[j] = hi[j].max(w);
                            }
                        }
                    }
                }
                Some((lo, hi))
            }
            XformKind::ScaleUniform => {
                let s = n.pf("scale")?;
                Some((std::array::from_fn(|i| b.0[i] * s), std::array::from_fn(|i| b.1[i] * s)))
            }
            XformKind::ScaleNonUniform | XformKind::ScaleNonUniformSafe => {
                let s = Self::s3(n)?;
                Some((std::array::from_fn(|i| b.0[i] * s[i]), std::array::from_fn(|i| b.1[i] * s[i])))
            }
        })
    }
    fn render_sampling_scale(&self, n: &Node) -> Option<f64> {
        match self.0 {
            XformKind::ScaleUniform => n.pf("scale").ok(),
            XformKind::ScaleNonUniform | XformKind::ScaleNonUniformSafe => {
                Self::s3(n).ok().map(|s| s[0].min(s[1]).min(s[2]))
            }
            _ => None,
        }
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        kids: Vec<KernelBox<S>>,
        _c: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        let a = kids.into_iter().next().ok_or_else(|| GeometryError::Model("missing child".into()))?;
        let k = match self.0 {
            XformKind::Translate => {
                XK::Translate([inp.scalar("dx_mm")?, inp.scalar("dy_mm")?, inp.scalar("dz_mm")?])
            }
            XformKind::Rotate => {
                XK::Rotate(rot_matrix(inp.scalar("rx_deg")?, inp.scalar("ry_deg")?, inp.scalar("rz_deg")?))
            }
            XformKind::ScaleUniform => XK::Uniform(inp.scalar("scale")?),
            XformKind::ScaleNonUniform => {
                XK::NonUniform([inp.scalar(SXYZ[0])?, inp.scalar(SXYZ[1])?, inp.scalar(SXYZ[2])?])
            }
            XformKind::ScaleNonUniformSafe => {
                let s = [inp.scalar(SXYZ[0])?, inp.scalar(SXYZ[1])?, inp.scalar(SXYZ[2])?];
                XK::Safe(s, reduce_min(&s))
            }
        };
        Ok(Box::new(XformK { k, a }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarpKind {
    Twist,
    Bend,
    Taper,
}

pub struct Warp(pub WarpKind);

impl Warp {
    fn info_of(k: WarpKind) -> &'static KindInfo {
        let ext = || ParamSpec::float("extent_mm", 5.0, "mm", "half-size of the cube the class covers");
        match k {
            WarpKind::Twist => op_info!(
                IW,
                "warp.twist",
                "Twist about local z at ``rate_deg_per_mm``.",
                "The child's class with its constant multiplied by",
                None,
                [],
                [ext(), ParamSpec::float("rate_deg_per_mm", 10.0, "deg/mm", "twist per mm along z")]
            ),
            WarpKind::Bend => op_info!(
                IB,
                "warp.bend",
                "Bend the local x axis onto a circular arc of radius ``R``, curving toward +z.",
                "The child's class with its constant multiplied by",
                None,
                [],
                [ext(), ParamSpec::float("radius_mm", 20.0, "mm", "arc radius; large is gentle")]
            ),
            WarpKind::Taper => op_info!(
                ITP,
                "warp.taper",
                "Taper about local z: the cross-section shrinks by ``1 + rate * z``.",
                "The child's class with its constant multiplied by",
                None,
                [],
                [ext(), ParamSpec::float("rate_per_mm", 0.05, "1/mm", "cross-section change per mm of z")]
            ),
        }
    }

    fn lipschitz_factor(&self, n: &Node) -> GResult<f64> {
        let e = n.pf("extent_mm")?;
        match self.0 {
            WarpKind::Twist => {
                let k = n.pf("rate_deg_per_mm")?.to_radians();
                let r = e * 2f64.sqrt();
                let a = (k * r).powi(2);
                Ok((0.5 * (2.0 + a + (a * a + 4.0 * a).sqrt())).sqrt())
            }
            WarpKind::Bend => {
                let big = n.pf("radius_mm")?;
                if big <= e {
                    return Err(GeometryError::Value(format!(
                        "radius_mm {} must exceed extent_mm {}, or the bend is singular inside the part",
                        pyfmt::g(big),
                        pyfmt::g(e)
                    )));
                }
                Ok((big / (big - e)).max(1.0))
            }
            WarpKind::Taper => {
                let a = n.pf("rate_per_mm")?.abs();
                if 1.0 - a * e <= 0.0 {
                    return Err(GeometryError::Value(format!(
                        "rate_per_mm {} inverts the section within extent_mm {}",
                        pyfmt::g(a),
                        pyfmt::g(e)
                    )));
                }
                Ok((2.0 * (1.0 + a * e).powi(2) + 2.0 * (a * e).powi(2) + 1.0).sqrt())
            }
        }
    }

    fn warp_validate(&self, n: &Node) -> GResult<String> {
        let e = n.pf("extent_mm")?;
        if !(e > 0.0) || !e.is_finite() {
            return Ok(format!("extent_mm must be finite and positive, got {}", pyfmt::g(e)));
        }
        if let Some(b) = n.children().first().and_then(|c| aabb_of(c)) {
            let r = b.0.iter().chain(b.1.iter()).map(|v| v.abs()).fold(f64::NEG_INFINITY, f64::max);
            if r > e * (1.0 + 1e-9) {
                return Ok(format!(
                    "extent_mm {} does not contain the child's bounding box (reaches {}); the declared Lipschitz class would not cover the shape itself",
                    pyfmt::g(e),
                    pyfmt::g(r)
                ));
            }
        }
        let k = match self.lipschitz_factor(n) {
            Ok(k) => k,
            Err(err) => return Ok(format!("ValueError({})", pyfmt::str_repr(&err.to_string()))),
        };
        if !k.is_finite() || k <= 0.0 {
            return Ok(format!("the Jacobian bound over extent_mm is {}", pyfmt::float_repr(k)));
        }
        Ok(String::new())
    }
}

enum WK<S> {
    Twist(S),
    Bend(S),
    Taper(S),
}

struct WarpK<S> {
    w: WK<S>,
    a: KernelBox<S>,
}
impl<S: Scalar> Kernel<S> for WarpK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        match &self.w {
            WK::Twist(k) => {
                let th = -*k * x[2];
                let (c, s) = (th.cos(), th.sin());
                self.a.eval([x[0] * c - x[1] * s, x[0] * s + x[1] * c, x[2]])
            }
            WK::Bend(big) => {
                let u = *big - x[2];
                let v = x[0];
                let rho = (u * u + v * v + SAFE_EPS).sqrt();
                self.a.eval([*big * v.atan2(u), x[1], *big - rho])
            }
            WK::Taper(a) => {
                let s = *a * x[2] + 1.0;
                self.a.eval([x[0] * s, x[1] * s, x[2]])
            }
        }
    }
}

impl NodeOp for Warp {
    fn info(&self) -> &KindInfo {
        Self::info_of(self.0)
    }
    fn validate(&self, n: &Node) -> GResult<Option<String>> {
        if self.0 == WarpKind::Bend {
            let big = n.pf("radius_mm")?;
            if !(big > 0.0) || !big.is_finite() {
                return Ok(Some(format!("radius_mm must be finite and positive, got {}", pyfmt::g(big))));
            }
        }
        Ok(Some(self.warp_validate(n)?))
    }
    fn field_class(&self, n: &Node, kids: &[FieldClass]) -> GResult<FieldClass> {
        let k = self.lipschitz_factor(n)?;
        times(&first(kids)?, k, &format!("{} over extent_mm", Self::info_of(self.0).kind))
    }
    fn aabb(&self, n: &Node) -> GResult<Bounds> {
        if self.0 != WarpKind::Twist {
            return Ok(None);
        }
        let Some(b) = n.children().first().and_then(|c| aabb_of(c)) else { return Ok(None) };
        let mut r = f64::NEG_INFINITY;
        for xx in [b.0[0], b.1[0]] {
            for yy in [b.0[1], b.1[1]] {
                r = r.max(xx.hypot(yy));
            }
        }
        Ok(Some(([-r, -r, b.0[2]], [r, r, b.1[2]])))
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        kids: Vec<KernelBox<S>>,
        _c: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        let a = kids.into_iter().next().ok_or_else(|| GeometryError::Model("missing child".into()))?;
        let w = match self.0 {
            WarpKind::Twist => WK::Twist(inp.scalar("rate_deg_per_mm")? * (std::f64::consts::PI / 180.0)),
            WarpKind::Bend => WK::Bend(inp.scalar("radius_mm")?),
            WarpKind::Taper => WK::Taper(inp.scalar("rate_per_mm")?),
        };
        Ok(Box::new(WarpK { w, a }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct ArrayLinear {
    pub axes: Vec<usize>,
    pub neighbours: i64,
}

pub struct ArrayPolar {
    pub neighbours: i64,
}

const PITCH: [&str; 3] = ["pitch_x_mm", "pitch_y_mm", "pitch_z_mm"];
const COUNT: [&str; 3] = ["count_x", "count_y", "count_z"];

fn neighbours_attr(args: &mut ConstructArgs, kind: &str) -> GResult<i64> {
    let n = match args.take_attr("neighbours") {
        None => 1,
        Some(a) => {
            let v = a.as_f64().ok_or_else(|| {
                GeometryError::Value(format!("invalid literal for int() with base 10: {}", a.py_obj().repr()))
            })?;
            #[allow(clippy::cast_possible_truncation)]
            let i = v.trunc() as i64;
            i
        }
    };
    if n != 0 && n != 1 {
        return model_err(format!("{kind} neighbours must be 0 or 1"));
    }
    Ok(n)
}

fn array_class(neighbours: i64, kids: &[FieldClass]) -> GResult<FieldClass> {
    if neighbours == 0 {
        return Ok(FieldClass::implicit().weaker_of(&first(kids)?));
    }
    weakest(kids, Some(ClassKind::Bound))
}

fn offsets(ndim: usize, neighbours: i64) -> Vec<Vec<u8>> {
    if neighbours == 0 {
        return vec![vec![0; ndim]];
    }
    let mut out: Vec<Vec<u8>> = vec![Vec::new()];
    for _ in 0..ndim {
        out = out
            .into_iter()
            .flat_map(|o| [0u8, 1].into_iter().map(move |d| [o.clone(), vec![d]].concat()))
            .collect();
    }
    out
}

impl ArrayLinear {
    fn info() -> &'static KindInfo {
        op_info!(
            I,
            "array.linear",
            "A rectangular pattern along any subset of the local axes.",
            "``IMPLICIT`` with ``neighbours=0``; the weakest child at floor",
            None,
            ["count_x", "count_y", "count_z"],
            [
                ParamSpec::float("count_x", 3.0, "count", "copies along x (integer, DISCRETE)"),
                ParamSpec::float("count_y", 3.0, "count", "copies along y (integer, DISCRETE)"),
                ParamSpec::float("count_z", 1.0, "count", "copies along z (integer, DISCRETE)"),
                ParamSpec::float("pitch_x_mm", 5.0, "mm", "cell size along x"),
                ParamSpec::float("pitch_y_mm", 5.0, "mm", "cell size along y"),
                ParamSpec::float("pitch_z_mm", 5.0, "mm", "cell size along z"),
            ]
        )
    }
    fn construct(mut args: ConstructArgs) -> GResult<Node> {
        let raw = args.take_attr("axes");
        let axes_repr = raw.as_ref().map_or_else(|| "(0, 1)".to_string(), |a| a.py_obj().repr());
        let mut set = std::collections::BTreeSet::new();
        match &raw {
            None => {
                set.insert(0i64);
                set.insert(1);
            }
            Some(Attr::List(items)) => {
                for it in items {
                    let v = it.as_f64().ok_or_else(|| {
                        GeometryError::Value(format!(
                            "invalid literal for int() with base 10: {}",
                            it.py_obj().repr()
                        ))
                    })?;
                    #[allow(clippy::cast_possible_truncation)]
                    set.insert(v.trunc() as i64);
                }
            }
            Some(other) => {
                return Err(GeometryError::Value(format!(
                    "{} object is not iterable",
                    pyfmt::str_repr(&other.py_obj().repr())
                )));
            }
        }
        if set.is_empty() || set.iter().any(|a| !(0..=2).contains(a)) {
            return model_err(format!(
                "array.linear axes must be a non-empty subset of (0, 1, 2), got {axes_repr}"
            ));
        }
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let axes: Vec<usize> = set.into_iter().map(|a| a as usize).collect();
        let neighbours = neighbours_attr(&mut args, "array.linear")?;
        build_op(Arc::new(Self { axes, neighbours }), args, Some(1))
    }
}

struct ArrayLinearK<S> {
    axes: Vec<usize>,
    p: [S; 3],
    n: [S; 3],
    neighbours: i64,
    a: KernelBox<S>,
}
impl<S: Scalar> Kernel<S> for ArrayLinearK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        let mut base = x;
        let mut cells: [Option<(S, S, S)>; 3] = [None, None, None];
        for &a in &self.axes {
            let (p, n) = (self.p[a], self.n[a]);
            let half = (n - 1.0) * 0.5;
            let t = x[a] / p + half;
            let j = t.round_even().clip(S::cst(0.0), n - 1.0);
            let lean = if (t - j).val() >= 0.0 { 1.0 } else { -1.0 };
            cells[a] = Some((half, j, S::cst(lean)));
            base[a] = x[a] - p * (j - half);
        }
        let mut best: Option<S> = None;
        for delta in offsets(self.axes.len(), self.neighbours) {
            let mut loc = base;
            for (&a, &d) in self.axes.iter().zip(&delta) {
                if d != 0
                    && let Some((half, j, lean)) = cells[a]
                {
                    let j2 = (j + lean).clip(S::cst(0.0), self.n[a] - 1.0);
                    loc[a] = x[a] - self.p[a] * (j2 - half);
                }
            }
            let v = self.a.eval(loc);
            best = Some(match best {
                None => v,
                Some(b) => b.min(v),
            });
        }
        best.unwrap_or_else(|| S::cst(f64::NAN))
    }
}

impl NodeOp for ArrayLinear {
    fn info(&self) -> &KindInfo {
        Self::info()
    }
    fn struct_tokens(&self) -> Vec<String> {
        #[allow(clippy::cast_possible_wrap)]
        let axes: Vec<i64> = self.axes.iter().map(|a| *a as i64).collect();
        vec![format!("axes={}", PyObj::int_tuple(&axes).repr()), format!("neighbours={}", self.neighbours)]
    }
    fn struct_json(&self) -> Vec<(String, serde_json::Value)> {
        vec![("axes".into(), serde_json::json!(self.axes)), ("neighbours".into(), self.neighbours.into())]
    }
    fn doc_attrs(&self) -> Vec<(String, Attr)> {
        let mut out = Vec::new();
        if self.axes != [0, 1] {
            #[allow(clippy::cast_possible_wrap)]
            out.push(("axes".into(), Attr::List(self.axes.iter().map(|a| Attr::Int(*a as i64)).collect())));
        }
        if self.neighbours != 1 {
            out.push(("neighbours".into(), Attr::Int(self.neighbours)));
        }
        out
    }
    fn validate(&self, n: &Node) -> GResult<Option<String>> {
        let b = n.children().first().and_then(|c| aabb_of(c));
        for &a in &self.axes {
            let p = n.pf(PITCH[a])?;
            let cnt = n.pf(COUNT[a])?;
            if !(p > 0.0) {
                return Ok(Some(format!("{} must be positive, got {}", PITCH[a], pyfmt::g(p))));
            }
            if cnt < 1.0 {
                return Ok(Some(format!("{} must be at least 1, got {}", COUNT[a], pyfmt::g(cnt))));
            }
            let Some(b) = b else {
                return Ok(Some(format!(
                    "the child has no conservative bounding box, so the cell-larger-than-the-shape condition on axis {a} \
                     cannot be CHECKED; wrap it in something that has one, or the pattern would be assumed correct"
                )));
            };
            let half = b.0[a].abs().max(b.1[a].abs());
            if half > 0.5 * p * (1.0 + 1e-9) {
                return Ok(Some(format!(
                    "axis {a}: the child reaches {} mm from the cell centre but the cell is only {} mm across, so copies would \
                     overlap and the fold would hide it",
                    pyfmt::fmt_g(half, 4),
                    pyfmt::fmt_g(p, 4)
                )));
            }
        }
        ok_msg()
    }
    fn field_class(&self, _n: &Node, kids: &[FieldClass]) -> GResult<FieldClass> {
        array_class(self.neighbours, kids)
    }
    fn aabb(&self, n: &Node) -> GResult<Bounds> {
        let Some(b) = n.children().first().and_then(|c| aabb_of(c)) else { return Ok(None) };
        let (mut lo, mut hi) = b;
        for &a in &self.axes {
            let span = 0.5 * (n.pf(COUNT[a])? - 1.0) * n.pf(PITCH[a])?;
            lo[a] -= span;
            hi[a] += span;
        }
        Ok(Some((lo, hi)))
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        kids: Vec<KernelBox<S>>,
        _c: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        let a = kids.into_iter().next().ok_or_else(|| GeometryError::Model("missing child".into()))?;
        let p = [inp.scalar(PITCH[0])?, inp.scalar(PITCH[1])?, inp.scalar(PITCH[2])?];
        let n = [inp.scalar(COUNT[0])?, inp.scalar(COUNT[1])?, inp.scalar(COUNT[2])?];
        Ok(Box::new(ArrayLinearK { axes: self.axes.clone(), p, n, neighbours: self.neighbours, a }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl ArrayPolar {
    fn info() -> &'static KindInfo {
        op_info!(
            I,
            "array.polar",
            "``count`` copies equally spaced about the local z axis.",
            "``IMPLICIT`` with ``neighbours=0``; the weakest child at floor",
            None,
            ["count"],
            [ParamSpec::float("count", 6.0, "count", "copies about local z (integer, DISCRETE)")]
        )
    }
    fn construct(mut args: ConstructArgs) -> GResult<Node> {
        let neighbours = neighbours_attr(&mut args, "array.polar")?;
        build_op(Arc::new(Self { neighbours }), args, Some(1))
    }
}

struct ArrayPolarK<S> {
    n: S,
    neighbours: i64,
    a: KernelBox<S>,
}
impl<S: Scalar> Kernel<S> for ArrayPolarK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        let sector = S::cst(2.0 * std::f64::consts::PI) / self.n;
        let th = x[1].atan2(x[0]);
        let r = vlen2(x[0], x[1]);
        let t = th / sector;
        let j = t.round_even();
        let lean = if (t - j).val() >= 0.0 { 1.0 } else { -1.0 };
        let ds: &[f64] = if self.neighbours == 0 { &[0.0] } else { &[0.0, 1.0] };
        let mut best: Option<S> = None;
        for d in ds {
            let t2 = th - (j + d * lean) * sector;
            let v = self.a.eval([r * t2.cos(), r * t2.sin(), x[2]]);
            best = Some(match best {
                None => v,
                Some(b) => b.min(v),
            });
        }
        best.unwrap_or_else(|| S::cst(f64::NAN))
    }
}

impl NodeOp for ArrayPolar {
    fn info(&self) -> &KindInfo {
        Self::info()
    }
    fn struct_tokens(&self) -> Vec<String> {
        vec![format!("neighbours={}", self.neighbours)]
    }
    fn struct_json(&self) -> Vec<(String, serde_json::Value)> {
        vec![("neighbours".into(), self.neighbours.into())]
    }
    fn doc_attrs(&self) -> Vec<(String, Attr)> {
        if self.neighbours == 1 {
            Vec::new()
        } else {
            vec![("neighbours".into(), Attr::Int(self.neighbours))]
        }
    }
    fn validate(&self, n: &Node) -> GResult<Option<String>> {
        let cnt = n.pf("count")?;
        if cnt < 1.0 {
            return Ok(Some(format!("count must be at least 1, got {}", pyfmt::g(cnt))));
        }
        if cnt < 2.0 {
            return ok_msg();
        }
        let Some((lo, hi)) = n.children().first().and_then(|c| aabb_of(c)) else {
            return Ok(Some("the child has no conservative bounding box, so the one-copy-per-wedge condition cannot be CHECKED".into()));
        };
        let half = std::f64::consts::PI / cnt;
        if lo[0] <= 0.0 {
            return Ok(Some(format!(
                "the child reaches x = {} <= 0, so it straddles the wedge apex and the angular fold would tear it",
                pyfmt::fmt_g(lo[0], 4)
            )));
        }
        let mut worst = 0.0f64;
        for xx in [lo[0], hi[0]] {
            for yy in [lo[1], hi[1]] {
                worst = worst.max(yy.atan2(xx).abs());
            }
        }
        if worst > half * (1.0 + 1e-9) {
            return Ok(Some(format!(
                "the child spans +/- {} rad about the x axis but a wedge is only +/- {} rad, so copies would overlap",
                pyfmt::fmt_f(worst, 3),
                pyfmt::fmt_f(half, 3)
            )));
        }
        ok_msg()
    }
    fn field_class(&self, _n: &Node, kids: &[FieldClass]) -> GResult<FieldClass> {
        array_class(self.neighbours, kids)
    }
    fn aabb(&self, n: &Node) -> GResult<Bounds> {
        let Some(b) = n.children().first().and_then(|c| aabb_of(c)) else { return Ok(None) };
        let mut rad = 0.0f64;
        for xx in [b.0[0], b.1[0]] {
            for yy in [b.0[1], b.1[1]] {
                rad = rad.max(xx.hypot(yy));
            }
        }
        Ok(Some(([-rad, -rad, b.0[2]], [rad, rad, b.1[2]])))
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        kids: Vec<KernelBox<S>>,
        _c: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        let a = kids.into_iter().next().ok_or_else(|| GeometryError::Model("missing child".into()))?;
        Ok(Box::new(ArrayPolarK { n: inp.scalar("count")?, neighbours: self.neighbours, a }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn c_offset(a: ConstructArgs) -> GResult<Node> {
    build_op(Arc::new(Unary(UnaryKind::Offset)), a, Some(1))
}
fn c_shell(a: ConstructArgs) -> GResult<Node> {
    build_op(Arc::new(Unary(UnaryKind::Shell)), a, Some(1))
}
fn c_negate(a: ConstructArgs) -> GResult<Node> {
    build_op(Arc::new(Unary(UnaryKind::Negate)), a, Some(1))
}
fn c_affine(a: ConstructArgs) -> GResult<Node> {
    build_op(Arc::new(Unary(UnaryKind::RemapAffine)), a, Some(1))
}
fn c_clamp(a: ConstructArgs) -> GResult<Node> {
    build_op(Arc::new(Unary(UnaryKind::RemapClamp)), a, Some(1))
}
fn c_soft(a: ConstructArgs) -> GResult<Node> {
    build_op(Arc::new(Unary(UnaryKind::RemapSoftClamp)), a, Some(1))
}
fn c_translate(a: ConstructArgs) -> GResult<Node> {
    build_op(Arc::new(Xform(XformKind::Translate)), a, Some(1))
}
fn c_rotate(a: ConstructArgs) -> GResult<Node> {
    build_op(Arc::new(Xform(XformKind::Rotate)), a, Some(1))
}
fn c_su(a: ConstructArgs) -> GResult<Node> {
    build_op(Arc::new(Xform(XformKind::ScaleUniform)), a, Some(1))
}
fn c_sn(a: ConstructArgs) -> GResult<Node> {
    build_op(Arc::new(Xform(XformKind::ScaleNonUniform)), a, Some(1))
}
fn c_ss(a: ConstructArgs) -> GResult<Node> {
    build_op(Arc::new(Xform(XformKind::ScaleNonUniformSafe)), a, Some(1))
}
fn c_twist(a: ConstructArgs) -> GResult<Node> {
    build_op(Arc::new(Warp(WarpKind::Twist)), a, Some(1))
}
fn c_bend(a: ConstructArgs) -> GResult<Node> {
    build_op(Arc::new(Warp(WarpKind::Bend)), a, Some(1))
}
fn c_taper(a: ConstructArgs) -> GResult<Node> {
    build_op(Arc::new(Warp(WarpKind::Taper)), a, Some(1))
}

#[must_use]
pub fn entries() -> KernelEntryList {
    vec![
        WithFields::entry(),
        entry(Boolean::info_of(BoolOp::Union), Boolean::construct_u),
        entry(Boolean::info_of(BoolOp::Intersect), Boolean::construct_i),
        entry(Boolean::info_of(BoolOp::Difference), Boolean::construct_d),
        entry(Blend::info_of(BlendKind::Fillet), Blend::construct_f),
        entry(Blend::info_of(BlendKind::Chamfer), Blend::construct_c),
        entry(Unary::info_of(UnaryKind::Offset), c_offset),
        entry(Unary::info_of(UnaryKind::Shell), c_shell),
        entry(Unary::info_of(UnaryKind::Negate), c_negate),
        entry(Unary::info_of(UnaryKind::RemapAffine), c_affine),
        entry(Unary::info_of(UnaryKind::RemapClamp), c_clamp),
        entry(Unary::info_of(UnaryKind::RemapSoftClamp), c_soft),
        entry(Interpolate::info(), Interpolate::construct),
        entry(Mask::info(), Mask::construct),
        entry(Xform::info_of(XformKind::Translate), c_translate),
        entry(Xform::info_of(XformKind::Rotate), c_rotate),
        entry(Xform::info_of(XformKind::ScaleUniform), c_su),
        entry(Xform::info_of(XformKind::ScaleNonUniform), c_sn),
        entry(Xform::info_of(XformKind::ScaleNonUniformSafe), c_ss),
        entry(Warp::info_of(WarpKind::Twist), c_twist),
        entry(Warp::info_of(WarpKind::Bend), c_bend),
        entry(Warp::info_of(WarpKind::Taper), c_taper),
        entry(ArrayLinear::info(), ArrayLinear::construct),
        entry(ArrayPolar::info(), ArrayPolar::construct),
    ]
}
