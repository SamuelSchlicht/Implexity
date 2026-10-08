// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::error::{GResult, GeometryError, value_err};
use crate::eval::SAFE_EPS;
use crate::fieldclass::FieldClass;
use crate::kinds::{StaticInfo, entry, kind_info, max3, triple, vlen2, vlen3};
use crate::node::{
    Attr, ConstructArgs, EvalCtx, Kernel, KernelBox, KernelEntryList, KernelInputs, KindInfo, Node, NodeOp,
    ParamSpec,
};
use crate::pyfmt::{self, PyObj};
use crate::scalar::{Scalar, reduce_min};
use crate::value::ParamValue;

const MODULE: &str = "implexity.implicit.primitives";

#[allow(clippy::unnecessary_wraps)]
fn leaf_exact() -> Option<FieldClass> {
    Some(FieldClass::exact())
}

fn simple(op: Arc<dyn crate::node::ErasedOp>, mut args: ConstructArgs) -> GResult<Node> {
    args.attrs_into_params();
    Node::new(op, args.children, args.names, args.params)
}

macro_rules! leaf_kind {
    ($name:ident, $static:ident, $kind:expr, $doc:expr, $leaf:expr, [$($p:expr),* $(,)?]) => {
        static $static: StaticInfo = StaticInfo::new();
        pub struct $name;
        impl $name {
            pub fn kind_info() -> &'static KindInfo {
                $static.get(|| kind_info($kind, MODULE, $doc, "", $leaf, true, &[], vec![$($p),*]))
            }
            fn construct(args: ConstructArgs) -> GResult<Node> {
                simple(Arc::new($name), args)
            }
            #[must_use]
            pub fn entry() -> crate::node::KindEntry {
                entry(Self::kind_info(), Self::construct)
            }
        }
    };
}

#[allow(clippy::unnecessary_wraps)]
fn aabb_sym(r: [f64; 3]) -> Option<([f64; 3], [f64; 3])> {
    Some(([-r[0], -r[1], -r[2]], r))
}

leaf_kind!(
    Sphere,
    SPHERE,
    "sphere",
    "``|p| - r``.  EXACT: the textbook case, and the reference the others are",
    leaf_exact(),
    [ParamSpec::float("radius_mm", 1.0, "mm", "radius")]
);

struct SphereK<S> {
    r: S,
}
impl<S: Scalar> Kernel<S> for SphereK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        vlen3(x) - self.r
    }
}

