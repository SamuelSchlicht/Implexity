// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::sync::Arc;

use crate::error::{GResult, model_err};
use crate::eval::{GradStats, aabb_of, gradient_statistics};
use crate::fieldclass::FieldClass;
use crate::kinds::{StaticInfo, entry, kind_info};
use crate::node::{
    Attr, ConstructArgs, EvalCtx, Kernel, KernelBox, KernelEntryList, KernelInputs, KindInfo, Mode, Node,
    NodeOp, NodeRef, ParamSpec,
};
use crate::pyfmt;
use crate::scalar::Scalar;

pub const TPMS_FAMILIES: [(&str, f64); 7] = [
    ("gyroid", 2.0),
    ("schwarz_p", 1.0),
    ("schwarz_d", 4.0),
    ("iwp", 6.0),
    ("neovius", 7.0),
    ("frd", 8.0),
    ("lidinoid", 4.0),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Gyroid,
    SchwarzP,
    SchwarzD,
    Iwp,
    Neovius,
    Frd,
    Lidinoid,
}

impl Family {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "gyroid" => Self::Gyroid,
            "schwarz_p" => Self::SchwarzP,
            "schwarz_d" => Self::SchwarzD,
            "iwp" => Self::Iwp,
            "neovius" => Self::Neovius,
            "frd" => Self::Frd,
            "lidinoid" => Self::Lidinoid,
            _ => return None,
        })
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Gyroid => "gyroid",
            Self::SchwarzP => "schwarz_p",
            Self::SchwarzD => "schwarz_d",
            Self::Iwp => "iwp",
            Self::Neovius => "neovius",
            Self::Frd => "frd",
            Self::Lidinoid => "lidinoid",
        }
    }

    #[must_use]
    pub fn constant(self) -> f64 {
        TPMS_FAMILIES.iter().find(|(n, _)| *n == self.name()).map_or(1.0, |(_, c)| *c)
    }

    pub fn value<S: Scalar>(self, u: [S; 3]) -> S {
        let (c1, c2, c3) = (u[0].cos(), u[1].cos(), u[2].cos());
        let (s1, s2, s3) = (u[0].sin(), u[1].sin(), u[2].sin());
        match self {
            Self::Gyroid => s1 * c2 + s2 * c3 + s3 * c1,
            Self::SchwarzP => c1 + c2 + c3,
            Self::SchwarzD => s1 * s2 * s3 + s1 * c2 * c3 + c1 * s2 * c3 + c1 * c2 * s3,
            _ => {
                let (cc1, cc2, cc3) = ((u[0] * 2.0).cos(), (u[1] * 2.0).cos(), (u[2] * 2.0).cos());
                match self {
                    Self::Iwp => (c1 * c2 + c2 * c3 + c3 * c1) * 2.0 - (cc1 + cc2 + cc3),
                    Self::Neovius => (c1 + c2 + c3) * 3.0 + c1 * 4.0 * c2 * c3,
                    Self::Frd => c1 * 4.0 * c2 * c3 - (cc1 * cc2 + cc2 * cc3 + cc3 * cc1),
                    _ => {
                        let (ss1, ss2, ss3) = ((u[0] * 2.0).sin(), (u[1] * 2.0).sin(), (u[2] * 2.0).sin());
                        (ss1 * c2 * s3 + ss2 * c3 * s1 + ss3 * c1 * s2) * 0.5
                            - (cc1 * cc2 + cc2 * cc3 + cc3 * cc1) * 0.5
                            + 0.15
                    }
                }
            }
        }
    }
}

fn family_list() -> String {
    let mut v: Vec<&str> = TPMS_FAMILIES.iter().map(|(n, _)| *n).collect();
    v.sort_unstable();
    v.join(", ")
}

static TPMS: StaticInfo = StaticInfo::new();

pub struct Tpms {
    pub family: Family,
    pub proven: bool,
}

