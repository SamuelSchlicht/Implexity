// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

#[derive(Clone,Copy,Debug)]
pub struct UnilateralContactCompliance {
    pub stiffness_pa_per_m: f64,
    pub transition_m: f64,
}
#[derive(Clone,Copy,Debug)]
pub struct ContactPotential {
    pub energy_j_per_m2: f64,
    pub pressure_pa: f64,
    pub tangent_pa_per_m: f64,
}
#[derive(Clone,Copy,Debug)]
pub struct ContactSecantPressure {
    pub pressure_pa: f64,
    pub previous_gap_derivative_pa_per_m: f64,
    pub current_gap_derivative_pa_per_m: f64,
}
impl UnilateralContactCompliance {
    fn validate(self) -> Result<(), &'static str> {
        if !self.stiffness_pa_per_m.is_finite() || self.stiffness_pa_per_m <= 0.
            || !self.transition_m.is_finite() || self.transition_m <= 0. {
            return Err("finite positive contact stiffness and transition required");
        }
        Ok(())
    }
    pub fn evaluate(self, gap_m: f64) -> Result<ContactPotential, &'static str> {
        self.validate()?;
        if !gap_m.is_finite() { return Err("finite contact gap required"); }
        let x = -gap_m;
        let k = self.stiffness_pa_per_m;
        let d = self.transition_m;
        let result = if x <= 0. {
            ContactPotential { energy_j_per_m2: 0., pressure_pa: 0., tangent_pa_per_m: 0. }
        } else if x < d {
            let r = x / d;
            let pressure = (k * r) * (0.5 * x);
            ContactPotential { energy_j_per_m2: pressure * (x / 3.), pressure_pa: pressure, tangent_pa_per_m: k * r }
        } else {
            let p = k * (x - 0.5 * d);
            ContactPotential { energy_j_per_m2: k * ((0.5 * x - 0.5 * d) * x + d * (d / 6.)), pressure_pa: p, tangent_pa_per_m: k }
        };
        if ![result.energy_j_per_m2, result.pressure_pa, result.tangent_pa_per_m].iter().all(|v| v.is_finite() && *v >= 0.) {
            return Err("contact potential overflow");
        }
        Ok(result)
    }
    pub fn secant(self, previous_gap_m: f64, current_gap_m: f64) -> Result<ContactSecantPressure, &'static str> {
        self.validate()?;
        if ![previous_gap_m, current_gap_m].iter().all(|v| v.is_finite()) { return Err("finite contact endpoint gaps required"); }
        let x0 = -previous_gap_m;
        let x1 = -current_gap_m;
        let dx = x1 - x0;
        if !dx.is_finite() { return Err("contact gap increment overflow"); }
        let mut cuts = vec![0., 1.];
        if dx != 0. {
            for x in [0., self.transition_m] {
                let t = (x - x0) / dx;
                if t.is_finite() && t > 0. && t < 1. { cuts.push(t); }
            }
        }
        cuts.sort_by(f64::total_cmp);
        let mut pressure = 0.;
        let mut previous = 0.;
        let mut current = 0.;
        let offset = 0.5 / 3_f64.sqrt();
        for interval in cuts.windows(2) {
            let width = interval[1] - interval[0];
            let mid = 0.5 * (interval[0] + interval[1]);
            for t in [mid - width * offset, mid + width * offset] {
                let gap = -((1. - t) * x0 + t * x1);
                let v = self.evaluate(gap)?;
                pressure += 0.5 * width * v.pressure_pa;
                previous -= 0.5 * width * (1. - t) * v.tangent_pa_per_m;
                current -= 0.5 * width * t * v.tangent_pa_per_m;
            }
        }
        if ![pressure, previous, current].iter().all(|v| v.is_finite()) { return Err("contact secant overflow"); }
        Ok(ContactSecantPressure { pressure_pa: pressure, previous_gap_derivative_pa_per_m: previous, current_gap_derivative_pa_per_m: current })
    }
}