impl NodeOp for Sphere {
    fn info(&self) -> &KindInfo {
        Self::kind_info()
    }
    fn field_class(&self, _n: &Node, _k: &[FieldClass]) -> GResult<FieldClass> {
        Ok(FieldClass::exact())
    }
    fn aabb(&self, n: &Node) -> GResult<Option<([f64; 3], [f64; 3])>> {
        let r = n.pf("radius_mm")?;
        Ok(aabb_sym([r, r, r]))
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        _k: Vec<KernelBox<S>>,
        _c: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        Ok(Box::new(SphereK { r: inp.scalar("radius_mm")? }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

const BXYZ: [&str; 3] = ["bx_mm", "by_mm", "bz_mm"];

leaf_kind!(
    BoxKind,
    BOX,
    "box",
    "Axis-aligned box of half-extents ``(bx, by, bz)``, centred on the origin.",
    leaf_exact(),
    [
        ParamSpec::float("bx_mm", 1.0, "mm", "half-extent along local x"),
        ParamSpec::float("by_mm", 1.0, "mm", "half-extent along local y"),
        ParamSpec::float("bz_mm", 1.0, "mm", "half-extent along local z"),
    ]
);

struct BoxK<S> {
    b: [S; 3],
    r: Option<S>,
}
impl<S: Scalar> Kernel<S> for BoxK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        let (b, r) = match self.r {
            Some(r) => ([self.b[0] - r, self.b[1] - r, self.b[2] - r], Some(r)),
            None => (self.b, None),
        };
        let q = [x[0].abs() - b[0], x[1].abs() - b[1], x[2].abs() - b[2]];
        let outside = vlen3([q[0].max_c(0.0), q[1].max_c(0.0), q[2].max_c(0.0)]);
        let inside = max3(q).min_c(0.0);
        match r {
            Some(r) => outside + inside - r,
            None => outside + inside,
        }
    }
}

fn b3(n: &Node) -> GResult<[f64; 3]> {
    Ok([n.pf("bx_mm")?, n.pf("by_mm")?, n.pf("bz_mm")?])
}

impl NodeOp for BoxKind {
    fn info(&self) -> &KindInfo {
        Self::kind_info()
    }
    fn field_class(&self, _n: &Node, _k: &[FieldClass]) -> GResult<FieldClass> {
        Ok(FieldClass::exact())
    }
    fn aabb(&self, n: &Node) -> GResult<Option<([f64; 3], [f64; 3])>> {
        Ok(aabb_sym(b3(n)?))
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        _k: Vec<KernelBox<S>>,
        _c: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        Ok(Box::new(BoxK { b: triple(inp, BXYZ)?, r: None }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

leaf_kind!(
    RoundedBox,
    ROUNDED_BOX,
    "rounded_box",
    "A box of half-extents ``b`` with every edge rounded to radius ``r``.",
    leaf_exact(),
    [
        ParamSpec::float("bx_mm", 1.0, "mm", "half-extent along local x"),
        ParamSpec::float("by_mm", 1.0, "mm", "half-extent along local y"),
        ParamSpec::float("bz_mm", 1.0, "mm", "half-extent along local z"),
        ParamSpec::float("radius_mm", 0.2, "mm", "edge and corner radius"),
    ]
);

impl NodeOp for RoundedBox {
    fn info(&self) -> &KindInfo {
        Self::kind_info()
    }
    fn field_class(&self, _n: &Node, _k: &[FieldClass]) -> GResult<FieldClass> {
        Ok(FieldClass::exact())
    }
    fn validate(&self, n: &Node) -> GResult<Option<String>> {
        let b = b3(n)?;
        let r = n.pf("radius_mm")?;
        if r < 0.0 {
            return Ok(Some(format!("radius_mm {} is negative", pyfmt::g(r))));
        }
        let bmin = b[0].min(b[1]).min(b[2]);
        if r >= bmin {
            return Ok(Some(format!(
                "radius_mm {} is not smaller than the smallest half-extent {}; the rounded box would be inside out",
                pyfmt::g(r),
                pyfmt::g(bmin)
            )));
        }
        Ok(Some(String::new()))
    }
    fn aabb(&self, n: &Node) -> GResult<Option<([f64; 3], [f64; 3])>> {
        Ok(aabb_sym(b3(n)?))
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        _k: Vec<KernelBox<S>>,
        _c: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        Ok(Box::new(BoxK { b: triple(inp, BXYZ)?, r: Some(inp.scalar("radius_mm")?) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

leaf_kind!(
    Cylinder,
    CYLINDER,
    "cylinder",
    "Capped cylinder about local z: radius ``r``, half-height ``h``.",
    leaf_exact(),
    [
        ParamSpec::float("radius_mm", 1.0, "mm", "radius"),
        ParamSpec::float("half_height_mm", 1.0, "mm", "half height along local z")
    ]
);

struct CylinderK<S> {
    r: S,
    h: S,
}
impl<S: Scalar> Kernel<S> for CylinderK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        let rad = vlen2(x[0], x[1]);
        let dx = rad - self.r;
        let dz = x[2].abs() - self.h;
        let ox = dx.max_c(0.0);
        let oz = dz.max_c(0.0);
        let outside = (ox.powi(2) + oz.powi(2) + SAFE_EPS).sqrt();
        let inside = dx.max(dz).min_c(0.0);
        outside + inside
    }
}

impl NodeOp for Cylinder {
    fn info(&self) -> &KindInfo {
        Self::kind_info()
    }
    fn field_class(&self, _n: &Node, _k: &[FieldClass]) -> GResult<FieldClass> {
        Ok(FieldClass::exact())
    }
    fn aabb(&self, n: &Node) -> GResult<Option<([f64; 3], [f64; 3])>> {
        let r = n.pf("radius_mm")?;
        let h = n.pf("half_height_mm")?;
        Ok(aabb_sym([r, r, h]))
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        _k: Vec<KernelBox<S>>,
        _c: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        Ok(Box::new(CylinderK { r: inp.scalar("radius_mm")?, h: inp.scalar("half_height_mm")? }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

leaf_kind!(
    Capsule,
    CAPSULE,
    "capsule",
    "A sphere of radius ``r`` swept along the segment ``a -> b``.",
    leaf_exact(),
    [
        ParamSpec::float("ax_mm", 0.0, "mm", "segment start x"),
        ParamSpec::float("ay_mm", 0.0, "mm", "segment start y"),
        ParamSpec::float("az_mm", -1.0, "mm", "segment start z"),
        ParamSpec::float("bx_mm", 0.0, "mm", "segment end x"),
        ParamSpec::float("by_mm", 0.0, "mm", "segment end y"),
        ParamSpec::float("bz_mm", 1.0, "mm", "segment end z"),
        ParamSpec::float("radius_mm", 0.5, "mm", "sweep radius"),
    ]
);

struct CapsuleK<S> {
    a: [S; 3],
    ba: [S; 3],
    baba: S,
    r: S,
}
impl<S: Scalar> Kernel<S> for CapsuleK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        let pa = [x[0] - self.a[0], x[1] - self.a[1], x[2] - self.a[2]];
        let dot = pa[0] * self.ba[0] + pa[1] * self.ba[1] + pa[2] * self.ba[2];
        let h = (dot / (self.baba + SAFE_EPS)).clip_c(0.0, 1.0);
        vlen3([pa[0] - self.ba[0] * h, pa[1] - self.ba[1] * h, pa[2] - self.ba[2] * h]) - self.r
    }
}

impl NodeOp for Capsule {
    fn info(&self) -> &KindInfo {
        Self::kind_info()
    }
    fn field_class(&self, _n: &Node, _k: &[FieldClass]) -> GResult<FieldClass> {
        Ok(FieldClass::exact())
    }
    fn aabb(&self, n: &Node) -> GResult<Option<([f64; 3], [f64; 3])>> {
        let a = [n.pf("ax_mm")?, n.pf("ay_mm")?, n.pf("az_mm")?];
        let b = [n.pf("bx_mm")?, n.pf("by_mm")?, n.pf("bz_mm")?];
        let r = n.pf("radius_mm")?;
        Ok(Some((std::array::from_fn(|i| a[i].min(b[i]) - r), std::array::from_fn(|i| a[i].max(b[i]) + r))))
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        _k: Vec<KernelBox<S>>,
        _c: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        let a = triple(inp, ["ax_mm", "ay_mm", "az_mm"])?;
        let b = triple(inp, ["bx_mm", "by_mm", "bz_mm"])?;
        let ba = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let baba = ba[0] * ba[0] + ba[1] * ba[1] + ba[2] * ba[2];
        Ok(Box::new(CapsuleK { a, ba, baba, r: inp.scalar("radius_mm")? }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

leaf_kind!(
    Torus,
    TORUS,
    "torus",
    "Torus in the local xy-plane: major radius ``R``, minor radius ``r``.",
    leaf_exact(),
    [
        ParamSpec::float("major_mm", 2.0, "mm", "distance from the origin to the tube axis"),
        ParamSpec::float("minor_mm", 0.5, "mm", "tube radius"),
    ]
);

struct TorusK<S> {
    big: S,
    small: S,
}
impl<S: Scalar> Kernel<S> for TorusK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        let q = vlen2(x[0], x[1]) - self.big;
        vlen2(q, x[2]) - self.small
    }
}

impl NodeOp for Torus {
    fn info(&self) -> &KindInfo {
        Self::kind_info()
    }
    fn field_class(&self, _n: &Node, _k: &[FieldClass]) -> GResult<FieldClass> {
        Ok(FieldClass::exact())
    }
    fn aabb(&self, n: &Node) -> GResult<Option<([f64; 3], [f64; 3])>> {
        let big = n.pf("major_mm")?;
        let r = n.pf("minor_mm")?;
        Ok(Some(([-(big + r), -(big + r), -r], [big + r, big + r, r])))
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        _k: Vec<KernelBox<S>>,
        _c: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        Ok(Box::new(TorusK { big: inp.scalar("major_mm")?, small: inp.scalar("minor_mm")? }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

leaf_kind!(
    Plane,
    PLANE,
    "plane",
    "Half-space ``{x : n.x <= d}``.  ``f = n.x/|n| - d``.",
    leaf_exact(),
    [
        ParamSpec::float("nx", 0.0, "-", "normal x"),
        ParamSpec::float("ny", 0.0, "-", "normal y"),
        ParamSpec::float("nz", 1.0, "-", "normal z"),
        ParamSpec::float("offset_mm", 0.0, "mm", "signed distance from the origin"),
    ]
);

struct PlaneK<S> {
    n: [S; 3],
    nn: S,
    d: S,
}
impl<S: Scalar> Kernel<S> for PlaneK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        (x[0] * self.n[0] + x[1] * self.n[1] + x[2] * self.n[2]) / self.nn - self.d
    }
}

impl NodeOp for Plane {
    fn info(&self) -> &KindInfo {
        Self::kind_info()
    }
    fn field_class(&self, _n: &Node, _k: &[FieldClass]) -> GResult<FieldClass> {
        Ok(FieldClass::exact())
    }
    fn validate(&self, n: &Node) -> GResult<Option<String>> {
        let v = [n.pf("nx")?, n.pf("ny")?, n.pf("nz")?];
        if (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt() < 1e-9 {
            return Ok(Some(format!(
                "the normal ({}, {}, {}) is degenerate",
                pyfmt::g(v[0]),
                pyfmt::g(v[1]),
                pyfmt::g(v[2])
            )));
        }
        Ok(Some(String::new()))
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        _k: Vec<KernelBox<S>>,
        _c: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        let n = triple(inp, ["nx", "ny", "nz"])?;
        let nn = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2] + SAFE_EPS).sqrt();
        Ok(Box::new(PlaneK { n, nn, d: inp.scalar("offset_mm")? }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

leaf_kind!(
    Cone,
    CONE,
    "cone",
    "Capped cone about local z: radius ``r1`` at ``z = -h``, ``r2`` at ``z = +h``.",
    leaf_exact(),
    [
        ParamSpec::float("r1_mm", 1.0, "mm", "radius at z = -half_height"),
        ParamSpec::float("r2_mm", 0.25, "mm", "radius at z = +half_height"),
        ParamSpec::float("half_height_mm", 1.0, "mm", "half height along local z"),
    ]
);

struct ConeK<S> {
    r1: S,
    r2: S,
    h: S,
}
impl<S: Scalar> Kernel<S> for ConeK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        let qx = vlen2(x[0], x[1]);
        let qy = x[2];
        let cap_r = if qy.val() < 0.0 { self.r1 } else { self.r2 };
        let cax = qx - qx.min(cap_r);
        let cay = qy.abs() - self.h;
        let (k1x, k1y) = (self.r2, self.h);
        let (k2x, k2y) = (self.r2 - self.r1, self.h * 2.0);
        let dot2k2 = k2x * k2x + k2y * k2y + SAFE_EPS;
        let t = (((k1x - qx) * k2x + (k1y - qy) * k2y) / dot2k2).clip_c(0.0, 1.0);
        let cbx = qx - k1x + k2x * t;
        let cby = qy - k1y + k2y * t;
        let s = if cbx.val() < 0.0 && cay.val() < 0.0 { -1.0 } else { 1.0 };
        ((cax * cax + cay * cay).min(cbx * cbx + cby * cby) + SAFE_EPS).sqrt() * s
    }
}

impl NodeOp for Cone {
    fn info(&self) -> &KindInfo {
        Self::kind_info()
    }
    fn field_class(&self, _n: &Node, _k: &[FieldClass]) -> GResult<FieldClass> {
        Ok(FieldClass::exact())
    }
    fn validate(&self, n: &Node) -> GResult<Option<String>> {
        let r1 = n.pf("r1_mm")?;
        let r2 = n.pf("r2_mm")?;
        let h = n.pf("half_height_mm")?;
        if r1 < 0.0 || r2 < 0.0 {
            return Ok(Some(format!("radii must be non-negative ({}, {})", pyfmt::g(r1), pyfmt::g(r2))));
        }
        if h <= 0.0 {
            return Ok(Some(format!("half_height_mm {} must be positive", pyfmt::g(h))));
        }
        if r1 == 0.0 && r2 == 0.0 {
            return Ok(Some("both radii are zero; there is no cone".into()));
        }
        Ok(Some(String::new()))
    }
    fn aabb(&self, n: &Node) -> GResult<Option<([f64; 3], [f64; 3])>> {
        let r = n.pf("r1_mm")?.max(n.pf("r2_mm")?);
        let h = n.pf("half_height_mm")?;
        Ok(aabb_sym([r, r, h]))
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        _k: Vec<KernelBox<S>>,
        _c: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        Ok(Box::new(ConeK {
            r1: inp.scalar("r1_mm")?,
            r2: inp.scalar("r2_mm")?,
            h: inp.scalar("half_height_mm")?,
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

leaf_kind!(
    Constant,
    CONSTANT,
    "constant",
    "``f(x) = c``.  BOUND, and the class is not a formality.",
    Some(FieldClass::bound()),
    [ParamSpec::float("value_mm", 0.0, "mm", "the value, everywhere")]
);

struct ConstantK<S> {
    c: S,
}
impl<S: Scalar> Kernel<S> for ConstantK<S> {
    fn eval(&self, _x: [S; 3]) -> S {
        self.c + 0.0
    }
}

impl NodeOp for Constant {
    fn info(&self) -> &KindInfo {
        Self::kind_info()
    }
    fn field_class(&self, _n: &Node, _k: &[FieldClass]) -> GResult<FieldClass> {
        Ok(FieldClass::bound())
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        _k: Vec<KernelBox<S>>,
        _c: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        Ok(Box::new(ConstantK { c: inp.scalar("value_mm")? }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplineBasis {
    CatmullRom,
    BSpline,
}

impl SplineBasis {
    fn name(self) -> &'static str {
        match self {
            Self::CatmullRom => "catmull_rom",
            Self::BSpline => "b_spline",
        }
    }

    fn matrix(self) -> [[f64; 4]; 4] {
        match self {
            Self::CatmullRom => {
                [[0.0, 1.0, 0.0, 0.0], [-0.5, 0.0, 0.5, 0.0], [1.0, -2.5, 2.0, -0.5], [-0.5, 1.5, -1.5, 0.5]]
            }
            Self::BSpline => [
                [1.0 / 6.0, 4.0 / 6.0, 1.0 / 6.0, 0.0],
                [-3.0 / 6.0, 0.0, 3.0 / 6.0, 0.0],
                [3.0 / 6.0, -6.0 / 6.0, 3.0 / 6.0, 0.0],
                [-1.0 / 6.0, 3.0 / 6.0, -3.0 / 6.0, 1.0 / 6.0],
            ],
        }
    }
}

static SPLINE: StaticInfo = StaticInfo::new();

pub struct SplineCurve {
    pub basis: SplineBasis,
    pub closed: bool,
    pub revolve: bool,
    pub samples_per_segment: usize,
}

fn f64_tuple(v: &[f64]) -> ParamValue {
    ParamValue::Tuple(v.iter().map(|x| ParamValue::Float(*x)).collect())
}

impl SplineCurve {
    pub fn kind_info() -> &'static KindInfo {
        SPLINE.get(|| {
            kind_info(
                "spline_curve",
                MODULE,
                "A tube of radius ``thickness_mm`` around a planar cubic spline",
                "",
                leaf_exact(),
                true,
                &[],
                vec![
                    ParamSpec::with(
                        "control_r_mm",
                        Some(f64_tuple(&[1.0, 1.0, 1.0, 1.0])),
                        "mm",
                        "radial (revolve) or x (planar) coordinate of each control point",
                    ),
                    ParamSpec::with(
                        "control_z_mm",
                        Some(f64_tuple(&[-1.5, -0.5, 0.5, 1.5])),
                        "mm",
                        "axial coordinate of each control point",
                    ),
                    ParamSpec::float("thickness_mm", 0.25, "mm", "tube radius about the curve"),
                ],
            )
        })
    }

    fn construct(mut args: ConstructArgs) -> GResult<Node> {
        let basis = match args.take_attr("basis") {
            None => SplineBasis::CatmullRom,
            Some(Attr::Str(s)) if s == "catmull_rom" => SplineBasis::CatmullRom,
            Some(Attr::Str(s)) if s == "b_spline" => SplineBasis::BSpline,
            Some(other) => {
                return value_err(format!(
                    "basis {}; expected one of b_spline, catmull_rom",
                    other.py_obj().repr()
                ));
            }
        };
        let closed = args.take_attr("closed").is_some_and(|a| a.truthy());
        let revolve = args.take_attr("revolve").is_none_or(|a| a.truthy());
        let sps = match args.take_attr("samples_per_segment") {
            None => 16,
            Some(a) => {
                let v = a.as_f64().ok_or_else(|| {
                    GeometryError::Value(format!("invalid literal for int(): {}", a.py_obj().repr()))
                })?;
                #[allow(clippy::cast_possible_truncation)]
                let i = v.trunc() as i64;
                i
            }
        };
        if sps < 1 {
            return value_err("samples_per_segment must be at least 1");
        }
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let op = Arc::new(Self { basis, closed, revolve, samples_per_segment: sps as usize });
        args.attrs_into_params();
        Node::new(op, args.children, args.names, args.params)
    }

    #[must_use]
    pub fn entry() -> crate::node::KindEntry {
        entry(Self::kind_info(), Self::construct)
    }

    fn n_segments(&self, n: usize) -> usize {
        if self.closed { n } else { n.saturating_sub(1) }
    }

    #[must_use]
    pub fn weights(&self, n: usize, t: &[f64], derivative: usize) -> Vec<Vec<f64>> {
        let ns = self.n_segments(n);
        let m = self.basis.matrix();
        let mut w = vec![vec![0.0; n]; t.len()];
        for (row, &tv) in t.iter().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let seg = (tv.floor() as i64).clamp(0, i64::try_from(ns).unwrap_or(1) - 1);
            #[allow(clippy::cast_precision_loss)]
            let u = tv - seg as f64;
            let basis = match derivative {
                0 => [1.0, u, u * u, u.powi(3)],
                1 => [0.0, 1.0, 2.0 * u, 3.0 * u * u],
                _ => [0.0, 0.0, 2.0, 6.0 * u],
            };
            let mut w4 = [0.0; 4];
            for (col, wc) in w4.iter_mut().enumerate() {
                *wc =
                    basis[0] * m[0][col] + basis[1] * m[1][col] + basis[2] * m[2][col] + basis[3] * m[3][col];
            }
            let ni = i64::try_from(n).unwrap_or(0);
            for (col, wc) in w4.iter().enumerate() {
                let i = seg - 1 + i64::try_from(col).unwrap_or(0);
                if self.closed {
                    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                    let j = i.rem_euclid(ni) as usize;
                    w[row][j] += wc;
                } else if i == -1 {
                    w[row][0] += 2.0 * wc;
                    w[row][1] -= wc;
                } else if i == ni {
                    w[row][n - 1] += 2.0 * wc;
                    w[row][n - 2] -= wc;
                } else {
                    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                    let j = i as usize;
                    w[row][j] += wc;
                }
            }
        }
        w
    }

    #[must_use]
    pub fn sample_parameters(&self, n: usize) -> Vec<f64> {
        let ns = self.n_segments(n);
        let m = self.samples_per_segment;
        #[allow(clippy::cast_precision_loss)]
        let mut t: Vec<f64> = (0..ns * m).map(|i| i as f64 / m as f64).collect();
        if !self.closed {
            #[allow(clippy::cast_precision_loss)]
            t.push(ns as f64);
        }
        t
    }

    fn controls(node: &Node) -> GResult<(Vec<f64>, Vec<f64>)> {
        let r = node.param("control_r_mm").map(ParamValue::to_f64_array).transpose().ok().flatten();
        let z = node.param("control_z_mm").map(ParamValue::to_f64_array).transpose().ok().flatten();
        match (r, z) {
            (Some((_, r)), Some((_, z))) => Ok((r, z)),
            _ => value_err("control_r_mm and control_z_mm must be numeric"),
        }
    }


    pub fn polyline(&self, node: &Node) -> GResult<Vec<[f64; 2]>> {
        let (r, z) = Self::controls(node)?;
        let n = r.len();
        let w = self.weights(n, &self.sample_parameters(n), 0);
        Ok(w.iter()
            .map(|row| {
                let mut a = [0.0, 0.0];
                for j in 0..n {
                    a[0] += row[j] * r[j];
                    a[1] += row[j] * z[j];
                }
                a
            })
            .collect())
    }


    pub fn evaluate(&self, node: &Node, t: &[f64]) -> GResult<serde_json::Value> {
        let (r, z) = Self::controls(node)?;
        let n = r.len();
        let apply = |w: &Vec<Vec<f64>>, v: &[f64]| -> Vec<f64> {
            w.iter().map(|row| (0..n).map(|j| row[j] * v[j]).sum()).collect()
        };
        let (w0, w1, w2) = (self.weights(n, t, 0), self.weights(n, t, 1), self.weights(n, t, 2));
        let (rr, zz) = (apply(&w0, &r), apply(&w0, &z));
        let (r1, z1) = (apply(&w1, &r), apply(&w1, &z));
        let (r2, z2) = (apply(&w2, &r), apply(&w2, &z));
        let kappa: Vec<f64> = (0..t.len())
            .map(|i| {
                let s2 = r1[i] * r1[i] + z1[i] * z1[i] + SAFE_EPS;
                (r1[i] * z2[i] - z1[i] * r2[i]) / s2.powf(1.5)
            })
            .collect();
        Ok(serde_json::json!({"t": t, "r": rr, "z": zz, "dr_dt": r1, "dz_dt": z1, "curvature": kappa}))
    }
}

struct SplineK<S> {
    a: Vec<[S; 2]>,
    ab: Vec<[S; 2]>,
    abab: Vec<S>,
    t: S,
    revolve: bool,
}

impl<S: Scalar> Kernel<S> for SplineK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        let q = if self.revolve { [vlen2(x[0], x[1]), x[2]] } else { [x[0], x[2]] };
        let mut d = Vec::with_capacity(self.a.len());
        for i in 0..self.a.len() {
            let pa = [q[0] - self.a[i][0], q[1] - self.a[i][1]];
            let ab = self.ab[i];
            let h = ((pa[0] * ab[0] + pa[1] * ab[1]) / (self.abab[i] + SAFE_EPS)).clip_c(0.0, 1.0);
            let e = [pa[0] - ab[0] * h, pa[1] - ab[1] * h];
            d.push((e[0] * e[0] + e[1] * e[1] + SAFE_EPS).sqrt());
        }
        reduce_min(&d) - self.t
    }
}

impl NodeOp for SplineCurve {
    fn info(&self) -> &KindInfo {
        Self::kind_info()
    }
    fn struct_tokens(&self) -> Vec<String> {
        vec![
            format!("basis={}", pyfmt::str_repr(self.basis.name())),
            format!("closed={}", PyObj::Bool(self.closed).repr()),
            format!("revolve={}", PyObj::Bool(self.revolve).repr()),
            format!("samples_per_segment={}", self.samples_per_segment),
        ]
    }
    fn struct_json(&self) -> Vec<(String, serde_json::Value)> {
        vec![
            ("basis".into(), self.basis.name().into()),
            ("closed".into(), self.closed.into()),
            ("revolve".into(), self.revolve.into()),
            ("samples_per_segment".into(), self.samples_per_segment.into()),
        ]
    }
    fn doc_attrs(&self) -> Vec<(String, Attr)> {
        Vec::new()
    }
    fn field_class(&self, _n: &Node, _k: &[FieldClass]) -> GResult<FieldClass> {
        Ok(FieldClass::exact())
    }
    fn validate(&self, n: &Node) -> GResult<Option<String>> {
        let ra = n.param("control_r_mm").and_then(|v| v.as_ndarray().ok());
        let za = n.param("control_z_mm").and_then(|v| v.as_ndarray().ok());
        let (Some(ra), Some(za)) = (ra, za) else {
            return value_err("control points must be numeric");
        };
        let t = n.pf("thickness_mm")?;
        if ra.ndim() != 1 || za.ndim() != 1 || ra.shape() != za.shape() {
            return Ok(Some(format!(
                "control_r_mm and control_z_mm must be 1-D and the same length, got {} and {}",
                pyfmt::shape_str(ra.shape()),
                pyfmt::shape_str(za.shape())
            )));
        }
        if ra.shape()[0] < 2 {
            return Ok(Some(format!("a spline needs at least two control points, got {}", ra.shape()[0])));
        }
        let (r, z) = (ra.to_f64_vec(), za.to_f64_vec());
        if !(r.iter().all(|v| v.is_finite()) && z.iter().all(|v| v.is_finite())) {
            return Ok(Some("control points must be finite".into()));
        }
        if self.revolve && r.iter().any(|v| *v < 0.0) {
            let mn = r.iter().copied().fold(f64::INFINITY, f64::min);
            return Ok(Some(format!(
                "control_r_mm has a negative radius (min {}); a revolved profile lives in r >= 0",
                pyfmt::g(mn)
            )));
        }
        if t < 0.0 {
            return Ok(Some(format!("thickness_mm {} is negative", pyfmt::g(t))));
        }
        Ok(Some(String::new()))
    }
    fn aabb(&self, n: &Node) -> GResult<Option<([f64; 3], [f64; 3])>> {
        let v = self.polyline(n)?;
        let t = n.pf("thickness_mm")?;
        let zlo = v.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min) - t;
        let zhi = v.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max) + t;
        if self.revolve {
            let r = v.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max) + t;
            return Ok(Some(([-r, -r, zlo], [r, r, zhi])));
        }
        Ok(None)
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        _k: Vec<KernelBox<S>>,
        _c: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        let r = &inp.array("control_r_mm")?.data;
        let z = &inp.array("control_z_mm")?.data;
        let n = r.len();
        let w = self.weights(n, &self.sample_parameters(n), 0);
        let v: Vec<[S; 2]> = w
            .iter()
            .map(|row| {
                let mut a = [S::cst(0.0), S::cst(0.0)];
                for j in 0..n {
                    a[0] = a[0] + r[j] * row[j];
                    a[1] = a[1] + z[j] * row[j];
                }
                a
            })
            .collect();
        let (a, b): (Vec<[S; 2]>, Vec<[S; 2]>) = if self.closed {
            let mut b = v[1..].to_vec();
            b.push(v[0]);
            (v.clone(), b)
        } else {
            (v[..v.len() - 1].to_vec(), v[1..].to_vec())
        };
        let ab: Vec<[S; 2]> = a.iter().zip(&b).map(|(p, q)| [q[0] - p[0], q[1] - p[1]]).collect();
        let abab = ab.iter().map(|e| e[0] * e[0] + e[1] * e[1]).collect();
        Ok(Box::new(SplineK { a, ab, abab, t: inp.scalar("thickness_mm")?, revolve: self.revolve }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[must_use]
pub fn entries() -> KernelEntryList {
    vec![
        Sphere::entry(),
        BoxKind::entry(),
        RoundedBox::entry(),
        Cylinder::entry(),
        Capsule::entry(),
        Torus::entry(),
        Plane::entry(),
        Cone::entry(),
        Constant::entry(),
        SplineCurve::entry(),
    ]
}

#[must_use]
pub fn params(items: &[(&str, f64)]) -> BTreeMap<String, ParamValue> {
    items.iter().map(|(k, v)| ((*k).to_string(), ParamValue::Float(*v))).collect()
}
