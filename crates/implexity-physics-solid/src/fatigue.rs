// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Map, Value, json};

use implexity_core::CaeError;

use crate::util::{contract, has_exact_keys, obj, real_array, text};

pub const SCHEMA: &str = "implexity-calibrated-cycle-fatigue/1";
pub const COMPONENT_ID: &str = "calibrated_cycle_fatigue";
pub const IMPLEMENTATION: &str = "implexity.physics_library.fatigue_observer.CalibratedCycleFatigue";
pub const COMPONENT_KIND: &str = "fatigue_history_observer";
pub const EDITOR_LABEL: &str = "Calibrated-cycle fatigue usage";
pub const LIMITATIONS: [&str; 4] = [
    "Explicit complete, closed, single-reversal uniaxial sampled cycles only; not rainflow.",
    "User supplied S-N calibration; no inferred endurance limit or stress-ratio correction.",
    "Usage is not stiffness degradation, rupture certification or creep-fatigue interaction.",
    "Unsampled stress extrema cannot be detected; temporal resolution remains a user validation obligation.",
];
const REQUIRED: [&str; 13] = [
    "schema",
    "provenance",
    "normal_axis",
    "stress_ratio",
    "stress_ratio_tolerance",
    "cycle_tolerance_Pa",
    "uniaxial_tolerance_Pa",
    "T_min_K",
    "T_max_K",
    "initial_usage",
    "amplitudes_Pa",
    "cycles_to_failure",
    "cycles",
];

#[must_use]
pub fn runtime_support() -> Map<String, Value> {
    obj(json!({"status": "postsolve_history_observer", "history": true, "differentiable_objective": false,
        "limitations": LIMITATIONS}))
}

#[must_use]
pub fn authoring_contract() -> Map<String, Value> {
    obj(json!({"schema": SCHEMA, "required_settings": REQUIRED,
        "cycle_keys": ["start_index", "end_index", "repetitions"],
        "units": {"amplitudes_Pa": "Pa, amplitude (half range), not range", "cycles_to_failure": "complete cycles, not reversals", "initial_usage": "1", "normal_axis": "0=xx,1=yy,2=zz", "T_min_K": "K", "T_max_K": "K"},
        "interpolation": "log amplitude/log cycles; no extrapolation",
        "mean_stress": "fixed calibrated sigma_min/sigma_max; actual ratio checked, no inferred correction"}))
}

#[must_use]
pub fn editor_schema(settings: &Value, _context: &Value) -> Value {
    if !settings.is_object() {
        return json!({});
    }
    let number = |title: &str, unit: &str, bounds: Value| {
        let mut row = json!({"title": title, "units": unit, "type": "number"});
        for (k, v) in bounds.as_object().cloned().unwrap_or_default() {
            row[k] = v;
        }
        row
    };
    json!({"description": "Postprocessing of explicit closed uniaxial cycles; not rainflow, stiffness degradation or an optimization objective.",
        "properties": {
            "schema": {"title": "Fatigue declaration version", "enum": [SCHEMA]},
            "provenance": {"title": "S–N calibration provenance"},
            "normal_axis": {"title": "Uniaxial normal stress axis (0: xx, 1: yy, 2: zz)", "type": "integer", "enum": [0, 1, 2]},
            "stress_ratio": number("Calibrated stress ratio (minimum / maximum)", "1", json!({"exclusiveMaximum": 1})),
            "stress_ratio_tolerance": number("Stress-ratio tolerance", "1", json!({"minimum": 0})),
            "cycle_tolerance_Pa": number("Cycle closure and reversal tolerance", "Pa", json!({"minimum": 0})),
            "uniaxial_tolerance_Pa": number("Transverse and physical shear stress tolerance", "Pa", json!({"minimum": 0})),
            "T_min_K": number("Minimum calibrated temperature", "K", json!({"exclusiveMinimum": 0})),
            "T_max_K": number("Maximum calibrated temperature", "K", json!({"exclusiveMinimum": 0})),
            "initial_usage": number("Initial accumulated usage", "1", json!({"minimum": 0})),
            "amplitudes_Pa": {"title": "S–N stress amplitudes (half-ranges)", "units": "Pa", "type": "array", "minItems": 2, "items": {"type": "number", "exclusiveMinimum": 0}, "description": "Strictly increasing; paired with cycles to failure. No extrapolation."},
            "cycles_to_failure": {"title": "S–N complete cycles to failure", "units": "cycles", "type": "array", "minItems": 2, "items": {"type": "number", "exclusiveMinimum": 0}, "description": "Positive, nonincreasing lives, one per amplitude; cycles, not reversals."},
            "cycles": {"title": "Explicit sampled cycles", "type": "array", "minItems": 1, "description": "Ordered, nonoverlapping closed cycles with one reversal. Repetitions repeat that sampled exposure; they are not time-step counts.",
                "items": {"type": "object", "properties": {
                    "start_index": {"title": "First history sample (zero-based)", "type": "integer", "minimum": 0},
                    "end_index": {"title": "Last history sample (inclusive)", "type": "integer", "minimum": 2},
                    "repetitions": {"title": "Number of complete repetitions", "type": "integer", "minimum": 1}}}}}})
}


