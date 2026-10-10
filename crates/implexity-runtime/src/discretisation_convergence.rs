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


use implexity_optim::numeric::{float_value, format_g6};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvergenceStatus {
    Sufficient,
    Insufficient,
    EvidenceMissing,
    NonAsymptotic,
}

impl ConvergenceStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sufficient => "sufficient",
            Self::Insufficient => "insufficient",
            Self::EvidenceMissing => "evidence_missing",
            Self::NonAsymptotic => "non_asymptotic",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RefinementSample {
    pub spacing: f64,
    pub response: f64,
    pub gradient: Option<Vec<f64>>,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConvergenceAssessment {
    pub status: ConvergenceStatus,
    pub response_relative_change: Option<f64>,
    pub gradient_relative_change: Option<f64>,
    pub observed_response_order: Option<f64>,
    pub levels: usize,
    pub reasons: Vec<String>,
}

impl ConvergenceAssessment {
    #[must_use]
    pub fn sufficient(&self) -> bool {
        self.status == ConvergenceStatus::Sufficient
    }

    #[must_use]
    pub fn as_dict(&self) -> Value {
        let opt = |v: Option<f64>| v.map_or(Value::Null, float_value);
        json!({
            "status": self.status.as_str(),
            "response_relative_change": opt(self.response_relative_change),
            "gradient_relative_change": opt(self.gradient_relative_change),
            "observed_response_order": opt(self.observed_response_order),
            "levels": self.levels,
            "reasons": self.reasons,
        })
    }

    fn missing(levels: usize, reason: String) -> Self {
        Self {
            status: ConvergenceStatus::EvidenceMissing,
            response_relative_change: None,
            gradient_relative_change: None,
            observed_response_order: None,
            levels,
            reasons: vec![reason],
        }
    }
}

fn relative_change(a: f64, b: f64) -> f64 {
    (a - b).abs() / 1.0_f64.max(a.abs()).max(b.abs())
}

fn norm(v: &[f64]) -> f64 {
    v.iter().map(|x| x * x).sum::<f64>().sqrt()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RefinementOptions {
    pub response_tolerance: f64,
    pub gradient_tolerance: f64,
    pub minimum_levels: usize,
    pub require_gradient: bool,
}

impl Default for RefinementOptions {
    fn default() -> Self {
        Self { response_tolerance: 0.01, gradient_tolerance: 0.05, minimum_levels: 3, require_gradient: true }
    }
}


pub fn assess_refinement(
    samples: &[RefinementSample],
    options: &RefinementOptions,
) -> Result<ConvergenceAssessment, String> {
    if options.minimum_levels < 3 {
        return Err("minimum_levels must be at least three for observed-order evidence".into());
    }
    for (name, value) in [
        ("response_tolerance", options.response_tolerance),
        ("gradient_tolerance", options.gradient_tolerance),
    ] {
        if !value.is_finite() || value < 0.0 {
            return Err(format!("{name} must be finite and non-negative"));
        }
    }
    let mut rows: Vec<&RefinementSample> = samples.iter().collect();
    rows.sort_by(|a, b| b.spacing.partial_cmp(&a.spacing).unwrap_or(std::cmp::Ordering::Equal));
    let n = rows.len();
    if n < options.minimum_levels {
        return Ok(ConvergenceAssessment::missing(
            n,
            format!("at least {} refinement levels are required", options.minimum_levels),
        ));
    }
    let spacings: Vec<f64> = rows.iter().map(|r| r.spacing).collect();
    let responses: Vec<f64> = rows.iter().map(|r| r.response).collect();
    if spacings.iter().any(|s| !s.is_finite() || *s <= 0.0) || !spacings.windows(2).all(|w| w[1] - w[0] < 0.0)
    {
        return Ok(ConvergenceAssessment::missing(
            n,
            "spacings must be finite, positive and strictly refined".into(),
        ));
    }
    if responses.iter().any(|r| !r.is_finite()) {
        return Ok(ConvergenceAssessment::missing(n, "responses must be finite".into()));
    }
    let mut reasons = Vec::new();
    let response_change = relative_change(responses[n - 1], responses[n - 2]);
    if response_change > options.response_tolerance {
        reasons.push(format!(
            "finest response change {} exceeds {}",
            format_g6(response_change),
            format_g6(options.response_tolerance)
        ));
    }
    let mut gradient_change = None;
    let finest = (&rows[n - 2].gradient, &rows[n - 1].gradient);
    if options.require_gradient && finest.0.is_none() && finest.1.is_none() {
        reasons.push("gradient evidence is required on the two finest levels".into());
    } else if finest.0.is_some() || finest.1.is_some() {
        match finest {
            (Some(coarse), Some(fine)) => {
                if coarse.len() != fine.len() || coarse.iter().chain(fine).any(|v| !v.is_finite()) {
                    reasons.push("finest-level gradients must have equal shapes and finite values".into());
                } else {
                    let diff: Vec<f64> = fine.iter().zip(coarse).map(|(f, c)| f - c).collect();
                    let change = norm(&diff) / 1.0_f64.max(norm(fine)).max(norm(coarse));
                    gradient_change = Some(change);
                    if change > options.gradient_tolerance {
                        reasons.push(format!(
                            "finest gradient change {} exceeds {}",
                            format_g6(change),
                            format_g6(options.gradient_tolerance)
                        ));
                    }
                }
            }
            _ => reasons.push("gradient evidence is incomplete on the two finest levels".into()),
        }
    }
    let mut observed = None;
    let d0 = (responses[n - 3] - responses[n - 2]).abs();
    let d1 = (responses[n - 2] - responses[n - 1]).abs();
    let ratio_h0 = spacings[n - 3] / spacings[n - 2];
    let ratio_h1 = spacings[n - 2] / spacings[n - 1];
    let status = if d0 > 0.0 && d1 > 0.0 && (ratio_h0 - ratio_h1).abs() <= 0.05 * ratio_h0.max(ratio_h1) {
        let order = (d0 / d1).ln() / (0.5 * (ratio_h0 + ratio_h1)).ln();
        observed = Some(order);
        if !order.is_finite() || order <= 0.0 {
            reasons.push("response sequence is not in a positive-order asymptotic regime".into());
            ConvergenceStatus::NonAsymptotic
        } else if reasons.is_empty() {
            ConvergenceStatus::Sufficient
        } else {
            ConvergenceStatus::Insufficient
        }
    } else if reasons.is_empty() {
        ConvergenceStatus::Sufficient
    } else {
        ConvergenceStatus::Insufficient
    };
    Ok(ConvergenceAssessment {
        status,
        response_relative_change: Some(response_change),
        gradient_relative_change: gradient_change,
        observed_response_order: observed,
        levels: n,
        reasons,
    })
}
