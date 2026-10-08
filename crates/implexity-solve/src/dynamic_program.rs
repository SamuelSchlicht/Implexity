// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::contracts::{RESPONSE_SENSES, ResponseSpec};
use implexity_core::json::{canonical_sha256, parse_strict};
use implexity_core::numeric_contract::real_scalar;
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;
use implexity_optim::response_program::{ProgramRow, ResponseProgram, normalise_rows};
use serde_json::{Map, Value, json};

use crate::time_functional::{self, FunctionalValue, HarmonicPart, SeriesFunctional, TimeWindow};

pub const SCHEMA: &str = "implexity-dynamic-response-program/1";

pub const SCHEMA_IDS: [&str; 1] = [SCHEMA];

pub const SERIES_KINDS: [&str; 14] = [
    "mean",
    "mean_square",
    "rms",
    "variance",
    "smooth_max",
    "smooth_min",
    "smooth_peak_to_peak",
    "harmonic",
    "harmonic_fit",
    "band_power",
    "crossing_period",
    "duty_fraction",
    "rate",
    "waveform_mismatch",
];

pub const COMPOSITE_KINDS: [&str; 3] = ["phase_lag", "normalised_difference", "target_match"];

pub const WAVEFORM_BETA: f64 = 50.0;

pub const PERIOD_KINDS: [&str; 2] = ["period", "frequency"];

pub const WINDOW_KINDS: [&str; 7] = ["periodic", "uniform", "trapezoid", "hann", "bump", "tukey", "custom"];

pub const MAX_NAME_LENGTH: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub enum TermQuantity {
    Series(SeriesFunctional),
    Period,
    Frequency,
    PhaseLag {
        sample: usize,
        reference: usize,
        order: usize,
        centre_rad: f64,
    },
    NormalisedDifference {
        a: SeriesFunctional,
        b: SeriesFunctional,
    },
    TargetMatch {
        entries: Vec<TargetEntry>,
    },
    Design {
        kind: String,
        spec: Value,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct TargetEntry {
    pub label: String,
    pub quantity: TermQuantity,
    pub window: Option<TimeWindow>,
    pub band: (f64, f64),
    pub tolerance: f64,
    pub weight: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DynamicTerm {
    pub name: String,
    pub quantity: TermQuantity,
    pub window: Option<TimeWindow>,
    pub response: Option<ResponseSpec>,
}

#[derive(Clone, Debug, PartialEq)]
struct Binding {
    columns: Vec<usize>,
    width: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DynamicProgram {
    window: TimeWindow,
    terms: Vec<DynamicTerm>,
    samples: Vec<String>,
    binding: Option<Binding>,
}

fn object<'a>(value: &'a Value, what: &str) -> CaeResult<&'a Map<String, Value>> {
    value.as_object().ok_or_else(|| CaeError::contract(format!("{what} must be a JSON object")))
}

fn refuse_unknown(map: &Map<String, Value>, allowed: &[&str], what: &str) -> CaeResult<()> {
    let mut unknown: Vec<&str> = map.keys().map(String::as_str).filter(|k| !allowed.contains(k)).collect();
    if unknown.is_empty() {
        return Ok(());
    }
    unknown.sort_unstable();
    Err(CaeError::contract(format!(
        "{what} has unknown keys {} (allowed: {})",
        unknown.join(", "),
        allowed.join(", ")
    )))
}

fn index_field(map: &Map<String, Value>, key: &str, what: &str) -> CaeResult<Option<usize>> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::Number(n)) if n.is_u64() => n
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .map(Some)
            .ok_or_else(|| CaeError::contract(format!("{what} {key} is out of range"))),
        Some(_) => Err(CaeError::contract(format!("{what} {key} must be a non-negative integer"))),
    }
}

fn required_index(map: &Map<String, Value>, key: &str, what: &str) -> CaeResult<usize> {
    index_field(map, key, what)?.ok_or_else(|| CaeError::contract(format!("{what} requires {key}")))
}

fn required_real(map: &Map<String, Value>, key: &str, what: &str) -> CaeResult<f64> {
    let value = map.get(key).ok_or_else(|| CaeError::contract(format!("{what} requires {key}")))?;
    real_scalar(value, &format!("{what} {key}"))
}

fn kind_of<'a>(map: &'a Map<String, Value>, what: &str) -> CaeResult<&'a str> {
    match map.get("kind") {
        Some(Value::String(s)) if !s.is_empty() => Ok(s),
        _ => Err(CaeError::contract(format!("{what} requires a nonempty text kind"))),
    }
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && text.len() <= MAX_NAME_LENGTH
}


pub fn window_from_value(value: &Value) -> CaeResult<TimeWindow> {
    let what = "time window";
    let map = object(value, what)?;
    let kind = kind_of(map, what)?;
    let what = format!("{kind} time window");
    let range = |map: &Map<String, Value>| -> CaeResult<(usize, usize)> {
        Ok((required_index(map, "from", &what)?, required_index(map, "to", &what)?))
    };
    let window = match kind {
        "periodic" => {
            refuse_unknown(map, &["kind"], &what)?;
            TimeWindow::Periodic
        }
        "uniform" | "trapezoid" | "hann" | "bump" => {
            refuse_unknown(map, &["kind", "from", "to"], &what)?;
            let (from, to) = range(map)?;
            match kind {
                "uniform" => TimeWindow::Uniform { from, to },
                "trapezoid" => TimeWindow::Trapezoid { from, to },
                "hann" => TimeWindow::Hann { from, to },
                _ => TimeWindow::Bump { from, to },
            }
        }
        "tukey" => {
            refuse_unknown(map, &["kind", "from", "to", "alpha"], &what)?;
            let (from, to) = range(map)?;
            TimeWindow::Tukey { from, to, alpha: required_real(map, "alpha", &what)? }
        }
        "custom" => {
            refuse_unknown(map, &["kind", "weights"], &what)?;
            let Some(Value::Array(rows)) = map.get("weights") else {
                return Err(CaeError::contract("custom time window requires a weights list"));
            };
            let weights = rows
                .iter()
                .map(|w| real_scalar(w, "custom time window weight"))
                .collect::<CaeResult<Vec<f64>>>()?;
            TimeWindow::Custom(weights)
        }
        other => {
            return Err(CaeError::contract(format!(
                "unknown time window kind {other:?} (known: {})",
                WINDOW_KINDS.join(", ")
            )));
        }
    };
    window.validate()?;
    Ok(window)
}

