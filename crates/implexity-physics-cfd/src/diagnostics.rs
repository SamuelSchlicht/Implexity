// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Map, Value, json};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MassBalance {
    pub net_outward_m3_s: f64,
    pub throughput_m3_s: f64,
    pub relative: f64,
}

impl MassBalance {
    #[must_use]
    pub fn as_dict(&self) -> Value {
        json!({"net_outward_m3_s": self.net_outward_m3_s, "throughput_m3_s": self.throughput_m3_s, "relative": self.relative})
    }
}

#[must_use]
pub fn np_sum(values: &[f64]) -> f64 {
    pairwise(values)
}

fn pairwise(a: &[f64]) -> f64 {
    let n = a.len();
    if n < 8 {
        let mut res = 0.0;
        for v in a {
            res += v;
        }
        res
    } else if n <= 128 {
        let mut r = [a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]];
        let mut i = 8;
        while i < n - (n % 8) {
            for (k, slot) in r.iter_mut().enumerate() {
                *slot += a[i + k];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        while i < n {
            res += a[i];
            i += 1;
        }
        res
    } else {
        let mut n2 = n / 2;
        n2 -= n2 % 8;
        pairwise(&a[..n2]) + pairwise(&a[n2..])
    }
}

#[must_use]
pub fn mass_balance(flows: &[(String, f64)]) -> MassBalance {
    let vals: Vec<f64> = flows.iter().map(|(_, q)| *q).collect();
    let net = np_sum(&vals);
    let abs: Vec<f64> = vals.iter().map(|v| v.abs()).collect();
    let throughput = (np_sum(&abs) / 2.0).max(1e-30);
    MassBalance { net_outward_m3_s: net, throughput_m3_s: throughput, relative: net.abs() / throughput }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DimensionlessInputs {
    pub rho_kg_m3: f64,
    pub mu_pa_s: f64,
    pub velocity_m_s: f64,
    pub length_m: f64,
    pub cell_m: f64,
    pub k_solid_m2: f64,
    pub k_fluid_m2: f64,
    pub cp_j_kg_k: Option<f64>,
    pub k_w_m_k: Option<f64>,
}

#[must_use]
pub fn dimensionless(i: &DimensionlessInputs) -> Map<String, Value> {
    let re = i.rho_kg_m3 * i.velocity_m_s * i.length_m / i.mu_pa_s;
    let mut out = Map::new();
    out.insert("Re".into(), json!(re));
    out.insert("Re_cell".into(), json!(i.rho_kg_m3 * i.velocity_m_s * i.cell_m / i.mu_pa_s));
    out.insert("Da_solid".into(), json!(i.k_solid_m2 / i.length_m.powi(2)));
    out.insert("Da_fluid".into(), json!(i.k_fluid_m2 / i.length_m.powi(2)));
    if let (Some(cp), Some(k)) = (i.cp_j_kg_k, i.k_w_m_k) {
        let pr = i.mu_pa_s * cp / k;
        out.insert("Pr".into(), json!(pr));
        out.insert("Pe".into(), json!(re * pr));
    }
    out
}

fn l2(x: &[f64]) -> f64 {
    x.iter().map(|v| v * v).sum::<f64>().sqrt()
}

#[must_use]
pub fn convergence(
    residual: &[f64],
    divergence: &[f64],
    flows: &[(String, f64)],
    adjoint_residual: Option<&[f64]>,
) -> Map<String, Value> {
    let linf = |x: &[f64]| x.iter().fold(f64::NEG_INFINITY, |m, v| m.max(v.abs()));
    let mb = mass_balance(flows);
    let mut out = Map::new();
    out.insert("residual_linf".into(), json!(linf(residual)));
    out.insert("residual_l2".into(), json!(l2(residual)));
    out.insert("divergence_linf_s-1".into(), json!(linf(divergence)));
    let abs: Vec<f64> = divergence.iter().map(|v| v.abs()).collect();
    out.insert("divergence_mean_abs_s-1".into(), json!(np_sum(&abs) / divergence.len() as f64));
    out.insert("mass_balance_relative".into(), json!(mb.relative));
    out.insert("net_outward_m3_s".into(), json!(mb.net_outward_m3_s));
    out.insert("throughput_m3_s".into(), json!(mb.throughput_m3_s));
    out.insert("relative".into(), json!(mb.relative));
    if let Some(a) = adjoint_residual {
        out.insert("adjoint_residual_linf".into(), json!(linf(a)));
        out.insert("adjoint_residual_l2".into(), json!(l2(a)));
    }
    out
}


