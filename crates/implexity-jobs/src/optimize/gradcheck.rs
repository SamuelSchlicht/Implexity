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


use std::sync::Arc;

use implexity_ad::AdError;
use serde_json::{Value, json};

use super::problem::{Design, Problem};
use super::spec::{OptimizeSpec, model_drive_of};
use crate::error::{JobError, JobResult};

fn ad_message(e: &AdError) -> String {
    match e {
        AdError::NonFinite(m) | AdError::Invalid(m) | AdError::Shape(m) | AdError::Singular(m) => m.clone(),
        other @ AdError::Callback(_) => other.to_string(),
    }
}


pub fn fd_vs_ad_probe(
    spec: &Arc<OptimizeSpec>,
    slot: Option<&str>,
    index: i64,
    step: Option<f64>,
) -> JobResult<Value> {
    if step.is_some_and(|s| !(s.is_finite() && s > 0.0)) {
        return Err(JobError::value("gradient-check step must be finite and positive"));
    }
    let mut prob = Problem::new(spec, None, false)?;
    prob.calibrate()?;
    let scaling = spec.settings.get("scaling").and_then(Value::as_str).unwrap_or("unit_range");
    let design = Design::new(&spec.free, scaling);
    let p = design.start();
    let drive = if spec.settings.get("steerable").is_some_and(implexity_core::pyobj::truthy) {
        Some(model_drive_of(spec)?)
    } else {
        None
    };
    let warm0 = prob.warm0.clone();
    let slot = match slot {
        Some(s) => s.to_string(),
        None => spec.free.first().map(|f| f.slot.clone()).unwrap_or_default(),
    };
    let Some(x_arr) = p.get(&slot) else {
        return Err(JobError::value(format!(
            "slot {} is not a free coordinate; free slots are: {}",
            implexity_core::py_repr::repr_str(&slot),
            p.names().join(", ")
        )));
    };
    let x: Vec<f64> = x_arr.iter().copied().collect();
    let Some(i) = usize::try_from(index).ok().filter(|i| *i < x.len()) else {
        return Err(JobError::value(format!(
            "index {index} out of range for slot {} of size {}",
            implexity_core::py_repr::repr_str(&slot),
            x.len()
        )));
    };
    if !x.iter().all(|v| v.is_finite()) {
        return Err(JobError::value("gradient-check design must be finite"));
    }
    let base = prob.evaluate(&p, warm0.as_ref(), drive.as_ref(), true, true, &[])?;
    let grad = base
        .grad
        .as_ref()
        .and_then(|g| g.get(&slot))
        .ok_or_else(|| JobError::runtime("the loss produced no gradient"))?;
    let ad = grad.iter().nth(i).copied().unwrap_or(f64::NAN);
    let shape = x_arr.shape().to_vec();
    let mut inner: Option<JobError> = None;
    let report = implexity_ad::gradcheck::fd_vs_ad_probe(
        |xs: &[f64]| {
            let mut pp = p.clone();
            let moved = ndarray::ArrayD::from_shape_vec(ndarray::IxDyn(&shape), xs.to_vec())
                .map_err(|e| AdError::Shape(e.to_string()))?;
            pp.insert(slot.clone(), moved);
            match prob.evaluate(&pp, warm0.as_ref(), drive.as_ref(), true, false, &[]) {
                Ok(e) => Ok(e.total),
                Err(e) => {
                    inner = Some(e);
                    Err(AdError::Invalid("the perturbed loss failed".into()))
                }
            }
        },
        &x,
        i,
        base.total,
        ad,
        step,
    );
    if let Some(e) = inner {
        return Err(e);
    }
    let r = report.map_err(|e| JobError::value(ad_message(&e)))?;
    Ok(json!({
        "slot": slot, "index": r.index, "x": r.x, "h": r.h, "dtype": "float64",
        "value": r.value, "ad": r.ad, "fd": r.fd, "rel_err": r.rel_err, "comparison": r.comparison,
    }))
}

fn write_err(text: &str) {
    let mut err = std::io::stderr().lock();
    let _ = std::io::Write::write_all(&mut err, text.as_bytes());
}

fn write_out(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = std::io::Write::write_all(&mut out, text.as_bytes());
}

#[must_use]
pub fn run_check(spec_file: &str, slot: Option<&str>, index: i64, step: Option<f64>, tol: f64) -> i32 {
    const PROG: &str = "implexity check-gradients";
    let result = (|| -> JobResult<Value> {
        let text = std::fs::read_to_string(spec_file).map_err(|e| {
            let class =
                if e.kind() == std::io::ErrorKind::NotFound { "FileNotFoundError" } else { "OSError" };
            JobError::of(
                class,
                format!(
                    "[Errno {}] {}: {}",
                    e.raw_os_error().unwrap_or(0),
                    if e.kind() == std::io::ErrorKind::NotFound {
                        "No such file or directory"
                    } else {
                        "cannot read file"
                    },
                    implexity_core::py_repr::repr_str(spec_file)
                ),
            )
        })?;
        let js = implexity_core::json::parse_with(
            &text,
            implexity_core::json::ParseOptions { reject_duplicate_keys: false },
        )
        .map_err(|e| JobError::value(e.to_string()))?;
        let (_, spec) = super::spec_from_job_spec(&js)?;
        let rep = fd_vs_ad_probe(&spec, slot, index, step)?;
        if !["x", "h", "value", "ad", "fd", "rel_err"]
            .iter()
            .all(|k| rep[*k].as_f64().is_some_and(f64::is_finite))
        {
            return Err(JobError::value("nonfinite gradient-check evidence cannot be reported as agreement"));
        }
        Ok(rep)
    })();
    let rep = match result {
        Ok(r) => r,
        Err(e)
            if matches!(e, JobError::Cae(_))
                || e.is_value_error()
                || e.python_class() == "JSONDecodeError" =>
        {
            write_err(&format!("{PROG}: {}\n", e.message()));
            return 2;
        }
        Err(e) => {
            let problems = match &e {
                JobError::Problems { problems, .. } if !problems.is_empty() => problems.clone(),
                other => vec![other.describe()],
            };
            write_err(&format!("{PROG}: the problem was refused:\n"));
            for line in problems {
                write_err(&format!("  - {line}\n"));
            }
            return 2;
        }
    };
    let f = |k: &str| rep[k].as_f64().unwrap_or(f64::NAN);
    let g = implexity_core::extensions::format_g;
    let e = implexity_io::provenance::format_e;
    write_out(&format!(
        "{PROG}: slot={} index={} x={} h={} ({})  L={}  AD={}  FD={}  rel_err={}  tol={}\n",
        rep["slot"].as_str().unwrap_or(""),
        rep["index"],
        g(f("x"), 6),
        g(f("h"), 3),
        rep["dtype"].as_str().unwrap_or(""),
        e(f("value"), 6),
        e(f("ad"), 6),
        e(f("fd"), 6),
        e(f("rel_err"), 2),
        e(tol, 2)
    ));
    if rep["comparison"].as_str() == Some("zero_direction") {
        write_out("Both sampled derivatives are zero. This direction does not verify nonzero sensitivity.\n");
    }
    if f("rel_err") > tol {
        write_err(&format!(
            "{PROG}: FAIL -- the AD gradient does not agree with a central finite difference on the sampled \
             coordinate. Do not trust an optimisation of this spec until this is explained (step too large for a \
             nonsmooth term, or a broken AD tape).\n"
        ));
        return 1;
    }
    0
}