#[must_use]
pub fn window_to_value(window: &TimeWindow) -> Value {
    match window {
        TimeWindow::Periodic => json!({"kind": "periodic"}),
        TimeWindow::Tukey { from, to, alpha } => {
            json!({"kind": "tukey", "from": from, "to": to, "alpha": alpha})
        }
        TimeWindow::Custom(weights) => json!({"kind": "custom", "weights": weights}),
        other => {
            let (from, to) = other.range().unwrap_or((0, 0));
            json!({"kind": other.kind(), "from": from, "to": to})
        }
    }
}

fn sample_slot(map: &Map<String, Value>, what: &str, samples: &mut Vec<String>) -> CaeResult<usize> {
    let name = match map.get("sample") {
        Some(Value::String(s)) if !s.is_empty() && s.trim() == s => s,
        Some(_) => return Err(CaeError::contract(format!("{what} sample must be a nonempty sample name"))),
        None => return Err(CaeError::contract(format!("{what} requires sample"))),
    };
    if let Some(i) = samples.iter().position(|s| s == name) {
        return Ok(i);
    }
    samples.push(name.clone());
    Ok(samples.len() - 1)
}

fn waveform_from_map(map: &Map<String, Value>, sample: usize, what: &str) -> CaeResult<SeriesFunctional> {
    let Some(Value::Array(rows)) = map.get("reference") else {
        return Err(CaeError::contract(
            "waveform_mismatch functional requires reference, a list of values over one period",
        ));
    };
    let reference = rows
        .iter()
        .map(|r| real_scalar(r, "waveform_mismatch reference value"))
        .collect::<CaeResult<Vec<f64>>>()?;
    let period_rows = match map.get("period_rows") {
        None | Some(Value::Null) => None,
        Some(_) => index_field(map, "period_rows", what)?,
    };
    let normalise = match map.get("normalise") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(CaeError::contract("waveform_mismatch normalise must be a Boolean")),
    };
    let beta = match map.get("beta") {
        None => WAVEFORM_BETA,
        Some(_) => required_real(map, "beta", what)?,
    };
    Ok(SeriesFunctional::WaveformMismatch { sample, reference, period_rows, normalise, beta })
}

fn series_from_map(
    map: &Map<String, Value>,
    kind: &str,
    samples: &mut Vec<String>,
) -> CaeResult<SeriesFunctional> {
    let what = format!("{kind} functional");
    let keys: &[&str] = match kind {
        "mean" | "mean_square" | "rms" | "variance" => &["kind", "sample"],
        "smooth_max" | "smooth_min" | "smooth_peak_to_peak" => &["kind", "sample", "beta"],
        "harmonic" | "harmonic_fit" => &["kind", "sample", "order", "part"],
        "band_power" => &["kind", "sample", "lo", "hi"],
        "crossing_period" => &["kind", "sample", "level"],
        "duty_fraction" => &["kind", "sample", "level", "width"],
        "waveform_mismatch" => &["kind", "sample", "reference", "period_rows", "normalise", "beta"],
        _ => &["kind", "inner", "order"],
    };
    refuse_unknown(map, keys, &what)?;
    let functional = if kind == "rate" {
        let order = index_field(map, "order", &what)?.unwrap_or(1);
        if order == 0 {
            return Err(CaeError::contract("rate order must be at least 1"));
        }
        let inner_value =
            map.get("inner").ok_or_else(|| CaeError::contract("rate functional requires inner"))?;
        let inner_map = object(inner_value, "rate inner functional")?;
        let inner_kind = kind_of(inner_map, "rate inner functional")?;
        if !SERIES_KINDS.contains(&inner_kind) {
            return Err(CaeError::contract(format!(
                "rate inner functional must be a series functional ({}), not {inner_kind:?}",
                SERIES_KINDS.join(", ")
            )));
        }
        let mut functional = series_from_map(inner_map, inner_kind, samples)?;
        for _ in 0..order {
            functional = SeriesFunctional::Rate { inner: Box::new(functional) };
        }
        functional
    } else {
        let sample = sample_slot(map, &what, samples)?;
        match kind {
            "mean" => SeriesFunctional::Mean { sample },
            "mean_square" => SeriesFunctional::MeanSquare { sample },
            "rms" => SeriesFunctional::Rms { sample },
            "variance" => SeriesFunctional::Variance { sample },
            "smooth_max" => SeriesFunctional::SmoothMax { sample, beta: required_real(map, "beta", &what)? },
            "smooth_min" => SeriesFunctional::SmoothMin { sample, beta: required_real(map, "beta", &what)? },
            "smooth_peak_to_peak" => {
                SeriesFunctional::SmoothPeakToPeak { sample, beta: required_real(map, "beta", &what)? }
            }
            "harmonic" | "harmonic_fit" => {
                let part = match map.get("part") {
                    None => HarmonicPart::Amplitude,
                    Some(Value::String(p)) if p == "amplitude" => HarmonicPart::Amplitude,
                    Some(Value::String(p)) if p == "real" => HarmonicPart::Real,
                    Some(Value::String(p)) if p == "imag" => HarmonicPart::Imag,
                    Some(_) => {
                        return Err(CaeError::contract(
                            "harmonic part must be amplitude, real or imag (the phase is not differentiable at a \
                             vanishing coefficient)",
                        ));
                    }
                };
                let order = required_index(map, "order", &what)?;
                if kind == "harmonic_fit" { SeriesFunctional::HarmonicFit { sample, order, part } } else { SeriesFunctional::Harmonic { sample, order, part } }
            }
            "band_power" => SeriesFunctional::BandPower {
                sample,
                lo: required_index(map, "lo", &what)?,
                hi: required_index(map, "hi", &what)?,
            },
            "crossing_period" => {
                SeriesFunctional::CrossingPeriod { sample, level: required_real(map, "level", &what)? }
            }
            "waveform_mismatch" => waveform_from_map(map, sample, &what)?,
            _ => SeriesFunctional::DutyFraction {
                sample,
                level: required_real(map, "level", &what)?,
                width: required_real(map, "width", &what)?,
            },
        }
    };
    functional.validate()?;
    Ok(functional)
}

