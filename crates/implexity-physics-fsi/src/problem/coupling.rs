// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Value, json};

use implexity_core::CaeResult;
use implexity_solve::multirate_coupling::{CouplingMode, FieldNewtonOptions, LinearSolveOptions};
use implexity_solve::newton_krylov::ExactKrylovPolicy;

use crate::json::{Section, refuse};

pub const COUPLING_KEYS: [&str; 17] = [
    "mode",
    "substeps",
    "predictor_order",
    "tolerance",
    "max_iterations",
    "reuse_steps",
    "initial_relaxation",
    "krylov",
    "pushforward",
    "schur_ratio_limit",
    "work_defect_limit",
    "work_defect_action",
    "preflight_modes",
    "field_newton",
    "linear_solves",
    "step_cache_bytes",
    "energy_scale_j",
];

pub const MODES: [&str; 4] = ["loose", "strong_quasi_newton", "strong_newton_krylov", "explicit_synchronous"];

#[derive(Clone, Debug)]
pub struct CouplingSpec {
    pub mode: CouplingMode,
    pub substeps: usize,
    pub kernel_width_cells: usize,
    pub points_per_axis: usize,
    pub saturation_width: f64,
    pub blocking_scale: f64,
    pub points_per_cell_axis: Option<f64>,
    pub void_threshold: f64,
    pub schur_ratio_limit: f64,
    pub work_defect_limit: f64,
    pub work_defect_action_record: bool,
    pub preflight_modes: usize,
    pub field_newton: FieldNewtonOptions,
    pub linear_solves: LinearSolveOptions,
    pub step_cache_bytes: u64,
    pub energy_scale_j: Option<f64>,
}

fn unit_interval(x: f64) -> bool {
    x > 0.0 && x < 1.0
}


