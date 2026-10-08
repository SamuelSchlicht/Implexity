// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;
use implexity_core::CaeError;
use implexity_physics_base::PhysicsError;
use serde_json::{Value, json};

use crate::util::{argmin, jnp_interp, pow10, tie_max, tie_min};

pub const RESULT_NAMES: [&str; 10] = [
    "t_rupture_hours",
    "log10_t_rupture",
    "LMP_per_station",
    "creep_damage_sum",
    "creep_damage_per_station",
    "t_rupture_min_hours",
    "min_station_index",
    "creep_damage_station_sum",
    "creep_damage_max",
    "creep_damage_station_mean",
];

fn invalid(message: &str) -> PhysicsError {
    PhysicsError::validation(message, "creep_life")
}

#[derive(Debug, Clone)]
pub struct CreepLifeResults<S> {
    pub t_rupture_hours: Vec<S>,
    pub log10_t_rupture: Vec<S>,
    pub lmp_per_station: Vec<S>,
    pub creep_damage_sum: S,
    pub creep_damage_per_station: Vec<S>,
    pub t_rupture_min_hours: S,
    pub min_station_index: i32,
    pub creep_damage_station_sum: S,
    pub creep_damage_max: S,
    pub creep_damage_station_mean: S,
}

impl<S: Scalar> CreepLifeResults<S> {
    #[must_use]
    pub fn to_json(&self) -> Value {
        let v = |x: &[S]| x.iter().map(Scalar::value).collect::<Vec<f64>>();
        json!({"t_rupture_hours": v(&self.t_rupture_hours), "log10_t_rupture": v(&self.log10_t_rupture),
            "LMP_per_station": v(&self.lmp_per_station), "creep_damage_sum": self.creep_damage_sum.value(),
            "creep_damage_per_station": v(&self.creep_damage_per_station),
            "t_rupture_min_hours": self.t_rupture_min_hours.value(),
            "min_station_index": self.min_station_index,
            "creep_damage_station_sum": self.creep_damage_station_sum.value(),
            "creep_damage_max": self.creep_damage_max.value(),
            "creep_damage_station_mean": self.creep_damage_station_mean.value()})
    }
}


#[derive(Debug, Clone, PartialEq)]
pub struct CreepLifeLarsonMiller {
    pub switch_certificate: f64,
    pub curve_coordinate_tolerance: f64,
    pub calibrated_temperature_range_k: Option<(f64, f64)>,
}

impl Default for CreepLifeLarsonMiller {
    fn default() -> Self {
        Self {
            switch_certificate: 1.0,
            curve_coordinate_tolerance: 1e-10,
            calibrated_temperature_range_k: None,
        }
    }
}

impl CreepLifeLarsonMiller {

    pub fn new(
        switch_certificate: f64,
        curve_coordinate_tolerance: f64,
        calibrated_temperature_range_k: Option<(f64, f64)>,
    ) -> Result<Self, PhysicsError> {
        for value in [switch_certificate, curve_coordinate_tolerance] {
            if !value.is_finite() || value <= 0.0 {
                return Err(invalid("Sensitivity tolerances must be positive and finite"));
            }
        }
        if let Some((lo, hi)) = calibrated_temperature_range_k
            && !(lo.is_finite() && hi.is_finite() && 0.0 < lo && lo < hi)
        {
            return Err(invalid("Temperature calibration requires positive increasing bounds"));
        }
        Ok(Self { switch_certificate, curve_coordinate_tolerance, calibrated_temperature_range_k })
    }