fn quantity_from_value(value: &Value, samples: &mut Vec<String>) -> CaeResult<TermQuantity> {
    let map = object(value, "response functional")?;
    let kind = kind_of(map, "response functional")?;
    if SERIES_KINDS.contains(&kind) {
        return Ok(TermQuantity::Series(series_from_map(map, kind, samples)?));
    }
    match kind {
        "period" | "frequency" => {
            refuse_unknown(map, &["kind"], &format!("{kind} functional"))?;
            Ok(if kind == "period" { TermQuantity::Period } else { TermQuantity::Frequency })
        }
        "phase_lag" => {
            let what = "phase_lag functional";
            refuse_unknown(map, &["kind", "sample", "reference_sample", "order", "centre_rad"], what)?;
            let sample = sample_slot(map, what, samples)?;
            let reference_name = match map.get("reference_sample") {
                Some(Value::String(r)) if !r.is_empty() && r.trim() == r => r.clone(),
                _ => {
                    return Err(CaeError::contract(
                        "phase_lag functional requires reference_sample, a sample name",
                    ));
                }
            };
            let mut reference_map = Map::new();
            reference_map.insert("sample".into(), Value::String(reference_name));
            let reference = sample_slot(&reference_map, what, samples)?;
            if reference == sample {
                return Err(CaeError::contract("phase_lag sample and reference_sample must differ"));
            }
            let order = index_field(map, "order", what)?.unwrap_or(1);
            if order == 0 {
                return Err(CaeError::contract("phase_lag order must be at least 1"));
            }
            let centre_rad = match map.get("centre_rad") {
                None => 0.0,
                Some(_) => required_real(map, "centre_rad", what)?,
            };
            if centre_rad.abs() > 2.0 * std::f64::consts::PI {
                return Err(CaeError::contract("phase_lag centre_rad must lie in [-2 pi, 2 pi]"));
            }
            Ok(TermQuantity::PhaseLag { sample, reference, order, centre_rad })
        }
        "normalised_difference" => {
            let what = "normalised_difference functional";
            refuse_unknown(map, &["kind", "a", "b"], what)?;
            let side = |key: &str, samples: &mut Vec<String>| -> CaeResult<SeriesFunctional> {
                let v = map.get(key).ok_or_else(|| {
                    CaeError::contract(format!("{what} requires {key}, a series functional"))
                })?;
                let m = object(v, &format!("{what} {key}"))?;
                let k = kind_of(m, &format!("{what} {key}"))?;
                if !SERIES_KINDS.contains(&k) {
                    return Err(CaeError::contract(format!(
                        "{what} {key} must be a series functional ({}), not {k:?}",
                        SERIES_KINDS.join(", ")
                    )));
                }
                series_from_map(m, k, samples)
            };
            let a = side("a", samples)?;
            let b = side("b", samples)?;
            Ok(TermQuantity::NormalisedDifference { a, b })
        }
        "target_match" => target_match_from_map(map, samples),
        other if is_identifier(other) => {
            Ok(TermQuantity::Design { kind: other.to_string(), spec: value.clone() })
        }
        other => Err(CaeError::contract(format!(
            "response functional kind {other:?} is neither a kernel functional ({}, {}, {}) nor a design-term identifier",
            SERIES_KINDS.join(", "),
            PERIOD_KINDS.join(", "),
            COMPOSITE_KINDS.join(", ")
        ))),
    }
}

fn band_of(map: &Map<String, Value>, what: &str) -> CaeResult<(f64, f64)> {
    match (map.get("target"), map.get("band")) {
        (Some(_), Some(_)) => {
            Err(CaeError::contract(format!("{what}: give either target or band, not both")))
        }
        (Some(_), None) => {
            let t = required_real(map, "target", what)?;
            Ok((t, t))
        }
        (None, Some(Value::Array(b))) if b.len() == 2 => {
            let lo = real_scalar(&b[0], &format!("{what} band lower bound"))?;
            let hi = real_scalar(&b[1], &format!("{what} band upper bound"))?;
            if lo <= hi {
                Ok((lo, hi))
            } else {
                Err(CaeError::contract(format!("{what} band must satisfy lower <= upper")))
            }
        }
        (None, Some(_)) => Err(CaeError::contract(format!("{what} band must be [lower, upper]"))),
        (None, None) => Err(CaeError::contract(format!("{what} requires target or band"))),
    }
}

fn target_match_from_map(map: &Map<String, Value>, samples: &mut Vec<String>) -> CaeResult<TermQuantity> {
    refuse_unknown(map, &["kind", "targets"], "target_match functional")?;
    let Some(Value::Array(rows)) = map.get("targets") else {
        return Err(CaeError::contract("target_match functional requires a targets list"));
    };
    if rows.is_empty() {
        return Err(CaeError::contract("target_match functional requires at least one target"));
    }
    let mut entries: Vec<TargetEntry> = Vec::with_capacity(rows.len());
    for (k, row) in rows.iter().enumerate() {
        let what = format!("target_match target {k}");
        let m = object(row, &what)?;
        refuse_unknown(
            m,
            &["label", "functional", "window", "target", "band", "tolerance", "weight"],
            &what,
        )?;
        let label = match m.get("label") {
            None => format!("target{k}"),
            Some(Value::String(l)) if is_identifier(l) => l.clone(),
            Some(_) => return Err(CaeError::contract(format!("{what} label must be an identifier"))),
        };
        if entries.iter().any(|e| e.label == label) {
            return Err(CaeError::contract(format!("target_match label {label} is repeated")));
        }
        let what = format!("target_match target {label}");
        let fv =
            m.get("functional").ok_or_else(|| CaeError::contract(format!("{what} requires a functional")))?;
        let quantity = quantity_from_value(fv, samples).map_err(|e| e.context(&what))?;
        if matches!(quantity, TermQuantity::Design { .. } | TermQuantity::TargetMatch { .. }) {
            return Err(CaeError::contract(format!(
                "{what}: a target quantity must be a kernel functional (no design term, no nested target_match)"
            )));
        }
        let window = match m.get("window") {
            None | Some(Value::Null) => None,
            Some(w) => {
                if matches!(quantity, TermQuantity::Period | TermQuantity::Frequency) {
                    return Err(CaeError::contract(format!("{what}: period and frequency take no window")));
                }
                Some(window_from_value(w).map_err(|e| e.context(&what))?)
            }
        };
        let band = band_of(m, &what)?;
        let tolerance = required_real(m, "tolerance", &what)?;
        if tolerance <= 0.0 {
            return Err(CaeError::contract(format!("{what} tolerance must be positive")));
        }
        let weight = match m.get("weight") {
            None => 1.0,
            Some(_) => required_real(m, "weight", &what)?,
        };
        if weight < 0.0 {
            return Err(CaeError::contract(format!("{what} weight must be non-negative")));
        }
        entries.push(TargetEntry { label, quantity, window, band, tolerance, weight });
    }
    if entries.iter().map(|e| e.weight).sum::<f64>() <= 0.0 {
        return Err(CaeError::contract("target_match weights must have a positive sum"));
    }
    Ok(TermQuantity::TargetMatch { entries })
}

