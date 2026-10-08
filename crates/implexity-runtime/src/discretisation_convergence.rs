// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
