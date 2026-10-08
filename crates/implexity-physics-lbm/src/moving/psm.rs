// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;

use super::collision::{Collision, CollisionBar};
use super::lattice::{Lattice, dot_c, equilibrium, equilibrium_vjp, moments};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CouplingLaw {
    Psm,
    PsmSuperposition,
    Brinkman {
        drag_max_per_s: f64,
        drag_shape: f64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Smagorinsky {
    pub constant: f64,
    pub norm_floor: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct CellParameters<S> {
    pub omega: S,
    pub accel: [S; 3],
    pub c_alpha: S,
}

#[derive(Clone, Copy, Debug)]
pub struct SolidInput<S> {
    pub d: S,
    pub m: [S; 3],
}

#[derive(Clone, Copy, Debug)]
pub struct CellOutput<S, const Q: usize> {
    pub post: [S; Q],
    pub dp: [S; 3],
    pub rho: S,
    pub u: [S; 3],
}

#[derive(Clone, Copy, Debug)]
pub struct CellBar<S, const Q: usize> {
    pub f: [S; Q],
    pub omega: S,
    pub accel: [S; 3],
    pub c_alpha: S,
    pub d: S,
    pub m: [S; 3],
}

#[inline]
#[must_use]
pub fn saturate<S: Scalar>(d: S, kappa: f64) -> S {
    let lo = 1.0 - kappa;
    let v = d.value();
    if v <= lo {
        d
    } else if v >= 1.0 + kappa {
        S::one()
    } else {
        let t = (d - lo) / (2.0 * kappa);
        let t2 = t * t;
        let h = t - t2 * t + t2 * t2 * 0.5;
        S::from_f64(lo) + h * (2.0 * kappa)
    }
}

#[inline]
#[must_use]
pub fn saturate_derivative<S: Scalar>(d: S, kappa: f64) -> S {
    let lo = 1.0 - kappa;
    let v = d.value();
    if v <= lo {
        S::one()
    } else if v >= 1.0 + kappa {
        S::zero()
    } else {
        let t = (d - lo) / (2.0 * kappa);
        let s = S::one() - t;
        s * s * (t * 2.0 + 1.0)
    }
}

#[inline]
pub(crate) fn saturated_ratio<S: Scalar>(d: S, kappa: f64) -> (S, S) {
    if d.value() <= 1.0 - kappa {
        (S::one(), S::zero())
    } else {
        let e = saturate(d, kappa);
        let de = saturate_derivative(d, kappa);
        (e / d, (de * d - e) / (d * d))
    }
}

#[inline]
pub(crate) fn beta<S: Scalar>(d: S, t: S, kappa: f64) -> (S, S, S) {
    if d.value() <= 1.0 - kappa {
        let den = S::one() - d + t;
        let b = t / den;
        let den2 = den * den;
        (b, t / den2, (S::one() - d) / den2)
    } else {
        let e = saturate(d, kappa);
        let de = saturate_derivative(d, kappa);
        let den = S::one() - e + t;
        let bf = e * t / den;
        let den2 = den * den;
        let bf_e = t * (t + 1.0) / den2;
        let bf_t = e * (S::one() - e) / den2;
        (bf / d, bf_e * de / d - bf / (d * d), bf_t / d)
    }
}

#[inline]
fn resistance<S: Scalar>(d: S, c_alpha: S, q: f64, kappa: f64) -> (S, S, [S; 4]) {
    let e = saturate(d, kappa);
    let de = saturate_derivative(d, kappa);
    let den = S::one() * (q + 1.0) - e;
    let alpha = c_alpha * e / den;
    let a_e = c_alpha * (q + 1.0) / (den * den);
    let a_c = e / den;
    if d.value() <= 1.0 - kappa {
        let g = c_alpha / den;
        (alpha, g, [a_e * de, a_c, c_alpha / (den * den), S::one() / den])
    } else {
        let g = alpha / d;
        (alpha, g, [a_e * de, a_c, a_e * de / d - alpha / (d * d), a_c / d])
    }
}

#[inline]
fn smagorinsky<S: Scalar, const Q: usize, L: Lattice<Q>>(
    les: &Smagorinsky,
    f: &[S; Q],
    rho: S,
    u: [S; 3],
    omega: S,
) -> S {
    let eq = equilibrium::<S, Q, L>(rho, u);
    let mut pi = [[S::zero(); 3]; 3];
    for i in 0..Q {
        let n = f[i] - eq[i];
        let c = L::CF[i];
        for a in 0..3 {
            if c[a] == 0.0 {
                continue;
            }
            for b in 0..3 {
                if c[b] != 0.0 {
                    pi[a][b] += n * (c[a] * c[b]);
                }
            }
        }
    }
    let mut qn = S::zero();
    for row in &pi {
        for v in row {
            qn += *v * *v;
        }
    }
    let delta = les.norm_floor;
    let p = (qn + delta * delta).sqrt() - delta;
    let k = 18.0 * std::f64::consts::SQRT_2 * les.constant * les.constant;
    let tau0 = S::one() / omega;
    let r = (tau0 * tau0 + p * k / rho).sqrt();
    S::one() / ((tau0 + r) * 0.5)
}

#[inline]
#[must_use]
pub fn cell_update<S: Scalar, const Q: usize, L: Lattice<Q>>(
    collision: &Collision<Q, L>,
    law: CouplingLaw,
    kappa: f64,
    les: Option<&Smagorinsky>,
    f: &[S; Q],
    p: &CellParameters<S>,
    solid: Option<SolidInput<S>>,
) -> CellOutput<S, Q> {
    let (rho, j) = moments::<S, Q, L>(f);
    let a = p.accel;
    match (solid, law) {
        (Some(s), CouplingLaw::Brinkman { drag_shape, .. }) => {
            let (alpha, gamma, _) = resistance(s.d, p.c_alpha, drag_shape, kappa);
            let den = S::one() + alpha * 0.5;
            let u: [S; 3] = std::array::from_fn(|d| (j[d] / rho + a[d] * 0.5 + gamma * s.m[d] * 0.5) / den);
            let omega = match les {
                Some(l) => smagorinsky::<S, Q, L>(l, f, rho, u, p.omega),
                None => p.omega,
            };
            let z: [S; 3] = std::array::from_fn(|d| gamma * s.m[d] - alpha * u[d]);
            let force: [S; 3] = std::array::from_fn(|d| rho * (a[d] + z[d]));
            let post = collision.collide(f, rho, u, force, omega);
            let dp = std::array::from_fn(|d| rho * z[d]);
            CellOutput { post, dp, rho, u }
        }
        (solid, _) => {
            let u: [S; 3] = std::array::from_fn(|d| j[d] / rho + a[d] * 0.5);
            let omega = match les {
                Some(l) => smagorinsky::<S, Q, L>(l, f, rho, u, p.omega),
                None => p.omega,
            };
            let force: [S; 3] = std::array::from_fn(|d| rho * a[d]);
            let fluid = collision.collide(f, rho, u, force, omega);
            let Some(s) = solid else {
                return CellOutput { post: fluid, dp: [S::zero(); 3], rho, u };
            };
            let t = S::one() / omega - 0.5;
            let (b_over_d, _, _) = beta(s.d, t, kappa);
            let bb = b_over_d * s.d;
            let eq = equilibrium::<S, Q, L>(rho, u);
            let m2 = s.m[0] * s.m[0] + s.m[1] * s.m[1] + s.m[2] * s.m[2];
            let mut dp = [S::zero(); 3];
            let post = std::array::from_fn(|i| {
                let o = L::OPPOSITE[i];
                let cm = dot_c::<S, Q, L>(i, s.m);
                let solid_eq = rho
                    * L::W[i]
                    * (bb + b_over_d * cm * 3.0 + b_over_d * cm * cm * 4.5 / s.d - b_over_d * m2 * 1.5 / s.d);
                let superposition = matches!(law, CouplingLaw::PsmSuperposition);
                let omega_s =
                    if superposition { solid_eq - bb * eq[i] } else { bb * (f[o] - eq[o] - f[i]) + solid_eq };
                let c = L::CF[i];
                for d in 0..3 {
                    if c[d] != 0.0 {
                        dp[d] += omega_s * c[d];
                    }
                }
                if superposition {
                    fluid[i] + omega_s
                } else {
                    f[i] + (S::one() - bb) * (fluid[i] - f[i]) + omega_s
                }
            });
            CellOutput { post, dp, rho, u }
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[inline]
#[must_use]
pub fn cell_update_vjp<S: Scalar, const Q: usize, L: Lattice<Q>>(
    collision: &Collision<Q, L>,
    law: CouplingLaw,
    kappa: f64,
    les: Option<&Smagorinsky>,
    f: &[S; Q],
    p: &CellParameters<S>,
    solid: Option<SolidInput<S>>,
    g: &[S; Q],
    dp_bar: [S; 3],
    rho_bar_ext: S,
    u_bar_ext: [S; 3],
) -> CellBar<S, Q> {
    let (rho, j) = moments::<S, Q, L>(f);
    let a = p.accel;
    let mut f_bar = [S::zero(); Q];
    let mut rho_bar = rho_bar_ext;
    let mut u_bar = u_bar_ext;
    let mut accel_bar = [S::zero(); 3];
    let mut c_alpha_bar = S::zero();
    let mut d_bar = S::zero();
    let mut m_bar = [S::zero(); 3];
    let omega_eff_bar;
    let omega_eff;

    let mut brinkman: Option<(S, S, [S; 4], S, [S; 3], S)> = None;
    let u: [S; 3];
    match (solid, law) {
        (Some(s), CouplingLaw::Brinkman { drag_shape, .. }) => {
            let (alpha, gamma, partials) = resistance(s.d, p.c_alpha, drag_shape, kappa);
            let den = S::one() + alpha * 0.5;
            let sv: [S; 3] = std::array::from_fn(|d| j[d] / rho + a[d] * 0.5 + gamma * s.m[d] * 0.5);
            u = std::array::from_fn(|d| sv[d] / den);
            omega_eff = match les {
                Some(l) => smagorinsky::<S, Q, L>(l, f, rho, u, p.omega),
                None => p.omega,
            };
            let z: [S; 3] = std::array::from_fn(|d| gamma * s.m[d] - alpha * u[d]);
            let force: [S; 3] = std::array::from_fn(|d| rho * (a[d] + z[d]));
            let cb: CollisionBar<S, Q> = collision.collide_vjp(f, rho, u, force, omega_eff, g);
            f_bar = cb.f;
            rho_bar += cb.rho;
            for d in 0..3 {
                u_bar[d] += cb.u[d];
            }
            omega_eff_bar = cb.omega;
            let mut alpha_bar = S::zero();
            let mut gamma_bar = S::zero();
            for d in 0..3 {
                rho_bar += cb.force[d] * (a[d] + z[d]) + dp_bar[d] * z[d];
                accel_bar[d] += cb.force[d] * rho;
                let zb = (cb.force[d] + dp_bar[d]) * rho;
                gamma_bar += zb * s.m[d];
                m_bar[d] += zb * gamma;
                alpha_bar -= zb * u[d];
                u_bar[d] -= zb * alpha;
            }
            brinkman = Some((alpha_bar, gamma_bar, partials, den, sv, gamma));
        }
        (solid, _) => {
            u = std::array::from_fn(|d| j[d] / rho + a[d] * 0.5);
            omega_eff = match les {
                Some(l) => smagorinsky::<S, Q, L>(l, f, rho, u, p.omega),
                None => p.omega,
            };
            let force: [S; 3] = std::array::from_fn(|d| rho * a[d]);
            let mut fluid_bar = *g;
            let mut omega_extra = S::zero();
            if let Some(s) = solid {
                let superposition = matches!(law, CouplingLaw::PsmSuperposition);
                let fluid = collision.collide(f, rho, u, force, omega_eff);
                let tau = S::one() / omega_eff;
                let t = tau - 0.5;
                let (b_over_d, beta_d, beta_t) = beta(s.d, t, kappa);
                let bb = b_over_d * s.d;
                let eq = equilibrium::<S, Q, L>(rho, u);
                let m2 = s.m[0] * s.m[0] + s.m[1] * s.m[1] + s.m[2] * s.m[2];
                let mut b_bar = S::zero();
                let mut beta_bar = S::zero();
                let mut eq_bar = [S::zero(); Q];
                let mut wsum = S::zero();
                for i in 0..Q {
                    let o = L::OPPOSITE[i];
                    let c = L::CF[i];
                    let mut h = g[i];
                    for d in 0..3 {
                        if c[d] != 0.0 {
                            h += dp_bar[d] * c[d];
                        }
                    }
                    if superposition {
                        fluid_bar[i] = g[i];
                        b_bar += h * (rho * L::W[i] - eq[i]);
                        eq_bar[i] -= h * bb;
                    } else {
                        fluid_bar[i] = g[i] * (S::one() - bb);
                        f_bar[i] += g[i] * bb;
                        b_bar += g[i] * (f[i] - fluid[i]);
                        b_bar += h * (f[o] - eq[o] - f[i] + rho * L::W[i]);
                        f_bar[o] += h * bb;
                        f_bar[i] -= h * bb;
                        eq_bar[o] -= h * bb;
                    }
                    let cm = dot_c::<S, Q, L>(i, s.m);
                    let w = h * L::W[i];
                    rho_bar += w
                        * (bb + b_over_d * cm * 3.0 + b_over_d * cm * cm * 4.5 / s.d
                            - b_over_d * m2 * 1.5 / s.d);
                    let wr = w * rho;
                    beta_bar += wr * (cm * 3.0 + cm * cm * 4.5 / s.d - m2 * 1.5 / s.d);
                    let cm_bar = wr * b_over_d * (cm * 9.0 / s.d + 3.0);
                    for d in 0..3 {
                        if c[d] != 0.0 {
                            m_bar[d] += cm_bar * c[d];
                        }
                    }
                    wsum += wr;
                    d_bar += wr * b_over_d * (m2 * 1.5 - cm * cm * 4.5) / (s.d * s.d);
                }
                for d in 0..3 {
                    m_bar[d] -= wsum * b_over_d * s.m[d] * 3.0 / s.d;
                }
                equilibrium_vjp::<S, Q, L>(rho, u, &eq_bar, &mut rho_bar, &mut u_bar);
                beta_bar += b_bar * s.d;
                d_bar += b_bar * b_over_d + beta_bar * beta_d;
                let t_bar = beta_bar * beta_t;
                omega_extra = -t_bar * tau * tau;
            }
            let cb: CollisionBar<S, Q> = collision.collide_vjp(f, rho, u, force, omega_eff, &fluid_bar);
            for i in 0..Q {
                f_bar[i] += cb.f[i];
            }
            rho_bar += cb.rho;
            for d in 0..3 {
                u_bar[d] += cb.u[d];
                rho_bar += cb.force[d] * a[d];
                accel_bar[d] += cb.force[d] * rho;
            }
            omega_eff_bar = cb.omega + omega_extra;
        }
    }

    let omega_bar = match les {
        None => omega_eff_bar,
        Some(l) => {
            let eq = equilibrium::<S, Q, L>(rho, u);
            let mut pi = [[S::zero(); 3]; 3];
            for i in 0..Q {
                let n = f[i] - eq[i];
                let c = L::CF[i];
                for x in 0..3 {
                    if c[x] == 0.0 {
                        continue;
                    }
                    for y in 0..3 {
                        if c[y] != 0.0 {
                            pi[x][y] += n * (c[x] * c[y]);
                        }
                    }
                }
            }
            let mut qn = S::zero();
            for row in &pi {
                for v in row {
                    qn += *v * *v;
                }
            }
            let delta = l.norm_floor;
            let root = (qn + delta * delta).sqrt();
            let pn = root - delta;
            let k = 18.0 * std::f64::consts::SQRT_2 * l.constant * l.constant;
            let tau0 = S::one() / p.omega;
            let r = (tau0 * tau0 + pn * k / rho).sqrt();
            let tau_e = (tau0 + r) * 0.5;
            let tau_e_bar = -omega_eff_bar / (tau_e * tau_e);
            let r_bar = tau_e_bar * 0.5;
            let tau0_bar = tau_e_bar * 0.5 + r_bar * tau0 / r;
            let inner_bar = r_bar / (r * 2.0);
            let p_bar = inner_bar * k / rho;
            rho_bar -= inner_bar * pn * k / (rho * rho);
            let qn_bar = p_bar / (root * 2.0);
            let mut n_bar = [S::zero(); Q];
            for i in 0..Q {
                let c = L::CF[i];
                let mut acc = S::zero();
                for x in 0..3 {
                    if c[x] == 0.0 {
                        continue;
                    }
                    for y in 0..3 {
                        if c[y] != 0.0 {
                            acc += pi[x][y] * (c[x] * c[y]);
                        }
                    }
                }
                n_bar[i] = acc * qn_bar * 2.0;
                f_bar[i] += n_bar[i];
            }
            let neg: [S; Q] = std::array::from_fn(|i| -n_bar[i]);
            equilibrium_vjp::<S, Q, L>(rho, u, &neg, &mut rho_bar, &mut u_bar);
            -tau0_bar / (p.omega * p.omega)
        }
    };

    let mut j_bar = [S::zero(); 3];
    match brinkman {
        Some((mut alpha_bar, mut gamma_bar, partials, den, sv, gamma_val)) => {
            let s = solid.map_or([S::zero(); 3], |s| s.m);
            for d in 0..3 {
                let s_bar = u_bar[d] / den;
                alpha_bar -= u_bar[d] * sv[d] / (den * den) * 0.5;
                j_bar[d] = s_bar / rho;
                rho_bar -= s_bar * j[d] / (rho * rho);
                accel_bar[d] += s_bar * 0.5;
                gamma_bar += s_bar * s[d] * 0.5;
                m_bar[d] += s_bar * gamma_val * 0.5;
            }
            d_bar += alpha_bar * partials[0] + gamma_bar * partials[2];
            c_alpha_bar += alpha_bar * partials[1] + gamma_bar * partials[3];
        }
        None => {
            for d in 0..3 {
                j_bar[d] = u_bar[d] / rho;
                rho_bar -= u_bar[d] * j[d] / (rho * rho);
                accel_bar[d] += u_bar[d] * 0.5;
            }
        }
    }
    for i in 0..Q {
        let c = L::CF[i];
        let mut v = f_bar[i] + rho_bar;
        for d in 0..3 {
            if c[d] != 0.0 {
                v += j_bar[d] * c[d];
            }
        }
        f_bar[i] = v;
    }
    CellBar { f: f_bar, omega: omega_bar, accel: accel_bar, c_alpha: c_alpha_bar, d: d_bar, m: m_bar }
}

