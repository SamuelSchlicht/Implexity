// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#[derive(Clone, Debug, PartialEq)]
pub struct LatticeConfig {
    pub period: f64,
    pub t_min: f64,
    pub t_max: f64,
    pub interface_eps: f64,
    pub grad_floor_rel: f64,
    pub soft_eps_rel: f64,
    pub det_floor: f64,
    pub cond_max: f64,
    pub min_elems_per_period: f64,
    pub control_dx: [f64; 3],
    pub s_max: f64,
    pub sec_scale: f64,
    pub sec_ratio: f64,
    pub res_scale: f64,
    pub w_eps: f64,
}

impl Default for LatticeConfig {
    fn default() -> Self {
        Self {
            period: 4.0e-3,
            t_min: 0.2e-3,
            t_max: 1.4e-3,
            interface_eps: 1e-3,
            grad_floor_rel: 0.05,
            soft_eps_rel: 1e-2,
            det_floor: 0.25,
            cond_max: 4.0,
            min_elems_per_period: 8.0,
            control_dx: [1.0e-3, 2.0e-3, 2.0e-3],
            s_max: 0.12,
            sec_scale: 0.5,
            sec_ratio: 2.0,
            res_scale: 0.3,
            w_eps: 1e-6,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Channels {
    pub stretch: bool,
    pub family: bool,
    pub mode: bool,
    pub secondary: bool,
    pub residual: bool,
    pub material: bool,
    pub phase: bool,
}

impl Default for Channels {
    fn default() -> Self {
        Self {
            stretch: true,
            family: true,
            mode: true,
            secondary: false,
            residual: false,
            material: true,
            phase: true,
        }
    }
}

impl Channels {
    #[must_use]
    pub fn all() -> Self {
        Self { secondary: true, residual: true, ..Self::default() }
    }

    #[must_use]
    pub fn active(&self) -> Vec<&'static str> {
        let mut names = vec!["a", "m"];
        for (on, name) in [
            (self.phase, "dphi"),
            (self.stretch, "s"),
            (self.family, "w"),
            (self.mode, "nu"),
            (self.secondary, "w2"),
            (self.residual, "res"),
            (self.material, "c"),
        ] {
            if on {
                names.push(name);
            }
        }
        names
    }
}