fn quantity_to_value(quantity: &TermQuantity, names: &[String]) -> CaeResult<Option<Value>> {
    let name = |i: usize| {
        names.get(i).cloned().ok_or_else(|| CaeError::contract(format!("sample column {i} has no name")))
    };
    Ok(Some(match quantity {
        TermQuantity::Series(f) => functional_to_value(f, names)?,
        TermQuantity::Period => json!({"kind": "period"}),
        TermQuantity::Frequency => json!({"kind": "frequency"}),
        TermQuantity::PhaseLag { sample, reference, order, centre_rad } => json!({"kind": "phase_lag",
            "sample": name(*sample)?, "reference_sample": name(*reference)?, "order": order, "centre_rad": centre_rad}),
        TermQuantity::NormalisedDifference { a, b } => json!({"kind": "normalised_difference",
            "a": functional_to_value(a, names)?, "b": functional_to_value(b, names)?}),
        TermQuantity::TargetMatch { entries } => {
            let rows = entries
                .iter()
                .map(|e| {
                    let mut row = Map::new();
                    row.insert("label".into(), Value::String(e.label.clone()));
                    row.insert(
                        "functional".into(),
                        quantity_to_value(&e.quantity, names)?.unwrap_or(Value::Null),
                    );
                    if let Some(w) = &e.window {
                        row.insert("window".into(), window_to_value(w));
                    }
                    row.insert("band".into(), json!([e.band.0, e.band.1]));
                    row.insert("tolerance".into(), json!(e.tolerance));
                    row.insert("weight".into(), json!(e.weight));
                    Ok(Value::Object(row))
                })
                .collect::<CaeResult<Vec<Value>>>()?;
            json!({"kind": "target_match", "targets": rows})
        }
        TermQuantity::Design { .. } => return Ok(None),
    }))
}


pub fn functional_to_value(functional: &SeriesFunctional, names: &[String]) -> CaeResult<Value> {
    let name = |i: usize| {
        names.get(i).cloned().ok_or_else(|| CaeError::contract(format!("sample column {i} has no name")))
    };
    Ok(match functional {
        SeriesFunctional::Rate { .. } => {
            let mut order = 0usize;
            let mut inner = functional;
            while let SeriesFunctional::Rate { inner: next } = inner {
                order += 1;
                inner = next;
            }
            json!({"kind": "rate", "order": order, "inner": functional_to_value(inner, names)?})
        }
        SeriesFunctional::Mean { sample }
        | SeriesFunctional::MeanSquare { sample }
        | SeriesFunctional::Rms { sample }
        | SeriesFunctional::Variance { sample } => {
            json!({"kind": functional.kind(), "sample": name(*sample)?})
        }
        SeriesFunctional::SmoothMax { sample, beta }
        | SeriesFunctional::SmoothMin { sample, beta }
        | SeriesFunctional::SmoothPeakToPeak { sample, beta } => {
            json!({"kind": functional.kind(), "sample": name(*sample)?, "beta": beta})
        }
        SeriesFunctional::Harmonic { sample, order, part } | SeriesFunctional::HarmonicFit { sample, order, part } => {
            json!({"kind": functional.kind(), "sample": name(*sample)?, "order": order, "part": part.name()})
        }
        SeriesFunctional::BandPower { sample, lo, hi } => {
            json!({"kind": "band_power", "sample": name(*sample)?, "lo": lo, "hi": hi})
        }
        SeriesFunctional::CrossingPeriod { sample, level } => {
            json!({"kind": "crossing_period", "sample": name(*sample)?, "level": level})
        }
        SeriesFunctional::DutyFraction { sample, level, width } => {
            json!({"kind": "duty_fraction", "sample": name(*sample)?, "level": level, "width": width})
        }
        SeriesFunctional::WaveformMismatch { sample, reference, period_rows, normalise, beta } => {
            json!({"kind": "waveform_mismatch", "sample": name(*sample)?, "reference": reference,
                   "period_rows": period_rows, "normalise": normalise, "beta": beta})
        }
    })
}

fn response_from_value(value: &Value, name: &str) -> CaeResult<ResponseSpec> {
    let what = format!("dynamic response {name} response row");
    let map = object(value, &what)?;
    refuse_unknown(map, &["sense", "target", "bound", "weight", "scale"], &what)?;
    let mut row = map.clone();
    row.insert("name".into(), Value::String(name.to_string()));
    ResponseSpec::from_dict(&Value::Object(row))
}

fn response_to_value(spec: &ResponseSpec) -> Value {
    let mut row = Map::new();
    row.insert("sense".into(), Value::String(spec.sense.clone()));
    if let Some(target) = spec.target {
        row.insert("target".into(), target.to_value());
    }
    row.insert("weight".into(), spec.weight.to_value());
    row.insert("scale".into(), spec.scale.to_value());
    Value::Object(row)
}


pub fn normalise(value: &Value) -> CaeResult<DynamicProgram> {
    let map = object(value, "dynamic response program")?;
    refuse_unknown(map, &["schema", "window", "terms"], "dynamic response program")?;
    match map.get("schema") {
        Some(Value::String(s)) if s == SCHEMA => {}
        Some(other) => {
            return Err(CaeError::contract(format!(
                "dynamic response program schema must be {SCHEMA}, not {other}"
            )));
        }
        None => return Err(CaeError::contract(format!("dynamic response program requires schema {SCHEMA}"))),
    }
    let window = window_from_value(
        map.get("window").ok_or_else(|| CaeError::contract("dynamic response program requires a window"))?,
    )?;
    let Some(Value::Array(rows)) = map.get("terms") else {
        return Err(CaeError::contract("dynamic response program requires a terms list"));
    };
    if rows.is_empty() {
        return Err(CaeError::contract("dynamic response program requires at least one term"));
    }
    let mut samples = Vec::new();
    let mut terms: Vec<DynamicTerm> = Vec::with_capacity(rows.len());
    for (position, row) in rows.iter().enumerate() {
        let row_map = object(row, &format!("dynamic response term {position}"))?;
        let name = match row_map.get("name") {
            Some(Value::String(s)) if is_identifier(s) => s.clone(),
            _ => {
                return Err(CaeError::contract(format!(
                    "dynamic response term {position} name must be an identifier ([A-Za-z_][A-Za-z0-9_]*, at most \
                     {MAX_NAME_LENGTH} characters)"
                )));
            }
        };
        let what = format!("dynamic response {name}");
        refuse_unknown(row_map, &["name", "functional", "window", "response"], &what)?;
        if terms.iter().any(|t| t.name == name) {
            return Err(CaeError::contract(format!("dynamic response name {name} is repeated")));
        }
        let quantity = quantity_from_value(
            row_map
                .get("functional")
                .ok_or_else(|| CaeError::contract(format!("{what} requires a functional")))?,
            &mut samples,
        )
        .map_err(|e| e.context(&what))?;
        let term_window = match row_map.get("window") {
            None => None,
            Some(w) => {
                if !matches!(
                    quantity,
                    TermQuantity::Series(_)
                        | TermQuantity::PhaseLag { .. }
                        | TermQuantity::NormalisedDifference { .. }
                        | TermQuantity::TargetMatch { .. }
                ) {
                    return Err(CaeError::contract(format!(
                        "{what}: a window override belongs only to a series or composite functional"
                    )));
                }
                Some(window_from_value(w).map_err(|e| e.context(&what))?)
            }
        };
        let response = row_map.get("response").map(|r| response_from_value(r, &name)).transpose()?;
        terms.push(DynamicTerm { name, quantity, window: term_window, response });
    }
    Ok(DynamicProgram { window, terms, samples, binding: None })
}


