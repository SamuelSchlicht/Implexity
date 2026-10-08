// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;
use implexity_physics_thermofluid::equation_of_state::EquationOfState;
use serde_json::{Map, Value, json};
use crate::errors::{PResult, ModelError};
use crate::plug_nozzle_expansion::{check_bounded, check_positive};
use crate::pyval::{fmt_e, repr_float};

const AREA_FLOOR_M2: f64 = 1.0e-30;

const MIX_EPS: f64 = 1.0e-30;

pub const RESULT_NAMES: [&str; 16] = [
    "p_chamber_Pa",
    "T_chamber_K",
    "y_chamber",
    "mdot_total_kg_s",
    "mixing_efficiency_estimated",
    "fuel_pressure_drop_Pa",
    "ox_pressure_drop_Pa",
    "orifice_area_fuel_m2",
    "orifice_area_ox_m2",
    "plate_fraction",
    "interface_length_proxy_m2",
    "face_field",
    "pressure_imbalance_Pa",
    "Cp_chamber_J_per_kg_K",
    "rho_chamber_kg_m3",
    "eta_effective",
];

pub fn softmax_rows<S: Scalar>(logits: &[[S; 3]], temperature: f64) -> Vec<[S; 3]> {
    logits
        .iter()
        .map(|row| {
            let scaled = row.map(|x| x / temperature);
            let m = crate::screening::reduce_max(&scaled);
            let e = scaled.map(|x| (x - m).exp());
            let total = e[0] + e[1] + e[2];
            e.map(|x| x / total)
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct InjectorInputs<S> {
    pub face_logits: Vec<[S; 3]>,
    pub cell_area: Vec<S>,
    pub p_fuel_supply: S,
    pub t_fuel_supply: S,
    pub y_fuel: Vec<S>,
    pub mdot_fuel: S,
    pub p_ox_supply: S,
    pub t_ox_supply: S,
    pub y_ox: Vec<S>,
    pub mdot_ox: S,
    pub combustion_efficiency: S,
    pub heat_of_combustion_j_per_kg: S,
    pub cd_fuel: Option<S>,
    pub cd_ox: Option<S>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InjectorResult<S> {
    pub scalars: Vec<(&'static str, S)>,
    pub y_chamber: Vec<S>,
    pub face_field: Vec<[S; 3]>,
}

impl InjectorResult<f64> {
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        for name in RESULT_NAMES {
            let v = match name {
                "y_chamber" => json!(self.y_chamber),
                "face_field" => json!(self.face_field),
                _ => json!(self.get(name)),
            };
            m.insert(name.to_string(), v);
        }
        Value::Object(m)
    }
}

impl<S: Copy> InjectorResult<S> {
    #[must_use]
    pub fn get(&self, name: &str) -> Option<S> {
        self.scalars.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InjectorMixing {
    pub eos_fuel: EquationOfState,
    pub eos_ox: EquationOfState,
    pub eos_chamber: EquationOfState,
    pub default_cd_fuel: f64,
    pub default_cd_ox: f64,
    pub softmax_temperature: f64,
    pub switch_certificate: f64,
}

impl InjectorMixing {

    pub fn new(
        eos_fuel: EquationOfState,
        eos_ox: EquationOfState,
        eos_chamber: EquationOfState,
        default_cd_fuel: f64,
        default_cd_ox: f64,
        softmax_temperature: f64,
        switch_certificate: f64,
    ) -> PResult<Self> {
        for (name, v) in [("default_cd_fuel", default_cd_fuel), ("default_cd_ox", default_cd_ox)] {
            if !v.is_finite() || v <= 0.0 || v > 1.0 {
                return Err(ModelError::validation(
                    format!("{name} must be a finite float in (0, 1]"),
                    format!("injector_mixing.{name}"),
                )
                .detail("value", json!(v)));
            }
        }
        if !softmax_temperature.is_finite() || softmax_temperature <= 0.0 {
            return Err(ModelError::validation(
                "softmax_temperature must be a positive finite float",
                "injector_mixing.softmax_temperature",
            )
            .detail("value", json!(softmax_temperature)));
        }
        if !switch_certificate.is_finite() || switch_certificate <= 0.0 {
            return Err(ModelError::validation(
                "switch_certificate must be a positive finite float",
                "injector_mixing.switch_certificate",
            )
            .detail("value", json!(switch_certificate)));
        }
        Ok(Self {
            eos_fuel,
            eos_ox,
            eos_chamber,
            default_cd_fuel,
            default_cd_ox,
            softmax_temperature,
            switch_certificate,
        })
    }


    pub fn with_defaults(
        eos_fuel: EquationOfState,
        eos_ox: EquationOfState,
        eos_chamber: EquationOfState,
    ) -> PResult<Self> {
        Self::new(eos_fuel, eos_ox, eos_chamber, 0.65, 0.65, 1.0, 1.0e-8)
    }


    #[allow(clippy::too_many_lines)]
    pub fn evaluate<S: Scalar>(&self, i: &InjectorInputs<S>) -> PResult<InjectorResult<S>> {
        let pre = "injector_mixing";
        check_positive(pre, "p_fuel_supply", i.p_fuel_supply.value(), 0.0)?;
        check_positive(pre, "T_fuel_supply", i.t_fuel_supply.value(), 0.0)?;
        check_positive(pre, "mdot_fuel", i.mdot_fuel.value(), 0.0)?;
        check_positive(pre, "p_ox_supply", i.p_ox_supply.value(), 0.0)?;
        check_positive(pre, "T_ox_supply", i.t_ox_supply.value(), 0.0)?;
        check_positive(pre, "mdot_ox", i.mdot_ox.value(), 0.0)?;
        check_bounded(pre, "combustion_efficiency", i.combustion_efficiency.value(), 0.0, 1.0)?;
        let q = i.heat_of_combustion_j_per_kg.value();
        if !q.is_finite() || q < 0.0 {
            return Err(ModelError::validation(
                "heat_of_combustion_J_per_kg must be finite and >= 0",
                "injector_mixing.heat_of_combustion_J_per_kg",
            )
            .detail("value", json!(q)));
        }
        for (name, cd) in [("cd_fuel", i.cd_fuel), ("cd_ox", i.cd_ox)] {
            if let Some(c) = cd.map(|c| c.value())
                && (!c.is_finite() || c <= 0.0 || c > 1.0)
            {
                return Err(ModelError::validation(
                    format!("{name} must be finite and in (0, 1]"),
                    format!("injector_mixing.{name}"),
                )
                .detail("value", json!(c)));
            }
        }
        let n_cells = i.face_logits.len();
        if n_cells < 1 {
            return Err(ModelError::validation(
                "face_logits must have at least one cell",
                "injector_mixing.face_logits",
            ));
        }
        if i.cell_area.len() != n_cells {
            return Err(ModelError::validation(
                format!("cell_area must be 1-D with length {n_cells} matching face_logits.shape[0]"),
                "injector_mixing.cell_area",
            )
            .detail("shape", json!([i.cell_area.len()])));
        }
        let n_species = self.eos_chamber.species.len();
        for (name, y) in [("y_fuel", &i.y_fuel), ("y_ox", &i.y_ox)] {
            if y.len() != n_species {
                return Err(ModelError::validation(
                    format!(
                        "{name} must have shape ({n_species},) matching eos_chamber.species; got ({},)",
                        y.len()
                    ),
                    format!("injector_mixing.{name}"),
                ));
            }
        }
        let cd_f = i.cd_fuel.unwrap_or_else(|| S::from_f64(self.default_cd_fuel));
        let cd_o = i.cd_ox.unwrap_or_else(|| S::from_f64(self.default_cd_ox));
        let w = softmax_rows(&i.face_logits, self.softmax_temperature);
        let dot = |f: &dyn Fn(&[S; 3]) -> S| {
            let mut s = S::zero();
            for (row, a) in w.iter().zip(&i.cell_area) {
                s += f(row) * *a;
            }
            s
        };
        let a_f = dot(&|r| r[0]);
        let a_o = dot(&|r| r[1]);
        let mut a_total = S::zero();
        for a in &i.cell_area {
            a_total += *a;
        }
        let plate_fraction = dot(&|r| r[2]) / a_total.max_f64(AREA_FLOOR_M2);
        for (name, eos) in [("eos_fuel", &self.eos_fuel), ("eos_ox", &self.eos_ox)] {
            if eos.species.len() != n_species {
                return Err(ModelError::validation(
                    format!(
                        "{name}.species count ({}) must equal eos_chamber.species count ({n_species}) at SCREENING fidelity; supply-species / chamber-species mismatch is a future INTERMEDIATE-rung generalisation.",
                        eos.species.len()
                    ),
                    format!("injector_mixing.{name}"),
                ));
            }
        }
        let props_f = self.eos_fuel.evaluate(&i.y_fuel, i.p_fuel_supply, i.t_fuel_supply)?;
        let props_o = self.eos_ox.evaluate(&i.y_ox, i.p_ox_supply, i.t_ox_supply)?;
        let a_f_safe = a_f.max_f64(AREA_FLOOR_M2);
        let a_o_safe = a_o.max_f64(AREA_FLOOR_M2);
        let (mf, mo) = (i.mdot_fuel, i.mdot_ox);
        let dp_f = mf * mf / (props_f.density * 2.0 * cd_f * cd_f * a_f_safe * a_f_safe);
        let dp_o = mo * mo / (props_o.density * 2.0 * cd_o * cd_o * a_o_safe * a_o_safe);
        let mdot_total = mf + mo;
        let p_f_post = i.p_fuel_supply - dp_f;
        let p_o_post = i.p_ox_supply - dp_o;
        let p_c = (mf * p_f_post + mo * p_o_post) / mdot_total;
        let y_c: Vec<S> =
            i.y_fuel.iter().zip(&i.y_ox).map(|(f, o)| (mf * *f + mo * *o) / mdot_total).collect();
        let interface = dot(&|r| r[0] * 4.0 * r[1]);
        let non_plate = dot(&|r| -r[2] + 1.0);
        let eta_mix = (interface / non_plate.max_f64(MIX_EPS)).clip(0.0, 1.0);
        let t_ref = (mf * i.t_fuel_supply + mo * i.t_ox_supply) / mdot_total;
        let cp_ref = (mf * props_f.cp + mo * props_o.cp) / mdot_total;
        let eta_eff = i.combustion_efficiency * eta_mix;
        let q_release = mf * i.heat_of_combustion_j_per_kg / mdot_total;
        let t_c0 = t_ref + eta_eff * q_release / cp_ref;
        let cp_c = self.eos_chamber.evaluate(&y_c, p_c, t_c0)?.cp;
        let t_c = t_ref + eta_eff * q_release / cp_c;
        let rho_c = self.eos_chamber.evaluate(&y_c, p_c, t_c)?.density;
        Ok(InjectorResult {
            scalars: vec![
                ("p_chamber_Pa", p_c),
                ("T_chamber_K", t_c),
                ("mdot_total_kg_s", mdot_total),
                ("mixing_efficiency_estimated", eta_mix),
                ("fuel_pressure_drop_Pa", dp_f),
                ("ox_pressure_drop_Pa", dp_o),
                ("orifice_area_fuel_m2", a_f),
                ("orifice_area_ox_m2", a_o),
                ("plate_fraction", plate_fraction),
                ("interface_length_proxy_m2", interface),
                ("pressure_imbalance_Pa", p_f_post - p_o_post),
                ("Cp_chamber_J_per_kg_K", cp_c),
                ("rho_chamber_kg_m3", rho_c),
                ("eta_effective", eta_eff),
            ],
            y_chamber: y_c,
            face_field: w,
        })
    }


    pub fn certify_sensitivity(&self, face_logits: &[[f64; 3]], cell_area: &[f64]) -> PResult<Value> {
        if cell_area.len() != face_logits.len() {
            return Err(ModelError::validation(
                format!("cell_area must be 1-D of length {}", face_logits.len()),
                "injector_mixing.certify_sensitivity.cell_area",
            )
            .detail("shape", json!([cell_area.len()])));
        }
        let w: Vec<[f64; 3]> = face_logits
            .iter()
            .map(|row| {
                let scaled = row.map(|x| x / self.softmax_temperature);
                let m = scaled.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let e = scaled.map(|x| (x - m).exp());
                let t = e[0] + e[1] + e[2];
                e.map(|x| x / t)
            })
            .collect();
        let a_total = implexity_mesh::numeric::pairwise_sum(cell_area);
        if a_total <= 0.0 || !a_total.is_finite() {
            return Err(ModelError::validation(
                "cell_area must sum to a positive finite total area",
                "injector_mixing.certify_sensitivity.cell_area",
            )
            .detail("A_total", json!(a_total)));
        }
        let col = |c: usize| {
            implexity_mesh::numeric::pairwise_sum(
                &w.iter().zip(cell_area).map(|(r, a)| r[c] * a).collect::<Vec<_>>(),
            )
        };
        let (frac_f, frac_o) = (col(0) / a_total, col(1) / a_total);
        let sc = self.switch_certificate;
        if frac_f <= sc || frac_o <= sc {
            return Err(ModelError::contract(format!(
                "InjectorMixing vanishing orifice area: A_fuel / A_total = {}, A_ox / A_total = {}, switch_certificate = {}.  The reciprocal-area pressure-drop sensitivity is dominated by the 1 / A^2 tail rather than by the mixing physics.  Grow the softmax fuel or oxidiser channel (raise the corresponding logit) before requesting a derivative.  (switch-distance certificate)",
                fmt_e(frac_f, 3),
                fmt_e(frac_o, 3),
                fmt_e(sc, 0)
            )));
        }
        let gaps: Vec<f64> = w
            .iter()
            .map(|r| {
                let mut s = *r;
                s.sort_by(f64::total_cmp);
                s[2] - s[1]
            })
            .collect();
        let mut worst = 0;
        for (k, g) in gaps.iter().enumerate() {
            if *g < gaps[worst] {
                worst = k;
            }
        }
        let min_gap = gaps[worst];
        if min_gap <= sc {
            let weights = w[worst].iter().map(|x| repr_float(*x)).collect::<Vec<_>>().join(", ");
            return Err(ModelError::contract(format!(
                "InjectorMixing softmax degeneracy: cell {worst} has the two largest softmax weights within {} of each other (switch_certificate = {}, weights = ({weights})).  The identity of the winning phase at that cell is set by tie-breaking rather than by the face logits; the sub-gradient is multi-valued.  Sharpen the logits (scale them up, or lower softmax_temperature) before requesting a derivative.  (switch-distance certificate)",
                fmt_e(min_gap, 3),
                fmt_e(sc, 0)
            )));
        }
        Ok(json!({"fuel_area_fraction": frac_f, "ox_area_fraction": frac_o, "min_softmax_gap": min_gap,
                  "switch_certificate": sc, "sensitivity_admissible": true}))
    }
}
