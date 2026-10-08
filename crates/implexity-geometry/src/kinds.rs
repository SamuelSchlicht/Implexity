// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::{Arc, OnceLock};

use crate::eval::SAFE_EPS;
use crate::fieldclass::FieldClass;
use crate::node::{ConstructArgs, Constructor, KindEntry, KindInfo, ParamSpec};
use crate::scalar::{Scalar, reduce_max};

#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn kind_info(
    kind: &str,
    module: &str,
    doc: &str,
    field_class_doc: &str,
    leaf_class: Option<FieldClass>,
    is_struct: bool,
    discrete: &[&str],
    params: Vec<ParamSpec>,
) -> KindInfo {
    KindInfo {
        kind: kind.into(),
        params,
        discrete: discrete.iter().map(|s| (*s).to_string()).collect(),
        module: module.into(),
        doc: doc.into(),
        field_class_doc: field_class_doc.into(),
        leaf_class,
        is_struct,
        glsl_refusal: None,
    }
}

pub struct StaticInfo(pub OnceLock<KindInfo>);

impl StaticInfo {
    #[must_use]
    pub const fn new() -> Self {
        Self(OnceLock::new())
    }

    pub fn get(&'static self, f: impl FnOnce() -> KindInfo) -> &'static KindInfo {
        self.0.get_or_init(f)
    }
}

impl Default for StaticInfo {
    fn default() -> Self {
        Self::new()
    }
}

#[must_use]
pub fn entry(
    info: &'static KindInfo,
    f: fn(ConstructArgs) -> crate::error::GResult<crate::node::Node>,
) -> KindEntry {
    let construct: Constructor = Arc::new(f);
    KindEntry { info: Arc::new(info.clone()), construct }
}

#[inline]
pub fn vlen3<S: Scalar>(v: [S; 3]) -> S {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2] + SAFE_EPS).sqrt()
}

#[inline]
pub fn vlen2<S: Scalar>(a: S, b: S) -> S {
    (a * a + b * b + SAFE_EPS).sqrt()
}

#[inline]
pub fn max3<S: Scalar>(v: [S; 3]) -> S {
    reduce_max(&v)
}

#[inline]
pub fn smooth_min<S: Scalar>(a: S, b: S, k: S, exp_kind: bool) -> S {
    if exp_kind {
        let m = a.min(b);
        return m - k * (-((a - b).abs()) / k).exp().ln_1p();
    }
    let h = ((b - a) * 0.5 / k + 0.5).clip_c(0.0, 1.0);
    b + h * (a - b) - k * h * (-h + 1.0)
}

#[inline]
pub fn smooth_max<S: Scalar>(a: S, b: S, k: S, exp_kind: bool) -> S {
    -smooth_min(-a, -b, k, exp_kind)
}


pub fn triple<S: Scalar>(
    inputs: &crate::node::KernelInputs<S>,
    names: [&str; 3],
) -> crate::error::GResult<[S; 3]> {
    Ok([inputs.scalar(names[0])?, inputs.scalar(names[1])?, inputs.scalar(names[2])?])
}