pub fn normalise_text(text: &str) -> CaeResult<DynamicProgram> {
    let value = parse_strict(text).map_err(|e| {
        CaeError::contract(format!(
            "dynamic response program is not strict JSON: {} (line {}, column {})",
            e.message, e.line, e.column
        ))
    })?;
    normalise(&value)
}

impl DynamicProgram {
    #[must_use]
    pub fn responses(&self) -> Vec<String> {
        self.terms.iter().map(|t| t.name.clone()).collect()
    }

    #[must_use]
    pub fn terms(&self) -> &[DynamicTerm] {
        &self.terms
    }

    #[must_use]
    pub fn window(&self) -> &TimeWindow {
        &self.window
    }

    #[must_use]
    pub fn sample_names(&self) -> &[String] {
        &self.samples
    }

    #[must_use]
    pub fn design_terms(&self) -> Vec<&DynamicTerm> {
        self.terms.iter().filter(|t| matches!(t.quantity, TermQuantity::Design { .. })).collect()
    }

    #[must_use]
    pub fn needs_period(&self) -> bool {
        fn needs(q: &TermQuantity) -> bool {
            match q {
                TermQuantity::Period | TermQuantity::Frequency => true,
                TermQuantity::TargetMatch { entries } => entries.iter().any(|e| needs(&e.quantity)),
                _ => false,
            }
        }
        self.terms.iter().any(|t| needs(&t.quantity))
    }

    #[must_use]
    pub fn is_bound(&self) -> bool {
        self.binding.is_some()
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        let terms: Vec<Value> = self
            .terms
            .iter()
            .map(|t| {
                let mut row = Map::new();
                row.insert("name".into(), Value::String(t.name.clone()));
                let functional = match &t.quantity {
                    TermQuantity::Design { spec, .. } => spec.clone(),

                    other => quantity_to_value(other, &self.samples).ok().flatten().unwrap_or(Value::Null),
                };
                row.insert("functional".into(), functional);
                if let Some(w) = &t.window {
                    row.insert("window".into(), window_to_value(w));
                }
                if let Some(r) = &t.response {
                    row.insert("response".into(), response_to_value(r));
                }
                Value::Object(row)
            })
            .collect();
        json!({"schema": SCHEMA, "window": window_to_value(&self.window), "terms": terms})
    }

    #[must_use]
    pub fn identity(&self) -> String {
        canonical_sha256(&self.to_value())
    }


    pub fn bind(&self, sample_names: &[String], design_kinds: &[&str]) -> CaeResult<Self> {
        let mut columns = Vec::with_capacity(self.samples.len());
        for name in &self.samples {
            let found: Vec<usize> =
                sample_names.iter().enumerate().filter(|(_, s)| *s == name).map(|(i, _)| i).collect();
            match found.as_slice() {
                [column] => columns.push(*column),
                [] => {
                    return Err(CaeError::contract(format!(
                        "dynamic response program reads unknown sample {name:?} (available: {})",
                        sample_names.join(", ")
                    )));
                }
                _ => {
                    return Err(CaeError::contract(format!(
                        "dynamic response program sample {name:?} is ambiguous: the stepper declares it {} times",
                        found.len()
                    )));
                }
            }
        }
        for term in &self.terms {
            if let TermQuantity::Design { kind, .. } = &term.quantity
                && !design_kinds.contains(&kind.as_str())
            {
                return Err(CaeError::contract(format!(
                    "dynamic response {}: unknown functional kind {kind:?} (kernel functionals: {}, {}, {}; provider \
                     design terms: {})",
                    term.name,
                    SERIES_KINDS.join(", "),
                    PERIOD_KINDS.join(", "),
                    COMPOSITE_KINDS.join(", "),
                    if design_kinds.is_empty() { "none".to_string() } else { design_kinds.join(", ") }
                )));
            }
        }
        let mut bound = self.clone();
        bound.binding = Some(Binding { columns, width: sample_names.len() });
        Ok(bound)
    }


    pub fn admit(&self, rows: usize, periodic: bool, autonomous: bool) -> CaeResult<()> {
        for term in &self.terms {
            let what = format!("dynamic response {}", term.name);
            admit_quantity(
                &term.quantity,
                term.window.as_ref().unwrap_or(&self.window),
                rows,
                periodic,
                autonomous,
            )
            .map_err(|e| e.context(&what))?;
        }
        Ok(())
    }


    pub fn evaluate(
        &self,
        samples: &DenseMatrix,
        step_s: f64,
        periodic: bool,
        autonomous: bool,
    ) -> CaeResult<Vec<(String, FunctionalValue)>> {
        let binding = self.binding.as_ref().ok_or_else(|| {
            CaeError::contract(
                "dynamic response program must be bound to the stepper's sample names before evaluation",
            )
        })?;
        if samples.ncols != binding.width {
            return Err(CaeError::contract(format!(
                "sample matrix has {} columns but the program is bound to {} sample names",
                samples.ncols, binding.width
            )));
        }
        if !(step_s.is_finite() && step_s > 0.0) {
            return Err(CaeError::contract("time functional step size must be finite and positive"));
        }
        let mut out = Vec::with_capacity(self.terms.len());
        for term in &self.terms {
            let what = format!("dynamic response {}", term.name);
            if matches!(term.quantity, TermQuantity::Design { .. }) {
                continue;
            }
            let context = Evaluation { columns: &binding.columns, samples, step_s, periodic, autonomous };
            let result = context
                .quantity(&term.quantity, term.window.as_ref().unwrap_or(&self.window))
                .map_err(|e| e.context(&what))?;
            out.push((term.name.clone(), result));
        }
        Ok(out)
    }


