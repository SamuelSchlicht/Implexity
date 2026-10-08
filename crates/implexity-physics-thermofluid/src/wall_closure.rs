// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Map, Value, json};

use implexity_ad::Scalar;
use implexity_core::py_repr::repr_str;
use implexity_physics_base::model_errors::{PhysicsError, PhysicsModelErrorKind, PhysicsResult};
use implexity_physics_cfd::pyfmt::fmt_e;

pub const DEFAULT_MODES: [&str; 4] =
    ["adiabatic", "prescribed_flux", "prescribed_temperature", "coupled_exchange"];
const FLUX_IDENTITY_FLOOR_W_M2: f64 = 1.0e-9;

pub const RESULT_NAMES: [&str; 5] =
    ["heat_flux_W_m2", "mode_weights", "q_per_mode_W_m2", "T_wall_effective_K", "tau"];

fn validation(message: String, path: &str, details: Map<String, Value>) -> PhysicsError {
    PhysicsError::Model { kind: PhysicsModelErrorKind::Validation, message, path: path.into(), details }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WallClosureInputs<S> {
    pub mode_logits: Vec<Vec<S>>,
    pub q_authored: Vec<S>,
    pub t_authored: Vec<S>,
    pub film_h: Vec<S>,
    pub t_hot_side: Vec<S>,
    pub t_cold_side: Vec<S>,
    pub tau: S,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WallClosureResult<S> {
    pub heat_flux_w_m2: Vec<S>,
    pub mode_weights: Vec<[S; 4]>,
    pub q_per_mode_w_m2: Vec<[S; 4]>,
    pub t_wall_effective_k: Vec<S>,
    pub tau: S,
}

impl<S: Scalar> WallClosureResult<S> {
    #[must_use]
    pub fn to_map(&self) -> Map<String, Value> {
        let v = |x: &Vec<S>| json!(x.iter().map(Scalar::value).collect::<Vec<_>>());
        let rows =
            |x: &Vec<[S; 4]>| json!(x.iter().map(|r| r.map(|s| s.value()).to_vec()).collect::<Vec<_>>());
        let mut m = Map::new();
        m.insert("heat_flux_W_m2".into(), v(&self.heat_flux_w_m2));
        m.insert("mode_weights".into(), rows(&self.mode_weights));
        m.insert("q_per_mode_W_m2".into(), rows(&self.q_per_mode_w_m2));
        m.insert("T_wall_effective_K".into(), v(&self.t_wall_effective_k));
        m.insert("tau".into(), json!(self.tau.value()));
        m
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WallCertificate {
    pub min_weight_gap: f64,
    pub max_weight: f64,
    pub switch_certificate: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WallClosureField {
    pub modes: Vec<String>,
    pub switch_certificate: f64,
}

impl WallClosureField {

    pub fn new(modes: &[&str], switch_certificate: f64) -> PhysicsResult<Self> {
        if modes.is_empty() {
            return Err(PhysicsError::validation(
                "modes must be a non-empty tuple of str",
                "wall_closure.modes",
            ));
        }
        if modes != DEFAULT_MODES {
            let quoted = |m: &[&str]| {
                let parts: Vec<String> = m.iter().map(|s| repr_str(s)).collect();
                if parts.len() == 1 { format!("({},)", parts[0]) } else { format!("({})", parts.join(", ")) }
            };
            let mut details = Map::new();
            details.insert("expected".into(), json!(DEFAULT_MODES));
            details.insert("got".into(), json!(modes));
            return Err(validation(
                format!(
                    "SCREENING wall_closure supports only {} as its mode list; got {}",
                    quoted(&DEFAULT_MODES),
                    quoted(modes)
                ),
                "wall_closure.modes",
                details,
            ));
        }
        if !switch_certificate.is_finite() || switch_certificate <= 0.0 {
            let mut details = Map::new();
            details.insert("value".into(), json!(switch_certificate));
            return Err(validation(
                "switch_certificate must be a positive finite float".into(),
                "wall_closure.switch_certificate",
                details,
            ));
        }
        Ok(Self { modes: modes.iter().map(|s| (*s).to_string()).collect(), switch_certificate })
    }


    pub fn evaluate<S: Scalar>(&self, i: &WallClosureInputs<S>) -> PhysicsResult<WallClosureResult<S>> {
        let tau = i.tau.value();
        if !tau.is_finite() || tau <= 0.0 {
            let mut details = Map::new();
            details.insert("value".into(), json!(tau));
            return Err(validation("tau must be finite and > 0".into(), "wall_closure.tau", details));
        }
        let m = self.modes.len();
        let n = i.mode_logits.len();
        if let Some(bad) = i.mode_logits.iter().find(|r| r.len() != m) {
            let mut details = Map::new();
            details.insert("shape".into(), json!([n, bad.len()]));
            details.insert("n_modes_expected".into(), json!(m));
            return Err(validation(
                format!("mode_logits must have shape (n_stations, {m}); got ({n}, {})", bad.len()),
                "wall_closure.mode_logits",
                details,
            ));
        }
        if n < 1 {
            let mut details = Map::new();
            details.insert("n_stations".into(), json!(n));
            return Err(validation(
                "mode_logits must have at least one station".into(),
                "wall_closure.mode_logits",
                details,
            ));
        }
        let lens = [
            ("q_authored", i.q_authored.len()),
            ("T_authored", i.t_authored.len()),
            ("film_h", i.film_h.len()),
            ("T_hot_side", i.t_hot_side.len()),
            ("T_cold_side", i.t_cold_side.len()),
        ];
        if lens.iter().any(|(_, l)| *l != n) {
            let mut shapes = Map::new();
            for (k, l) in lens {
                shapes.insert(k.into(), json!([l]));
            }
            let mut details = Map::new();
            details.insert("shapes".into(), Value::Object(shapes));
            details.insert("expected_length".into(), json!(n));
            return Err(validation(
                format!("per-station arrays must all be 1-D with length n_stations={n}"),
                "wall_closure.arrays",
                details,
            ));
        }
        let mut result = WallClosureResult {
            heat_flux_w_m2: Vec::with_capacity(n),
            mode_weights: Vec::with_capacity(n),
            q_per_mode_w_m2: Vec::with_capacity(n),
            t_wall_effective_k: Vec::with_capacity(n),
            tau: i.tau,
        };
        for k in 0..n {
            let scaled: Vec<S> = i.mode_logits[k].iter().map(|l| *l / i.tau).collect();

            let shift = scaled.iter().fold(f64::NEG_INFINITY, |a, s| a.max(s.value()));
            let ex: Vec<S> = scaled.iter().map(|s| (*s - shift).exp()).collect();
            let total = ex[0] + ex[1] + ex[2] + ex[3];
            let w = [ex[0] / total, ex[1] / total, ex[2] / total, ex[3] / total];
            let h = i.film_h[k];
            let q = [
                i.t_hot_side[k] * 0.0,
                i.q_authored[k],
                h * (i.t_authored[k] - i.t_cold_side[k]),
                h * (i.t_hot_side[k] - i.t_cold_side[k]),
            ];
            let tw = [i.t_hot_side[k], i.t_hot_side[k], i.t_authored[k], i.t_hot_side[k]];
            result.heat_flux_w_m2.push(w[0] * q[0] + w[1] * q[1] + w[2] * q[2] + w[3] * q[3]);
            result.t_wall_effective_k.push(w[0] * tw[0] + w[1] * tw[1] + w[2] * tw[2] + w[3] * tw[3]);
            result.mode_weights.push(w);
            result.q_per_mode_w_m2.push(q);
        }
        Ok(result)
    }


    pub fn certify_sensitivity(&self, i: &WallClosureInputs<f64>) -> PhysicsResult<WallCertificate> {
        let out = self.evaluate(i)?;
        let w = &out.mode_weights;
        let q = &out.q_per_mode_w_m2;
        let mut min_gap = f64::INFINITY;
        let (mut worst_station, mut worst_i, mut worst_j) = (0usize, 0usize, 0usize);
        for a in 0..4 {
            for b in a + 1..4 {
                let mut best = f64::INFINITY;
                let mut best_k = 0usize;
                for k in 0..w.len() {
                    let active = w[k][a].min(w[k][b]) > self.switch_certificate;
                    let distinct = (q[k][a] - q[k][b]).abs() > FLUX_IDENTITY_FLOOR_W_M2;
                    let gap = if active && distinct { (w[k][a] - w[k][b]).abs() } else { f64::INFINITY };
                    if gap < best {
                        best = gap;
                        best_k = k;
                    }
                }
                if best < min_gap {
                    min_gap = best;
                    worst_station = best_k;
                    worst_i = a;
                    worst_j = b;
                }
            }
        }
        let max_weight = w.iter().flat_map(|r| r.iter().copied()).fold(0.0_f64, f64::max);
        if min_gap <= self.switch_certificate {
            return Err(PhysicsError::contract(format!(
                "wall-closure softmax is degenerate at a physically active mode pair: station {worst_station} carries modes {} and {} with weight gap {} <= switch_certificate = {}; the mode-blend tangent is tie-broken and the sensitivity is not admissible (softmax-degeneracy certificate)",
                repr_str(&self.modes[worst_i]),
                repr_str(&self.modes[worst_j]),
                fmt_e(min_gap, 3),
                fmt_e(self.switch_certificate, 0)
            )));
        }
        Ok(WallCertificate {
            min_weight_gap: min_gap,
            max_weight,
            switch_certificate: self.switch_certificate,
        })
    }
}

impl Default for WallClosureField {
    fn default() -> Self {
        Self { modes: DEFAULT_MODES.iter().map(|s| (*s).to_string()).collect(), switch_certificate: 1.0e-8 }
    }
}