    #[allow(clippy::too_many_arguments)]
    fn check_inputs<S: Scalar>(
        &self,
        temperature: &[S],
        stress: &[S],
        constant: S,
        curve: &[[S; 2]],
        dwell: &[S],
    ) -> Result<(), PhysicsError> {
        if temperature.is_empty() || stress.len() != temperature.len() {
            return Err(invalid("Temperature and stress must be equal-length, nonempty vectors"));
        }
        if curve.len() < 2 {
            return Err(invalid("Expected scalar C and an (n>=2,2) LMP table"));
        }
        if dwell.len() != 1 && dwell.len() != temperature.len() {
            return Err(invalid("Dwell must be scalar or match the station vector"));
        }
        let v = |x: &S| x.value();
        let (lo, hi) = (curve[0][0].value(), curve[curve.len() - 1][0].value());
        let mut valid = temperature.iter().all(|t| v(t).is_finite() && v(t) > 0.0)
            && stress.iter().all(|s| v(s).is_finite() && v(s) > 0.0)
            && constant.value().is_finite()
            && curve.iter().flatten().all(|c| v(c).is_finite())
            && curve.windows(2).all(|w| w[1][0].value() - w[0][0].value() > 0.0)
            && curve.iter().all(|r| r[1].value() > 0.0)
            && dwell.iter().all(|d| v(d).is_finite() && v(d) >= 0.0)
            && stress.iter().all(|s| {
                let l = v(s).log10();
                l >= lo && l <= hi
            });
        if let Some((lower, upper)) = self.calibrated_temperature_range_k {
            valid = valid && temperature.iter().all(|t| v(t) >= lower && v(t) <= upper);
        }
        if valid {
            Ok(())
        } else {
            Err(invalid("Invalid creep input or state outside the supplied material table/domain"))
        }
    }


    pub fn evaluate<S: Scalar>(
        &self,
        temperature: &[S],
        stress: &[S],
        constant: S,
        curve: &[[S; 2]],
        dwell: &[S],
    ) -> Result<CreepLifeResults<S>, PhysicsError> {
        let zero = [S::zero()];
        let dwell = if dwell.is_empty() { &zero[..] } else { dwell };
        self.check_inputs(temperature, stress, constant, curve, dwell)?;
        let xp: Vec<S> = curve.iter().map(|r| r[0]).collect();
        let fp: Vec<S> = curve.iter().map(|r| r[1]).collect();
        let nan = S::from_f64(f64::NAN);
        let n = temperature.len();
        let mut parameter = Vec::with_capacity(n);
        let mut log_time = Vec::with_capacity(n);
        let mut rupture = Vec::with_capacity(n);
        let mut damage = Vec::with_capacity(n);
        for i in 0..n {
            let p = jnp_interp(stress[i].log10(), &xp, &fp, Some(nan), Some(nan));
            let l = p / temperature[i] - constant;
            let t = pow10(l);
            let d = if dwell.len() == 1 { dwell[0] } else { dwell[i] };
            parameter.push(p);
            log_time.push(l);
            rupture.push(t);
            damage.push(d / t);
        }
        let mut station_sum = S::zero();
        for d in &damage {
            station_sum += *d;
        }
        let valid = rupture.iter().all(|t| t.value().is_finite() && t.value() > 0.0)
            && damage.iter().all(|d| d.value().is_finite())
            && station_sum.value().is_finite();
        if !valid {
            return Err(invalid("Creep rupture calculation overflowed, underflowed or became nonfinite"));
        }
        let values: Vec<f64> = rupture.iter().map(Scalar::value).collect();
        Ok(CreepLifeResults {
            t_rupture_min_hours: tie_min(&rupture),
            min_station_index: i32::try_from(argmin(&values)).unwrap_or(-1),
            creep_damage_max: tie_max(&damage),
            creep_damage_station_mean: station_sum / n as f64,
            creep_damage_sum: station_sum,
            creep_damage_station_sum: station_sum,
            t_rupture_hours: rupture,
            log10_t_rupture: log_time,
            lmp_per_station: parameter,
            creep_damage_per_station: damage,
        })
    }