impl Tpms {
    pub fn kind_info() -> &'static KindInfo {
        TPMS.get(|| {
            kind_info(
                "tpms",
                "implexity.implicit.lattice_ops",
                "One triply-periodic minimal surface, normalised so that it is ``BOUND``.",
                "",
                None,
                true,
                &[],
                vec![
                    ParamSpec::float("level", 0.0, "-", "iso-level of the nodal function"),
                    ParamSpec::float("period_mm", 4.0, "mm", "unit cell size"),
                ],
            )
        })
    }

    #[must_use]
    pub fn grad_bound(&self) -> f64 {
        self.family.constant() * 3f64.sqrt()
    }

    fn construct(mut args: ConstructArgs) -> GResult<Node> {
        let family = match args.take_attr("family") {
            None => Family::Gyroid,
            Some(a) => match a.as_str().and_then(Family::parse) {
                Some(f) => f,
                None => {
                    return model_err(format!(
                        "no TPMS family {}; the families are {}",
                        a.py_obj().repr(),
                        family_list()
                    ));
                }
            },
        };
        let proven = match args.take_attr("normalise") {
            None => true,
            Some(Attr::Str(s)) if s == "proven" => true,
            Some(Attr::Str(s)) if s == "none" => false,
            Some(a) => {
                return model_err(format!(
                    "tpms normalise {}; expected 'proven' or 'none'",
                    a.py_obj().repr()
                ));
            }
        };
        args.attrs_into_params();
        let node = Node::new(Arc::new(Self { family, proven }), args.children, args.names, args.params)?;
        if !node.children().is_empty() {
            return model_err("tpms is a leaf and takes no children");
        }
        let p = node.pf("period_mm")?;
        if !(p > 0.0) || !p.is_finite() {
            return model_err(format!("tpms period_mm must be finite and positive, got {}", pyfmt::g(p)));
        }
        Ok(node)
    }

    #[must_use]
    pub fn entry() -> crate::node::KindEntry {
        entry(Self::kind_info(), Self::construct)
    }
}

struct TpmsK<S> {
    family: Family,
    k0: S,
    level: S,
    scale: Option<S>,
}
impl<S: Scalar> Kernel<S> for TpmsK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        let g = self.family.value([x[0] * self.k0, x[1] * self.k0, x[2] * self.k0]) - self.level;
        match self.scale {
            None => g,
            Some(s) => g / s,
        }
    }
}

impl NodeOp for Tpms {
    fn info(&self) -> &KindInfo {
        Self::kind_info()
    }
    fn struct_tokens(&self) -> Vec<String> {
        vec![
            format!("family={}", pyfmt::str_repr(self.family.name())),
            format!("normalise={}", pyfmt::str_repr(if self.proven { "proven" } else { "none" })),
        ]
    }
    fn struct_json(&self) -> Vec<(String, serde_json::Value)> {
        vec![
            ("family".into(), self.family.name().into()),
            ("normalise".into(), (if self.proven { "proven" } else { "none" }).into()),
        ]
    }
    fn doc_attrs(&self) -> Vec<(String, Attr)> {
        let mut out = Vec::new();
        if self.family != Family::Gyroid {
            out.push(("family".into(), Attr::Str(self.family.name().into())));
        }
        if !self.proven {
            out.push(("normalise".into(), Attr::Str("none".into())));
        }
        out
    }
    fn validate(&self, n: &Node) -> GResult<Option<String>> {
        let p = n.pf("period_mm")?;
        if !(p > 0.0) || !p.is_finite() {
            return Ok(Some(format!("period_mm must be finite and positive, got {}", pyfmt::g(p))));
        }
        Ok(Some(String::new()))
    }
    fn field_class(&self, _n: &Node, _k: &[FieldClass]) -> GResult<FieldClass> {
        Ok(if self.proven { FieldClass::bound() } else { FieldClass::implicit() })
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        _k: Vec<KernelBox<S>>,
        _c: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        let p = inp.scalar("period_mm")?;
        let k0 = S::cst(2.0 * std::f64::consts::PI) / p;
        let scale = self.proven.then(|| k0 * self.grad_bound());
        Ok(Box::new(TpmsK { family: self.family, k0, level: inp.scalar("level")?, scale }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}


pub fn measure_lipschitz(
    node: &NodeRef,
    n: usize,
    seed: u64,
    region: Option<([f64; 3], [f64; 3])>,
    mode: Mode,
    batch: usize,
) -> GResult<GradStats> {
    let Some((lo, hi)) = region.or_else(|| aabb_of(node)) else {
        return model_err(
            "measure_lipschitz needs a region: this node has no bounding box, so pass lo= and hi=",
        );
    };
    let mut rng = implexity_core::rng::default_rng(u128::from(seed));
    let mut pts = Vec::with_capacity(n);
    let mut done = 0;
    while done < n {
        let k = batch.max(1).min(n - done);
        let u = rng.random_vec(3 * k);
        for c in u.chunks_exact(3) {
            pts.push([
                lo[0] + (hi[0] - lo[0]) * c[0],
                lo[1] + (hi[1] - lo[1]) * c[1],
                lo[2] + (hi[2] - lo[2]) * c[2],
            ]);
        }
        done += k;
    }
    gradient_statistics(node, &pts, mode)
}

#[must_use]
pub fn entries() -> KernelEntryList {
    vec![Tpms::entry()]
}