pub fn real(value: &Value) -> Result<(Vec<usize>, Vec<f64>), CaeError> {
    match real_array(value) {
        Some((s, v)) if v.iter().all(|x| x.is_finite()) => Ok((s, v)),
        _ => contract("fatigue inputs must contain finite real numbers, not text or booleans"),
    }
}

fn int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) if n.is_i64() || n.is_u64() => n.as_i64(),
        _ => None,
    }
}

fn scalar(s: &Value, key: &str) -> f64 {
    s[key].as_f64().unwrap_or(f64::NAN)
}


pub fn validate(s: &Value) -> Result<Value, CaeError> {
    if !has_exact_keys(s, &REQUIRED) || s["schema"] != json!(SCHEMA) {
        return contract(format!("fatigue settings require exactly {}", crate::util::sorted_repr(REQUIRED)));
    }
    if !text(&s["provenance"]) {
        return contract("fatigue calibration provenance required");
    }
    if !int(&s["normal_axis"]).is_some_and(|a| (0..=2).contains(&a)) {
        return contract("fatigue normal_axis must be 0,1,2");
    }
    for k in [
        "stress_ratio",
        "stress_ratio_tolerance",
        "cycle_tolerance_Pa",
        "uniaxial_tolerance_Pa",
        "T_min_K",
        "T_max_K",
        "initial_usage",
    ] {
        if !real(&s[k])?.0.is_empty() {
            return contract("fatigue settings require finite scalars");
        }
    }
    let min_tol = ["stress_ratio_tolerance", "cycle_tolerance_Pa", "uniaxial_tolerance_Pa", "initial_usage"]
        .iter()
        .map(|k| scalar(s, k))
        .fold(f64::INFINITY, f64::min);
    let (lo, hi) = (scalar(s, "T_min_K"), scalar(s, "T_max_K"));
    if scalar(s, "stress_ratio") >= 1.0 || min_tol < 0.0 || !(0.0 < lo && lo < hi) {
        return contract("invalid fatigue ratio, tolerance, usage or temperature interval");
    }
    let (sa, a) = real(&s["amplitudes_Pa"])?;
    let (sn, n) = real(&s["cycles_to_failure"])?;
    if sa.len() != 1
        || a.len() < 2
        || sn != sa
        || a.iter().chain(&n).any(|v| *v <= 0.0)
        || a.windows(2).any(|w| w[1] - w[0] <= 0.0)
        || n.windows(2).any(|w| w[1] - w[0] > 0.0)
    {
        return contract("S-N data require increasing positive amplitudes and nonincreasing positive cycles");
    }
    let Some(cycles) = s["cycles"].as_array().filter(|c| !c.is_empty()) else {
        return contract("explicit complete cycle intervals required");
    };
    let mut last = -1;
    for c in cycles {
        let keys = ["end_index", "repetitions", "start_index"];
        if !has_exact_keys(c, &keys) || keys.iter().any(|k| int(&c[*k]).is_none()) {
            return contract("cycle indices/repetitions must be explicit integers");
        }
        let (lo, hi, rep) = (
            int(&c["start_index"]).unwrap_or(0),
            int(&c["end_index"]).unwrap_or(0),
            int(&c["repetitions"]).unwrap_or(0),
        );
        if lo < 0 || hi - lo < 2 || rep < 1 || lo < last {
            return contract("cycle intervals must be ordered, nonoverlapping and include a full reversal");
        }
        last = hi;
    }
    Ok(s.clone())
}


pub fn validate_fatigue_row(row: &Value) -> Result<Option<Value>, CaeError> {
    if row.is_null() {
        return Ok(None);
    }
    if !has_exact_keys(row, &["component", "settings"]) || row["component"] != json!(COMPONENT_ID) {
        return contract("fatigue_observer requires component=calibrated_cycle_fatigue and settings");
    }
    Ok(Some(json!({"component": COMPONENT_ID, "settings": validate(&row["settings"])?})))
}