    pub fn certify_sensitivity(
        &self,
        temperature: Option<&[f64]>,
        stress: Option<&[f64]>,
        constant: Option<f64>,
        curve: Option<&[[f64; 2]]>,
        dwell: &[f64],
    ) -> Result<Value, PhysicsError> {
        let (Some(temperature), Some(stress), Some(constant), Some(curve)) =
            (temperature, stress, constant, curve)
        else {
            return Err(invalid("Sensitivity admission requires temperature, stress, C and material table"));
        };
        self.evaluate(temperature, stress, constant, curve, dwell)?;
        let coordinates: Vec<f64> = stress.iter().map(|s| s.log10()).collect();
        let first = curve[0][0];
        let last = curve[curve.len() - 1][0];
        let margin = coordinates
            .iter()
            .map(|c| c - first)
            .fold(f64::INFINITY, f64::min)
            .min(coordinates.iter().map(|c| last - c).fold(f64::INFINITY, f64::min));
        let stress_min = stress.iter().copied().fold(f64::INFINITY, f64::min);
        if stress_min <= self.switch_certificate || margin <= self.curve_coordinate_tolerance {
            return Err(CaeError::contract("Creep sensitivity is at the stress/domain boundary").into());
        }
        let slopes: Vec<f64> = curve.windows(2).map(|w| (w[1][1] - w[0][1]) / (w[1][0] - w[0][0])).collect();
        for (index, knot) in curve[1..curve.len() - 1].iter().map(|r| r[0]).enumerate() {

            let (a, b) = (slopes[index], slopes[index + 1]);
            let close = (a - b).abs() <= 1e-12 * b.abs();
            let nearest = coordinates.iter().map(|c| (c - knot).abs()).fold(f64::INFINITY, f64::min);
            if !close && nearest <= self.curve_coordinate_tolerance {
                return Err(CaeError::contract(
                    "Creep sensitivity is at a nondifferentiable material-table knot",
                )
                .into());
            }
        }
        Ok(json!({"sensitivity_admissible": true,
            "qualification_scope": "station_interpolation_and_spatial_sums_only",
            "qualified_outputs": ["t_rupture_hours", "log10_t_rupture", "LMP_per_station",
                "creep_damage_per_station", "creep_damage_sum", "creep_damage_station_sum",
                "creep_damage_station_mean"],
            "excluded_outputs": ["t_rupture_min_hours", "min_station_index", "creep_damage_max"],
            "derivative_orders": [1, 2], "stress_min_Pa": stress_min,
            "table_boundary_margin_log10_Pa": margin,
            "switch_certificate": self.switch_certificate,
            "temperature_calibration_supplied": self.calibrated_temperature_range_k.is_some(),
            "aggregation": "spatial_station_statistics_not_temporal_accumulation"}))
    }


    #[allow(clippy::too_many_arguments)]
    pub fn certify_result_sensitivity(
        &self,
        temperature: &[f64],
        stress: &[f64],
        constant: f64,
        curve: &[[f64; 2]],
        dwell: &[f64],
        relative_extremum_tolerance: f64,
    ) -> Result<Value, PhysicsError> {
        if !relative_extremum_tolerance.is_finite() || relative_extremum_tolerance <= 0.0 {
            return Err(invalid("Extremum tolerance must be positive and finite"));
        }
        let mut certificate = self.certify_sensitivity(
            Some(temperature), Some(stress), Some(constant), Some(curve), dwell,
        )?;
        let result = self.evaluate(temperature, stress, constant, curve, dwell)?;
        let separation = |values: &[f64], minimum: bool| {
            let mut sorted = values.to_vec();
            sorted.sort_by(f64::total_cmp);
            if !minimum { sorted.reverse(); }
            if sorted.len() == 1 { return f64::INFINITY; }
            let scale = sorted[0].abs().max(sorted[1].abs());
            if scale == 0.0 { 0.0 } else { (sorted[0] - sorted[1]).abs() / scale }
        };
        let life_margin = separation(&result.t_rupture_hours, true);
        let damage_margin = separation(&result.creep_damage_per_station, false);
        if life_margin <= relative_extremum_tolerance || damage_margin <= relative_extremum_tolerance {
            return Err(CaeError::contract("Creep result sensitivity requires unique life and damage extrema").into());
        }
        let map = certificate.as_object_mut().expect("station certificate is an object");
        map.insert("qualification_scope".into(), json!("all_result_outputs_local_branch"));
        map.insert("qualified_outputs".into(), json!(RESULT_NAMES));
        map.remove("excluded_outputs");
        map.insert("relative_extremum_tolerance".into(), json!(relative_extremum_tolerance));
        map.insert("life_extremum_relative_margin".into(), json!(life_margin));
        map.insert("damage_extremum_relative_margin".into(), json!(damage_margin));
        Ok(certificate)
    }
}