    pub fn response_program(&self) -> CaeResult<ResponseProgram> {
        let mut objectives = Vec::new();
        let mut constraints = Vec::new();
        for spec in self.terms.iter().filter_map(|t| t.response.clone()) {
            if spec.is_bounded() {
                constraints.push(ProgramRow::Spec(spec));
            } else {
                objectives.push(ProgramRow::Spec(spec));
            }
        }
        normalise_rows(&objectives, &constraints)
    }
}

fn admit_quantity(
    quantity: &TermQuantity,
    window: &TimeWindow,
    rows: usize,
    periodic: bool,
    autonomous: bool,
) -> CaeResult<()> {
    match quantity {
        TermQuantity::Series(_)
        | TermQuantity::PhaseLag { .. }
        | TermQuantity::NormalisedDifference { .. } => window.support(rows, periodic, autonomous).map(|_| ()),
        TermQuantity::Period | TermQuantity::Frequency => period_admission(periodic),
        TermQuantity::TargetMatch { entries } => {
            for e in entries {
                admit_quantity(&e.quantity, e.window.as_ref().unwrap_or(window), rows, periodic, autonomous)
                    .map_err(|err| err.context(&format!("target {}", e.label)))?;
            }
            Ok(())
        }
        TermQuantity::Design { .. } => Ok(()),
    }
}

fn linear_combination(
    parts: &[(f64, &FunctionalValue)],
    value: f64,
    rows: usize,
    cols: usize,
) -> FunctionalValue {
    let mut d_samples = DenseMatrix::zeros(rows, cols);
    let mut d_period_s = 0.0;
    for (c, v) in parts {
        if *c == 0.0 {
            continue;
        }
        for (o, x) in d_samples.data.iter_mut().zip(&v.d_samples.data) {
            *o += c * x;
        }
        d_period_s += c * v.d_period_s;
    }
    FunctionalValue { value, d_samples, d_period_s }
}

struct Evaluation<'a> {
    columns: &'a [usize],
    samples: &'a DenseMatrix,
    step_s: f64,
    periodic: bool,
    autonomous: bool,
}

impl Evaluation<'_> {
    fn series(&self, f: &SeriesFunctional, window: &TimeWindow) -> CaeResult<FunctionalValue> {
        let bound = f.with_sample(self.columns[f.sample()]);
        time_functional::evaluate(&bound, window, self.samples, self.step_s, self.periodic, self.autonomous)
    }

    fn quantity(&self, quantity: &TermQuantity, window: &TimeWindow) -> CaeResult<FunctionalValue> {
        let (rows, cols) = (self.samples.nrows, self.samples.ncols);
        match quantity {
            TermQuantity::Series(f) => self.series(f, window),
            TermQuantity::Period | TermQuantity::Frequency => {
                period_admission(self.periodic)?;
                let period = rows as f64 * self.step_s;
                let (value, derivative) = if matches!(quantity, TermQuantity::Period) {
                    (period, 1.0)
                } else {
                    (1.0 / period, -1.0 / (period * period))
                };
                Ok(FunctionalValue {
                    value,
                    d_samples: DenseMatrix::zeros(rows, cols),
                    d_period_s: if self.autonomous { derivative } else { 0.0 },
                })
            }
            TermQuantity::PhaseLag { sample, reference, order, centre_rad } => {
                let part = |column: usize, part: HarmonicPart| {
                    self.series(&SeriesFunctional::Harmonic { sample: column, order: *order, part }, window)
                };

                part(*sample, HarmonicPart::Amplitude)?;
                part(*reference, HarmonicPart::Amplitude)?;
                let (ra, ia) = (part(*sample, HarmonicPart::Real)?, part(*sample, HarmonicPart::Imag)?);
                let (rb, ib) = (part(*reference, HarmonicPart::Real)?, part(*reference, HarmonicPart::Imag)?);
                let (xa, ya, xb, yb) = (ra.value, ia.value, rb.value, ib.value);
                let x = xa * xb + ya * yb;
                let y = ya * xb - xa * yb;
                let modulus2 = x * x + y * y;
                let scale = xa.hypot(ya) * xb.hypot(yb);
                if modulus2.sqrt() <= 1e-12 * scale {
                    return Err(CaeError::convergence(format!(
                        "phase_lag: harmonic {order} of a sample column vanishes; the phase is not differentiable there"
                    )));
                }
                let (c, s) = (centre_rad.cos(), centre_rad.sin());
                let value = centre_rad + (y * c - x * s).atan2(x * c + y * s);
                let q = 1.0 / modulus2;
                let d_ra = q * (x * (-yb) - y * xb);
                let d_ia = q * (x * xb - y * yb);
                let d_rb = q * (x * ya - y * xa);
                let d_ib = q * (x * (-xa) - y * ya);
                Ok(linear_combination(
                    &[(d_ra, &ra), (d_ia, &ia), (d_rb, &rb), (d_ib, &ib)],
                    value,
                    rows,
                    cols,
                ))
            }
            TermQuantity::NormalisedDifference { a, b } => {
                let (va, vb) = (self.series(a, window)?, self.series(b, window)?);
                let sum = va.value + vb.value;
                if sum.abs() <= 1e-12 * (va.value.abs() + vb.value.abs()) || sum == 0.0 {
                    return Err(CaeError::convergence(
                        "normalised_difference: A + B vanishes; the index is not defined there",
                    ));
                }
                let value = (va.value - vb.value) / sum;
                let (da, db) = (2.0 * vb.value / (sum * sum), -2.0 * va.value / (sum * sum));
                Ok(linear_combination(&[(da, &va), (db, &vb)], value, rows, cols))
            }
            TermQuantity::TargetMatch { entries } => {
                let total: f64 = entries.iter().map(|e| e.weight).sum();
                let mut value = 0.0;
                let mut parts = Vec::with_capacity(entries.len());
                for e in entries {
                    let v = self
                        .quantity(&e.quantity, e.window.as_ref().unwrap_or(window))
                        .map_err(|err| err.context(&format!("target {}", e.label)))?;
                    let (lo, hi) = e.band;
                    let excess = if v.value < lo {
                        (v.value - lo) / e.tolerance
                    } else if v.value > hi {
                        (v.value - hi) / e.tolerance
                    } else {
                        0.0
                    };
                    value += e.weight * excess * excess / total;
                    parts.push((2.0 * e.weight * excess / (e.tolerance * total), v));
                }
                let refs: Vec<(f64, &FunctionalValue)> = parts.iter().map(|(c, v)| (*c, v)).collect();
                Ok(linear_combination(&refs, value, rows, cols))
            }
            TermQuantity::Design { kind, .. } => {
                Err(CaeError::contract(format!("design term {kind} is evaluated by the provider")))
            }
        }
    }
}

