// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
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