#[must_use]
pub fn interp(x: f64, xp: &[f64], fp: &[f64]) -> f64 {
    let n = xp.len();
    if x.is_nan() {
        return f64::NAN;
    }
    if x <= xp[0] {
        return fp[0];
    }
    if x >= xp[n - 1] {
        return fp[n - 1];
    }
    let j = xp.partition_point(|v| *v <= x) - 1;
    let slope = (fp[j + 1] - fp[j]) / (xp[j + 1] - xp[j]);

    #[allow(clippy::float_cmp)]
    if x == xp[j] {
        return fp[j];
    }
    slope * (x - xp[j]) + fp[j]
}


#[allow(clippy::too_many_lines)]
pub fn evaluate(
    s: &Value,
    stress: &[Vec<[f64; 6]>],
    temperature: &[Vec<f64>],
    times: &[f64],
) -> Result<Value, CaeError> {
    validate(s)?;
    let nt = stress.len();
    let np = stress.first().map_or(0, Vec::len);
    let finite = stress
        .iter()
        .flatten()
        .flatten()
        .chain(temperature.iter().flatten())
        .chain(times)
        .all(|v| v.is_finite());
    if stress.iter().any(|r| r.len() != np)
        || temperature.len() != nt
        || temperature.iter().any(|r| r.len() != np)
        || times.len() != nt
        || !finite
        || times.windows(2).any(|w| w[1] - w[0] <= 0.0)
    {
        return contract(
            "fatigue requires finite (time,point,6) Mandel stress, matching temperature and increasing times",
        );
    }
    let (lo_t, hi_t) = (scalar(s, "T_min_K"), scalar(s, "T_max_K"));
    if np == 0 || temperature.iter().flatten().any(|t| *t < lo_t || *t > hi_t) {
        return contract("fatigue temperature outside calibration interval or empty sample set");
    }
    let axis = usize::try_from(int(&s["normal_axis"]).unwrap_or(0)).unwrap_or(0);
    let tol_u = scalar(s, "uniaxial_tolerance_Pa");
    for row in stress.iter().flatten() {
        let mut other: Vec<f64> = (0..6).filter(|k| *k != axis).map(|k| row[k]).collect();
        let len = other.len();
        for v in &mut other[len - 3..] {
            *v /= std::f64::consts::SQRT_2;
        }
        if other.iter().any(|v| v.abs() > tol_u) {
            return contract(
                "fatigue calibration is uniaxial; transverse/shear stresses exceed authored tolerance",
            );
        }
    }
    let mut usage = vec![scalar(s, "initial_usage"); np];
    let (_, amps) = real(&s["amplitudes_Pa"])?;
    let (_, lives) = real(&s["cycles_to_failure"])?;
    let log_a: Vec<f64> = amps.iter().map(|v| v.log10()).collect();
    let log_n: Vec<f64> = lives.iter().map(|v| v.log10()).collect();
    let tolerance = scalar(s, "cycle_tolerance_Pa");
    let mut rows = Vec::new();
    for c in s["cycles"].as_array().into_iter().flatten() {
        let lo = usize::try_from(int(&c["start_index"]).unwrap_or(0)).unwrap_or(0);
        let hi = usize::try_from(int(&c["end_index"]).unwrap_or(0)).unwrap_or(0);
        let reps = int(&c["repetitions"]).unwrap_or(0);
        if hi >= nt {
            return contract("fatigue cycle index exceeds solved history");
        }
        let path = |t: usize, p: usize| stress[t][p][axis];
        if (0..np).any(|p| (path(hi, p) - path(lo, p)).abs() > tolerance) {
            return contract("fatigue cycle is not closed");
        }
        let maximum: Vec<f64> =
            (0..np).map(|p| (lo..=hi).map(|t| path(t, p)).fold(f64::NEG_INFINITY, f64::max)).collect();
        let minimum: Vec<f64> =
            (0..np).map(|p| (lo..=hi).map(|t| path(t, p)).fold(f64::INFINITY, f64::min)).collect();
        let amplitude: Vec<f64> = (0..np).map(|p| 0.5 * maximum[p] - 0.5 * minimum[p]).collect();
        let active: Vec<usize> = (0..np).filter(|p| amplitude[*p] > 0.0).collect();
        for &p in &active {
            let signs: Vec<f64> = (lo..hi)
                .map(|t| 0.5 * path(t + 1, p) - 0.5 * path(t, p))
                .filter(|inc| inc.abs() > 0.5 * tolerance)
                .map(f64::signum)
                .collect();
            #[allow(clippy::float_cmp)]
            let changes = signs.windows(2).filter(|w| w[1] != w[0]).count();
            let endpoint = path(lo, p);
            if changes != 1 || (endpoint - maximum[p]).abs().min((endpoint - minimum[p]).abs()) > tolerance {
                return contract(
                    "authored cycle must contain exactly one sampled reversal between matching extrema",
                );
            }
        }
        if active.iter().any(|p| maximum[*p] <= 0.0) {
            return contract("fixed-R fatigue requires positive maximum tensile stress");
        }
        let (ratio, ratio_tol) = (scalar(s, "stress_ratio"), scalar(s, "stress_ratio_tolerance"));
        if active.iter().any(|p| (minimum[*p] / maximum[*p] - ratio).abs() > ratio_tol) {
            return contract("solved cycle stress ratio differs from S-N calibration");
        }
        let (a0, a1) = (amps[0], amps[amps.len() - 1]);
        if active.iter().any(|p| amplitude[*p] < a0 || amplitude[*p] > a1) {
            return contract(
                "fatigue amplitude outside S-N data; no extrapolation or assumed endurance limit",
            );
        }
        let mut increment = vec![0.0; np];
        for &p in &active {
            let life = 10f64.powf(interp(amplitude[p].log10(), &log_a, &log_n));
            let inc = reps as f64 / life;
            if !inc.is_finite() || life == 0.0 {
                return contract("fatigue usage exceeds the supported finite numerical range");
            }
            increment[p] = inc;
        }
        for (u, i) in usage.iter_mut().zip(&increment) {
            *u += i;
        }
        if usage.iter().chain(&increment).any(|v| !v.is_finite()) {
            return contract("fatigue usage exceeds the supported finite numerical range");
        }
        rows.push(json!({"start_index": lo, "end_index": hi, "repetitions": reps,
            "amplitude_Pa": amplitude, "usage_increment": increment}));
    }
    let max_usage = usage.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Ok(json!({"component": COMPONENT_ID, "qualification": false,
        "damage_model": "Palmgren-Miner usage, not stiffness damage",
        "usage_per_point": usage, "maximum_usage": max_usage,
        "unity_threshold_reached": usage.iter().any(|u| *u >= 1.0),
        "cycles": rows, "response_units": {"usage_per_point": "1", "maximum_usage": "1"},
        "limitations": LIMITATIONS}))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RainflowCycle {
    pub amplitude: f64,
    pub mean: f64,
    pub count: f64,
    pub start_index: usize,
    pub end_index: usize,
}


pub fn rainflow_cycles(values: &[f64]) -> Result<Vec<RainflowCycle>, CaeError> {
    if values.len() < 2 || values.iter().any(|v| !v.is_finite()) {
        return contract("At least two scalar stress samples required");
    }
    let mut distinct = vec![(0usize, values[0])];
    for (i, v) in values.iter().enumerate().skip(1) {
        #[allow(clippy::float_cmp)]
        if *v != distinct[distinct.len() - 1].1 {
            distinct.push((i, *v));
        }
    }
    let mut turning = vec![distinct[0]];
    for j in 1..distinct.len().saturating_sub(1) {
        if (distinct[j].1 > distinct[j - 1].1) != (distinct[j + 1].1 > distinct[j].1) {
            turning.push(distinct[j]);
        }
    }
    if distinct.len() > 1 {
        turning.push(distinct[distinct.len() - 1]);
    }
    let mut stack: Vec<(usize, f64)> = Vec::new();
    let mut cycles = Vec::new();
    let record = |a: (usize, f64), b: (usize, f64), count: f64, cycles: &mut Vec<RainflowCycle>| {
        cycles.push(RainflowCycle {
            amplitude: (b.1 / 2.0 - a.1 / 2.0).abs(),
            mean: a.1 / 2.0 + b.1 / 2.0,
            count,
            start_index: a.0,
            end_index: b.0,
        });
    };
    for point in turning {
        stack.push(point);
        while stack.len() >= 3 {
            let k = stack.len();
            let older = (stack[k - 2].1 / 2.0 - stack[k - 3].1 / 2.0).abs();
            let newer = (stack[k - 1].1 / 2.0 - stack[k - 2].1 / 2.0).abs();
            if newer < older {
                break;
            }
            if k == 3 {
                record(stack[0], stack[1], 0.5, &mut cycles);
                stack.remove(0);
            } else {
                record(stack[k - 3], stack[k - 2], 1.0, &mut cycles);
                let last = stack.pop();
                stack.pop();
                stack.pop();
                stack.extend(last);
            }
        }
    }
    for w in stack.windows(2) {
        record(w[0], w[1], 0.5, &mut cycles);
    }
    Ok(cycles)
}


pub fn validate_rainflow_settings(settings: &Value) -> Result<Value, CaeError> {
    let required = [
        "T_max_K",
        "T_min_K",
        "amplitudes_Pa",
        "cycles_to_failure",
        "initial_usage",
        "mean_stresses_Pa",
        "provenance",
        "solid_temperature_K",
        "stress_measure",
    ];
    let keys_ok = settings.as_object().is_some_and(|m| {
        let keys: Vec<&str> = m.keys().map(String::as_str).filter(|k| *k != "temperature_source").collect();
        keys.len() == required.len() && required.iter().all(|k| keys.contains(k))
    });
    if !keys_ok {
        return contract("Complete rainflow calibration surface and solid temperature required");
    }
    let s = settings.clone();
    let source = s.get("temperature_source").cloned().unwrap_or_else(|| json!("prescribed"));
    if source != json!("prescribed") && source != json!("solved_solid") {
        return contract("Fatigue temperature_source must be prescribed or solved_solid");
    }
    if !text(&s["provenance"]) {
        return contract("Calibration provenance for chosen stress measure required");
    }
    let measures = ["normal_x", "normal_y", "normal_z", "signed_von_mises_hydrostatic"];
    if !s["stress_measure"].as_str().is_some_and(|m| measures.contains(&m)) {
        return contract("Explicit calibrated scalar stress measure required");
    }
    let (sa, a) = real(&s["amplitudes_Pa"])?;
    let (sm, m) = real(&s["mean_stresses_Pa"])?;
    let (sl, life) = real(&s["cycles_to_failure"])?;
    let bad = sa.len() != 1
        || sm.len() != 1
        || a.len() < 2
        || m.len() < 2
        || sl != [m.len(), a.len()]
        || a.iter().any(|v| *v <= 0.0)
        || a.windows(2).any(|w| w[1] - w[0] <= 0.0)
        || m.windows(2).any(|w| w[1] - w[0] <= 0.0)
        || life.iter().any(|v| *v <= 0.0)
        || life.chunks(a.len()).any(|r| r.windows(2).any(|w| w[1] - w[0] > 0.0));
    if bad {
        return contract(
            "Increasing amplitudes/means and positive mean-by-amplitude life table decreasing with amplitude required",
        );
    }
    for key in ["solid_temperature_K", "T_min_K", "T_max_K", "initial_usage"] {
        if !real(&s[key])?.0.is_empty() {
            return contract("Scalar fatigue temperature and usage required");
        }
    }
    let (lo, hi, t, u) = (
        scalar(&s, "T_min_K"),
        scalar(&s, "T_max_K"),
        scalar(&s, "solid_temperature_K"),
        scalar(&s, "initial_usage"),
    );
    if !(0.0 < lo && lo < hi) || !(lo <= t && t <= hi) || u < 0.0 {
        return contract("Fatigue temperature/usage outside calibration");
    }
    Ok(s)
}


pub fn evaluate_rainflow(
    settings: &Value,
    stress: &[Vec<[f64; 6]>],
    times: &[f64],
    solid_temperature: Option<&[Vec<f64>]>,
) -> Result<Value, CaeError> {
    let s = validate_rainflow_settings(settings)?;
    let nt = stress.len();
    let ne = stress.first().map_or(0, Vec::len);
    if stress.iter().any(|r| r.len() != ne)
        || ne == 0
        || nt < 2
        || times.len() != nt
        || times.windows(2).any(|w| w[1] - w[0] <= 0.0)
        || stress.iter().flatten().flatten().chain(times).any(|v| !v.is_finite())
    {
        return contract("Time-by-element physical stress [xx,yy,zz,xy,yz,xz] and increasing times required");
    }
    let source = s.get("temperature_source").and_then(Value::as_str).unwrap_or("prescribed").to_string();
    let (lo, hi) = (scalar(&s, "T_min_K"), scalar(&s, "T_max_K"));
    let temperature: Vec<f64> = if source == "solved_solid" {
        let Some(t) = solid_temperature else {
            return contract("Solved-solid fatigue requires mapped element temperature history");
        };
        if t.len() != nt
            || t.iter().any(|r| r.len() != ne)
            || t.iter().flatten().any(|v| !v.is_finite() || *v < lo || *v > hi)
        {
            return contract(
                "Fatigue element temperatures must match stress endpoints and remain inside calibration",
            );
        }
        t.iter().flatten().copied().collect()
    } else {
        if solid_temperature.is_some() {
            return contract("Explicitly select solved_solid to consume solved fatigue temperatures");
        }
        vec![scalar(&s, "solid_temperature_K"); nt * ne]
    };
    let measure = s["stress_measure"].as_str().unwrap_or_default().to_string();
    let scalar_of = |v: &[f64; 6]| -> f64 {
        if let Some(axis) = measure.strip_prefix("normal_") {
            v[match axis {
                "x" => 0,
                "y" => 1,
                _ => 2,
            }]
        } else {
            let hydro = (v[0] + v[1] + v[2]) / 3.0;
            let d = [v[0] - hydro, v[1] - hydro, v[2] - hydro];
            let dev2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
            let shear2 = v[3] * v[3] + v[4] * v[4] + v[5] * v[5];
            (if hydro < 0.0 { -1.0 } else { 1.0 }) * (1.5 * (dev2 + 2.0 * shear2)).sqrt()
        }
    };
    let values: Vec<Vec<f64>> = stress.iter().map(|r| r.iter().map(&scalar_of).collect()).collect();
    if values.iter().flatten().any(|v| !v.is_finite()) {
        return contract("Equivalent stress exceeds finite numerical range");
    }
    let (_, a) = real(&s["amplitudes_Pa"])?;
    let (_, m) = real(&s["mean_stresses_Pa"])?;
    let (_, life) = real(&s["cycles_to_failure"])?;
    let loglife: Vec<f64> = life.iter().map(|v| v.ln()).collect();
    let log_a: Vec<f64> = a.iter().map(|v| v.ln()).collect();
    let mut usage = vec![scalar(&s, "initial_usage"); ne];
    let mut rows = Vec::new();
    for element in 0..ne {
        let series: Vec<f64> = values.iter().map(|r| r[element]).collect();
        let mut out = Vec::new();
        for c in rainflow_cycles(&series)? {
            if !(a[0] <= c.amplitude && c.amplitude <= a[a.len() - 1])
                || !(m[0] <= c.mean && c.mean <= m[m.len() - 1])
            {
                return contract(
                    "Counted stress amplitude/mean outside calibrated S-N surface; no extrapolation",
                );
            }
            let at_mean: Vec<f64> =
                loglife.chunks(a.len()).map(|r| interp(c.amplitude.ln(), &log_a, r)).collect();
            let cycles_to_failure = interp(c.mean, &m, &at_mean).exp();
            let increment = c.count / cycles_to_failure;
            usage[element] += increment;
            out.push(json!({"amplitude_Pa": c.amplitude, "mean_Pa": c.mean, "count": c.count,
                "start_index": c.start_index, "end_index": c.end_index,
                "cycles_to_failure": cycles_to_failure, "usage_increment": increment}));
        }
        rows.push(Value::Array(out));
    }
    if usage.iter().any(|u| !u.is_finite()) {
        return contract("Nonfinite fatigue usage");
    }
    let (tmin, tmax) =
        temperature.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(x, y), v| (x.min(*v), y.max(*v)));
    Ok(
        json!({"usage_per_element": usage, "maximum_usage": usage.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        "unity_threshold_reached": usage.iter().any(|u| *u >= 1.0),
        "cycles_by_element": rows, "stress_measure": measure,
        "solid_temperature_K": if source == "prescribed" { s["solid_temperature_K"].clone() } else { Value::Null },
        "temperature_source": source,
        "temperature_range_K": [tmin, tmax],
        "qualification": false, "provenance": s["provenance"],
        "limitations": ["Open sampled history, residual half cycles, no inferred block repetitions or endurance limit.",
            "Scalar stress measure needs matching material/multiaxial calibration; not a critical-plane method.",
            "Palmgren-Miner usage only: no stiffness degradation, crack growth or creep-fatigue interaction.",
            "The S-N surface must be calibrated for the entire reported solid-temperature interval; no temperature-dependent life law is inferred.",
            "Nondifferentiable cycle extraction; not an optimization objective. Refine CFD and structural time resolution."]}),
    )
}