fn period_admission(periodic: bool) -> CaeResult<()> {
    if periodic {
        Ok(())
    } else {
        Err(CaeError::contract(
            "period and frequency terms need the samples of one period of a periodic orbit; on a fixed horizon use \
             crossing_period",
        ))
    }
}

fn window_schema(title: &str, description: &str, default: Option<Value>) -> Value {
    let mut schema = json!({
        "title": title,
        "type": "object",
        "description": description,
        "properties": {
            "kind": {"title": "Window", "type": "string", "enum": WINDOW_KINDS,
                "description": "periodic: uniform weights over exactly one period of a periodic orbit (spectrally accurate). uniform, trapezoid: fixed-horizon averages over the rows from..to. hann, bump (C-infinity), tukey: smooth windows whose ends vanish; required for autonomous oscillations run to the limit cycle (windowing theorem: uniform and trapezoid windows are refused there). custom: authored weights, one per sample row, normalised by their sum."},
            "from": {"title": "First sample row", "type": "integer", "minimum": 0, "unit": "steps",
                "description": "Range windows (uniform, trapezoid, hann, bump, tukey): first sample row included, e.g. the first row after the spin-up."},
            "to": {"title": "End sample row", "type": "integer", "minimum": 1, "unit": "steps",
                "description": "Range windows: the row after the last included one (exclusive); at most the number of macro steps."},
            "alpha": {"title": "Taper fraction", "type": "number", "exclusiveMinimum": 0, "maximum": 1, "unit": "1",
                "description": "tukey only: fraction of the window covered by the two cosine tapers (1 is the Hann window)."},
            "weights": {"title": "Custom weights", "type": "array", "items": {"type": "number", "minimum": 0},
                "description": "custom only: one non-negative weight per sample row; normalised by their sum."}
        },
        "required": ["kind"],
        "additionalProperties": false
    });
    if let (Some(d), Some(m)) = (default, schema.as_object_mut()) {
        m.insert("default".into(), d);
    }
    schema
}