#[allow(clippy::too_many_lines)]
pub fn parse(v: &Value) -> CaeResult<(CouplingSpec, Value)> {
    let mut s = Section::new(v, "coupling", &COUPLING_KEYS)?;
    let mode_name = s.choice_or("mode", "strong_newton_krylov", &MODES)?;
    let only_for = |s: &Section<'_>, keys: &[&str], mode: &str| -> CaeResult<()> {
        if let Some(k) = keys.iter().find(|k| s.has(k)) {
            return refuse(format!("coupling.{k} does not apply to mode {mode}"));
        }
        Ok(())
    };
    let mode = match mode_name.as_str() {
        "loose" => {
            only_for(
                &s,
                &["tolerance", "max_iterations", "reuse_steps", "initial_relaxation", "krylov"],
                "loose",
            )?;
            let p = s.integer_or("predictor_order", 1, 0..=3)?;
            #[allow(clippy::cast_possible_truncation)]
            CouplingMode::Loose { predictor_order: p as u8 }
        }
        "strong_quasi_newton" => {
            only_for(&s, &["predictor_order", "krylov"], "strong_quasi_newton")?;
            CouplingMode::StrongQuasiNewton {

                tolerance: s.number_or("tolerance", 1e-8, unit_interval, "in (0, 1)")?,
                max_iterations: s.integer_or("max_iterations", 50, 1..=1000)?,
                reuse_steps: s.integer_or("reuse_steps", 0, 0..=64)?,
                initial_relaxation: s.number_or(
                    "initial_relaxation",
                    0.5,
                    |x| x > 0.0 && x <= 1.0,
                    "in (0, 1]",
                )?,
            }
        }
        "strong_newton_krylov" => {
            only_for(&s, &["predictor_order", "reuse_steps", "initial_relaxation"], "strong_newton_krylov")?;
            let tolerance = s.number_or("tolerance", 1e-10, unit_interval, "in (0, 1)")?;
            let max_iterations = s.integer_or("max_iterations", 30, 1..=500)?;
            let mut k = s.child_or_empty("krylov", &["rtol", "restart", "maxiter"])?;
            let krylov = ExactKrylovPolicy {
                enabled: true,
                rtol: k.number_or("rtol", 1e-4, unit_interval, "in (0, 1)")?,
                restart: k.integer_or("restart", 30, 1..=500)?,
                maxiter: k.integer_or("maxiter", 200, 1..=10_000)?,
                ..ExactKrylovPolicy::default()
            };
            s.put("krylov", k.finish());
            CouplingMode::StrongNewtonKrylov { tolerance, max_iterations, krylov }
        }
        _ => {
            return refuse(
                "coupling.mode explicit_synchronous is not available: the soft solid provides implicit schemes only \
                 (avf_midpoint, generalized_alpha, newmark, quasistatic) and no explicit central-difference solid \
                 substep inside the fluid substeps exists (recorded as deferred scope in docs/HANDOFF.md); use loose \
                 coupling for the cheapest explicit-in-time interface",
            );
        }
    };
    let substeps = s.integer_or("substeps", 10, 1..=100_000)?;
    let mut p = s.child_or_empty(
        "pushforward",
        &[
            "kernel",
            "width_cells",
            "points_per_axis",
            "points_per_cell_axis",
            "saturation_width",
            "blocking_scale",
            "void_threshold",
        ],
    )?;
    p.choice_or("kernel", "cubic_bspline", &["cubic_bspline"])?;
    let kernel_width_cells = p.integer_or("width_cells", 1, 1..=4)?;
    let points_per_axis = p.integer_or("points_per_axis", 1, 1..=4)?;

    let points_per_cell_axis = if p.has("points_per_cell_axis") {
        if p.has("points_per_axis") {
            return refuse("coupling.pushforward: give points_per_axis or points_per_cell_axis, not both");
        }
        Some(p.number("points_per_cell_axis", |x| (0.25..=8.0).contains(&x), "in [0.25, 8]")?)
    } else {
        None
    };
    let saturation_width = p.number_or("saturation_width", 0.05, |x| x > 0.0 && x < 0.5, "in (0, 0.5)")?;
    let blocking_scale = p.number_or("blocking_scale", 1.1, |x| (1.0..=4.0).contains(&x), "in [1, 4]")?;

    let void_threshold = p.number_or("void_threshold", 0.0, |x| (0.0..1.0).contains(&x), "in [0, 1)")?;
    s.put("pushforward", p.finish());
    let schur_ratio_limit = s.number_or("schur_ratio_limit", 0.5, |x| x > 0.0, "positive")?;
    let work_defect_limit = s.number_or("work_defect_limit", 0.01, |x| x >= 0.0, "nonnegative")?;
    let work_defect_action_record =
        s.choice_or("work_defect_action", "refuse", &["refuse", "record"])? == "record";
    let preflight_modes = s.integer_or("preflight_modes", 2, 1..=16)?;
    let mut f = s.child_or_empty("field_newton", &["relative_tolerance", "max_iterations"])?;
    let field_newton = FieldNewtonOptions {
        relative_tolerance: f.number_or("relative_tolerance", 1e-12, unit_interval, "in (0, 1)")?,
        max_iterations: f.integer_or("max_iterations", 30, 1..=500)?,
    };
    s.put("field_newton", f.finish());
    let mut l = s.child_or_empty("linear_solves", &["relative_tolerance", "restart", "max_iterations"])?;
    let linear_solves = LinearSolveOptions {
        relative_tolerance: l.number_or("relative_tolerance", 1e-11, unit_interval, "in (0, 1)")?,
        restart: l.integer_or("restart", 40, 1..=500)?,
        max_iterations: l.integer_or("max_iterations", 400, 1..=10_000)?,
    };
    s.put("linear_solves", l.finish());
    let cache = s.number_or(
        "step_cache_bytes",
        268_435_456.0,
        |x| x >= 0.0 && x.fract() == 0.0,
        "a nonnegative integer",
    )?;
    let energy_scale_j = if s.raw("energy_scale_j").is_some_and(|v| !v.is_null()) {
        Some(s.number("energy_scale_j", |x| x > 0.0, "positive")?)
    } else {
        s.put("energy_scale_j", Value::Null);
        None
    };
    s.put("mode", json!(mode_name));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let step_cache_bytes = cache as u64;
    let spec = CouplingSpec {
        mode,
        substeps,
        kernel_width_cells,
        points_per_axis,
        saturation_width,
        blocking_scale,
        points_per_cell_axis,
        void_threshold,
        schur_ratio_limit,
        work_defect_limit,
        work_defect_action_record,
        preflight_modes,
        field_newton,
        linear_solves,
        step_cache_bytes,
        energy_scale_j,
    };
    Ok((spec, s.finish()))
}