#[allow(clippy::too_many_lines)]
fn functional_schema(sample_names: &[String], design_kinds: Option<&[&str]>) -> Value {
    let design_text = match design_kinds {
        None => "Any other identifier is a design term the provider declares and evaluates (for example design_volume_fraction).".to_string(),
        Some([]) => "The provider declares no design terms.".to_string(),
        Some(kinds) => format!("Provider design terms: {}.", kinds.join(", ")),
    };
    let mut sample = json!({"title": "Sample", "type": "string", "minLength": 1,
        "description": "Name of the sample series the functional reads (every kind except period, frequency, rate and design terms)."});
    if !sample_names.is_empty()
        && let Some(m) = sample.as_object_mut()
    {
        m.insert("enum".into(), json!(sample_names));
    }
    let targets = json!({"title": "Targets", "type": "array", "minItems": 1,
                "description": "target_match only: rows {label, functional (any kernel functional except design terms and target_match), window (optional), target or band [lower, upper], tolerance (> 0, the excess unit), weight (>= 0, default 1)}.",
                "items": {"type": "object", "properties": {
                    "label": {"title": "Label", "type": "string", "description": "Identifier of the matched quantity (default target<k>)."},
                    "functional": {"title": "Quantity", "type": "object", "description": "A kernel functional object of this same form (not a design term, not target_match)."},
                    "window": {"title": "Window override", "type": "object", "description": "Optional window of this quantity."},
                    "target": {"title": "Target", "type": "number", "description": "Target value (the band [target, target])."},
                    "band": {"title": "Band", "type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2, "description": "Admissible band [lower, upper] (no penalty inside)."},
                    "tolerance": {"title": "Tolerance", "type": "number", "exclusiveMinimum": 0, "description": "Unit of the excess outside the band (the quantity's unit)."},
                    "weight": {"title": "Weight", "type": "number", "minimum": 0, "default": 1.0, "description": "Relative weight of the quantity."}},
                    "required": ["functional", "tolerance"]}});
    let mut schema = json!({
        "title": "Functional",
        "type": "object",
        "description": "How the sample series is reduced to one scalar response; every functional has an exact gradient with respect to the samples (and to the period of an autonomous orbit).",
        "properties": {
            "kind": {"title": "Functional", "type": "string", "minLength": 1,
                "description": format!("mean, mean_square, rms, variance: weighted moments. smooth_max, smooth_min, smooth_peak_to_peak: log-sum-exp with sharpness beta. harmonic: 2 Re, 2 Im or amplitude 2|c_k| of c_k = sum w_i s_i exp(-2 pi i k t_i / T_w) over whole periods (the phase is not offered: it is not differentiable where c_k vanishes). harmonic_fit: weighted least-squares fit of a constant plus sine and cosine at the authored harmonic order; real is the cosine coefficient, imag is the negative sine coefficient, amplitude is their magnitude. Refuses a rank-deficient basis and zero amplitude; does not measure an autonomous frequency. band_power: sum of |c_k|^2 for lo <= k <= hi. crossing_period: mean spacing of interpolated upward crossings of level (s). duty_fraction: weighted C2 smooth Heaviside of the sample above level with half width width. rate: the inner functional of the time derivative of its sample. waveform_mismatch: phase-aligned squared distance to a reference waveform of one period (softmin-weighted mean over circular shifts; normalise compares standardised shapes). period, frequency: the period T (s) or 1/T (Hz) of a periodic orbit (exact gradient when the period is an unknown, a constant on a forced orbit). phase_lag: phase (rad) of harmonic order of sample relative to reference_sample, in (centre_rad - pi, centre_rad + pi]. normalised_difference: (A - B)/(A + B) of the series functionals a and b (symmetry indices). target_match: weighted mean of the squared excess (in tolerances) of several quantities outside their target bands. {design_text}")},
            "sample": sample,
            "beta": {"title": "Sharpness", "type": "number", "exclusiveMinimum": 0, "unit": "1/sample unit",
                "description": "smooth_max, smooth_min, smooth_peak_to_peak: log-sum-exp sharpness; larger is closer to the true extremum."},
            "order": {"title": "Order", "type": "integer", "minimum": 1,
                "description": "harmonic and harmonic_fit: authored harmonic order k, cycles per window span (the fundamental of a one-period orbit is 1). rate: number of time derivatives (default 1)."},
            "part": {"title": "Harmonic part", "type": "string", "enum": ["amplitude", "real", "imag"],
                "description": "harmonic: amplitude 2|c_k|, real 2 Re c_k, imag 2 Im c_k. harmonic_fit: amplitude is coefficient magnitude, real is cosine coefficient, imag is negative sine coefficient (default amplitude)."},
            "lo": {"title": "Lowest harmonic", "type": "integer", "minimum": 0,
                "description": "band_power only: lowest harmonic order included."},
            "hi": {"title": "Highest harmonic", "type": "integer", "minimum": 0,
                "description": "band_power only: highest harmonic order included (below half the rows of the window span)."},
            "level": {"title": "Level", "type": "number", "unit": "sample unit",
                "description": "crossing_period: crossing level (every crossing must be transversal); duty_fraction: threshold."},
            "width": {"title": "Transition half width", "type": "number", "exclusiveMinimum": 0, "unit": "sample unit",
                "description": "duty_fraction only: half width of the C2 smooth Heaviside."},
            "inner": {"title": "Differentiated functional", "type": "object",
                "description": "rate only: a series functional object of this same form applied to the time derivative of its sample."},
            "reference": {"title": "Reference waveform", "type": "array", "items": {"type": "number"}, "minItems": 3, "unit": "sample unit",
                "description": "waveform_mismatch only: the reference over one period at the phases j/M (e.g. one period of a healthy or measured opening waveform)."},
            "period_rows": {"title": "Rows per period", "type": "integer", "minimum": 3, "unit": "steps",
                "description": "waveform_mismatch only: sample rows per period; omit on the periodic window (one period)."},
            "normalise": {"title": "Shape only", "type": "boolean", "default": false,
                "description": "waveform_mismatch only: compare standardised shapes (mean and scale removed) instead of raw values."},
            "reference_sample": {"title": "Reference sample", "type": "string", "minLength": 1,
                "description": "phase_lag only: the sample whose harmonic phase is the zero of the lag."},
            "centre_rad": {"title": "Branch centre", "type": "number", "default": 0.0, "unit": "rad",
                "description": "phase_lag only: the reported lag lies in (centre - pi, centre + pi]."},
            "a": {"title": "Functional A", "type": "object",
                "description": "normalised_difference only: series functional A of (A - B)/(A + B)."},
            "b": {"title": "Functional B", "type": "object",
                "description": "normalised_difference only: series functional B of (A - B)/(A + B)."},
            "targets": targets
        },
        "required": ["kind"]
    });

    if let Some(design) = design_kinds
        && let Some(kind) = schema.pointer_mut("/properties/kind").and_then(Value::as_object_mut)
    {
        let mut kinds: Vec<&str> =
            SERIES_KINDS.iter().chain(PERIOD_KINDS.iter()).chain(COMPOSITE_KINDS.iter()).copied().collect();
        kinds.extend(design.iter().copied());
        kind.insert("enum".into(), json!(kinds));
    }
    schema
}

fn term_schema(sample_names: &[String], design_kinds: Option<&[&str]>) -> Value {
    json!({
        "title": "Response term",
        "type": "object",
        "properties": {
            "name": {"title": "Response name", "type": "string", "minLength": 1,
                "description": format!("Identifier ([A-Za-z_][A-Za-z0-9_]*, at most {MAX_NAME_LENGTH} characters), unique in the program: the name under which the response is reported and optimized.")},
            "functional": functional_schema(sample_names, design_kinds),
            "window": window_schema("Window override",
                "Optional, series functionals only: replaces the program window for this term.", None),
            "response": {"title": "Optimization role", "type": "object",
                "description": "Optional. minimise/maximise rows become objectives and upper/lower/equal rows constraints of the existing response program; omit to report the term only.",
                "properties": {
                    "sense": {"title": "Sense", "type": "string", "enum": RESPONSE_SENSES, "default": "minimise",
                        "description": "minimise, maximise: objective; upper, lower, equal: constraint with the bound target (default minimise)."},
                    "target": {"title": "Bound", "type": "number",
                        "description": "Required for upper, lower and equal: the bound in the response's own unit."},
                    "bound": {"title": "Bound (alias of target)", "type": "number",
                        "description": "Alias of target, normalised to target; when both are given they must agree."},
                    "weight": {"title": "Weight", "type": "number", "minimum": 0, "default": 1.0},
                    "scale": {"title": "Scale", "type": "number", "exclusiveMinimum": 0, "default": 1.0,
                        "description": "Response scale used by the normalisation of the optimizer."}},
                "additionalProperties": false}
        },
        "required": ["name", "functional"],
        "additionalProperties": false
    })
}

#[must_use]
pub fn editor_schema() -> Value {
    fragment(&[], None)
}

#[must_use]
pub fn editor_schema_for(sample_names: &[String], design_kinds: &[&str]) -> Value {
    fragment(sample_names, Some(design_kinds))
}

fn fragment(sample_names: &[String], design_kinds: Option<&[&str]>) -> Value {
    let mut terms = json!({"title": "Response terms", "type": "array", "minItems": 1,
        "items": term_schema(sample_names, design_kinds),
        "description": "Named scalar responses of the time history; each is a functional of a sample series over a time window, the period or frequency of a periodic orbit, or a provider design term."});
    if let (Some(first), Some(m)) = (sample_names.first(), terms.as_object_mut()) {
        m.insert(
            "default".into(),
            json!([{"name": "mean_response", "functional": {"kind": "mean", "sample": first}}]),
        );
    }
    json!({
        "title": "Dynamic responses",
        "type": "object",
        "description": "Time-weighted and cycle-averaged responses of a dynamic history with exact gradients (the gradient of a weighted time mean is the same weighted mean of the total instantaneous gradients).",
        "properties": {
            "schema": {"title": "Schema", "type": "string", "enum": [SCHEMA], "default": SCHEMA},
            "window": window_schema("Time window",
                "Default weights of every series functional.", Some(json!({"kind": "periodic"}))),
            "terms": terms
        },
        "required": ["schema", "window", "terms"],
        "additionalProperties": false
    })
}
