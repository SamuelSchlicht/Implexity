// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_ad::{Dual, Scalar};

use super::equilibrium::{Lattice, LatticeData, equilibrium, macroscopic, moment_residual};

pub type Moments = (f64, [f64; 3], f64, f64);

pub fn macroscopic_vjp(
    f: &[f64],
    g: &[f64],
    gamma: f64,
    data: &LatticeData,
    bars: (f64, [f64; 3], f64, f64),
    f_bar: &mut [f64],
    g_bar: &mut [f64],
) {
    let (rho, u, _t, e) = macroscopic(f, g, gamma, data);
    let (mut rho_b, mut u_b, t_b, mut e_b) = bars;
    let k = gamma - 1.0;
    e_b += t_b * k / rho;
    rho_b -= t_b * k * e / (rho * rho);
    for d in 0..3 {
        u_b[d] -= t_b * k * u[d];
    }
    let m_b: [f64; 3] = std::array::from_fn(|d| u_b[d] / rho);
    for d in 0..3 {
        rho_b -= u_b[d] * u[d] / rho;
    }
    for (i, fb) in f_bar.iter_mut().enumerate() {
        let c = data.velocities[i];
        let mut v = rho_b + 0.5 * e_b * data.speed2[i];
        for d in 0..3 {
            if c[d] != 0 {
                v += m_b[d] * f64::from(c[d]);
            }
        }
        *fb += v;
    }
    for gb in g_bar.iter_mut() {
        *gb += 0.5 * e_b;
    }
}

#[must_use]
pub fn sensor_multiplier(sensor: f64, tau: f64) -> f64 {
    if sensor < 0.01 {
        1.0
    } else if sensor < 0.1 {
        1.05
    } else if sensor < 1.0 {
        1.35
    } else {
        1.0 / tau
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CollisionDiagnostics {
    pub kinetic_sensor: f64,
    pub effective_relaxation_time: f64,
    pub positivity_limited: bool,
    pub distance_to_positivity_switch: f64,
    pub equilibrium_residual: f64,
    pub distance_to_sensor_switch: f64,
}

struct RateInfo {
    rate: f64,
    active: bool,
    distance: f64,
    limiter: Option<(usize, f64)>,
    requested: f64,
    limit: f64,
}

fn positive_rate(f: &[f64], g: &[f64], feq: &[f64], geq: &[f64], requested: f64) -> RateInfo {
    let q = f.len();
    let ratio = |i: usize| {
        let (s, d) = if i < q { (f[i], f[i] - feq[i]) } else { (g[i - q], g[i - q] - geq[i - q]) };
        let r = if d > 0.0 { s / d } else { 2.0 };
        r.min(2.0)
    };
    let mut first = 0;
    let mut best = f64::INFINITY;
    for i in 0..2 * q {
        let r = ratio(i);
        if r < best {
            best = r;
            first = i;
        }
    }
    let limit = best * (1.0 - 1e-12);
    let mut second = f64::INFINITY;
    for i in 0..2 * q {
        let r = if i == first { 2.0 } else { ratio(i) };
        second = second.min(r);
    }
    let active = requested > limit;
    let distance = (requested - limit).abs().min(if active { second - best } else { 2.0 });
    let (s, d) = if first < q {
        (f[first], f[first] - feq[first])
    } else {
        (g[first - q], g[first - q] - geq[first - q])
    };
    let limiter = (d > 0.0 && s / d < 2.0).then_some((first, s / d));
    RateInfo { rate: requested.min(limit), active, distance, limiter, requested, limit }
}

#[must_use]
pub fn stabilized_collision(
    f: &[f64],
    g: &[f64],
    gamma: f64,
    tau: f64,
    lattice: Lattice,
) -> (Vec<f64>, Vec<f64>, CollisionDiagnostics) {
    let data = lattice.data();
    let (rho, u, t, _) = macroscopic(f, g, gamma, data);
    let (feq, geq) = equilibrium(rho, u, t, gamma, lattice);
    let q = f.len();
    let sensor = (0..q).map(|i| (f[i] - feq[i]).abs() / feq[i]).sum::<f64>() / q as f64;
    let effective = tau * sensor_multiplier(sensor, tau);
    let info = positive_rate(f, g, &feq, &geq, 1.0 / effective);
    let rate = info.rate;
    let fo: Vec<f64> = (0..q).map(|i| f[i] + rate * (feq[i] - f[i])).collect();
    let go: Vec<f64> = (0..q).map(|i| g[i] + rate * (geq[i] - g[i])).collect();
    let switch = [0.01, 0.1, 1.0].iter().fold(f64::INFINITY, |m, s| m.min((sensor - s).abs()));
    let diagnostics = CollisionDiagnostics {
        kinetic_sensor: sensor,
        effective_relaxation_time: 1.0 / rate,
        positivity_limited: info.active,
        distance_to_positivity_switch: info.distance,
        equilibrium_residual: moment_residual(&feq, rho, u, t, lattice),
        distance_to_sensor_switch: info.distance.min(switch),
    };
    (fo, go, diagnostics)
}

fn equilibrium_jacobian(moments: Moments, gamma: f64, lattice: Lattice) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let (rho, u, t, _) = moments;
    let seed = |v: f64, k: usize| Dual::<5>::variable(v, k);
    let (fd, gd) =
        equilibrium(seed(rho, 0), [seed(u[0], 1), seed(u[1], 2), seed(u[2], 3)], seed(t, 4), gamma, lattice);
    let fv = fd.iter().map(|d| d.re).collect();
    let gv = gd.iter().map(|d| d.re).collect();
    let mut jac = Vec::with_capacity(10 * fd.len());
    for d in fd.iter().chain(&gd) {
        jac.extend_from_slice(&d.eps);
    }
    (fv, gv, jac)
}

#[must_use]
pub fn stabilized_collision_vjp(
    f: &[f64],
    g: &[f64],
    gamma: f64,
    tau: f64,
    lattice: Lattice,
    fo_bar: &[f64],
    go_bar: &[f64],
) -> (Vec<f64>, Vec<f64>, f64) {
    let data = lattice.data();
    let q = f.len();
    let moments = macroscopic(f, g, gamma, data);
    let (feq, geq, jac) = equilibrium_jacobian(moments, gamma, lattice);
    let sensor = (0..q).map(|i| (f[i] - feq[i]).abs() / feq[i]).sum::<f64>() / q as f64;
    let multiplier = sensor_multiplier(sensor, tau);
    let effective = tau * multiplier;
    let info = positive_rate(f, g, &feq, &geq, 1.0 / effective);
    let rate = info.rate;
    let mut f_bar: Vec<f64> = fo_bar.iter().map(|b| b * (1.0 - rate)).collect();
    let mut g_bar: Vec<f64> = go_bar.iter().map(|b| b * (1.0 - rate)).collect();
    let mut feq_bar: Vec<f64> = fo_bar.iter().map(|b| b * rate).collect();
    let mut geq_bar: Vec<f64> = go_bar.iter().map(|b| b * rate).collect();
    let mut rate_bar = 0.0;
    for i in 0..q {
        rate_bar += fo_bar[i] * (feq[i] - f[i]) + go_bar[i] * (geq[i] - g[i]);
    }
    let (w_req, w_lim) = if info.requested < info.limit {
        (1.0, 0.0)
    } else if info.requested > info.limit {
        (0.0, 1.0)
    } else {
        (0.5, 0.5)
    };
    let mut tau_bar = 0.0;
    if w_req != 0.0 && sensor < 1.0 {
        tau_bar += w_req * rate_bar * (-multiplier / (effective * effective));
    }
    if w_lim != 0.0
        && let Some((k, _)) = info.limiter
    {
        let gl = w_lim * rate_bar * (1.0 - 1e-12);
        let (s, eq) = if k < q { (f[k], feq[k]) } else { (g[k - q], geq[k - q]) };
        let d = s - eq;
        let s_bar = gl / d;
        let d_bar = -gl * s / (d * d);
        if k < q {
            f_bar[k] += s_bar + d_bar;
            feq_bar[k] -= d_bar;
        } else {
            g_bar[k - q] += s_bar + d_bar;
            geq_bar[k - q] -= d_bar;
        }
    }
    let mut s_bar = [0.0; 5];
    for (row, b) in feq_bar.iter().chain(&geq_bar).enumerate() {
        if *b != 0.0 {
            for k in 0..5 {
                s_bar[k] += jac[row * 5 + k] * b;
            }
        }
    }
    macroscopic_vjp(
        f,
        g,
        gamma,
        data,
        (s_bar[0], [s_bar[1], s_bar[2], s_bar[3]], s_bar[4], 0.0),
        &mut f_bar,
        &mut g_bar,
    );
    (f_bar, g_bar, tau_bar)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DragTransfer {
    pub solid_impulse: [f64; 3],
    pub gas_dissipation_heat: f64,
    pub solid_dissipation_heat: f64,
    pub equilibrium_residual: f64,
}

fn drag_terms<S: Scalar>(
    s: &[S; 6],
    gamma: f64,
    fraction: f64,
    lattice: Lattice,
) -> (Vec<S>, Vec<S>, [S; 4], [S; 3], S) {
    let (rho, u, t, exposure) = (s[0], [s[1], s[2], s[3]], s[4], s[5]);
    let decay = (-exposure).exp();
    let uu = u[0] * u[0] + u[1] * u[1] + u[2] * u[2];
    let loss = rho * 0.5 * uu * (-(exposure * -2.0).exp_m1());
    let updated_u = [u[0] * decay, u[1] * decay, u[2] * decay];
    let updated_t = t + loss * ((gamma - 1.0) * fraction) / rho;
    let (old_f, old_g) = equilibrium(rho, u, t, gamma, lattice);
    let (new_f, new_g) = equilibrium(rho, updated_u, updated_t, gamma, lattice);
    let df = new_f.iter().zip(&old_f).map(|(a, b)| *a - *b).collect();
    let dg = new_g.iter().zip(&old_g).map(|(a, b)| *a - *b).collect();
    let impulse = [rho * (u[0] - updated_u[0]), rho * (u[1] - updated_u[1]), rho * (u[2] - updated_u[2])];
    (df, dg, [impulse[0], impulse[1], impulse[2], loss * (1.0 - fraction)], updated_u, updated_t)
}

#[must_use]
pub fn porous_drag(
    f: &[f64],
    g: &[f64],
    gamma: f64,
    exposure: f64,
    fraction: f64,
    lattice: Lattice,
) -> (Vec<f64>, Vec<f64>, DragTransfer) {
    let data = lattice.data();
    let (rho, u, t, _) = macroscopic(f, g, gamma, data);
    let decay = (-exposure).exp();
    let loss = 0.5 * rho * (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]) * (-(-2.0 * exposure).exp_m1());
    let updated_u = u.map(|v| v * decay);
    let updated_t = t + (gamma - 1.0) * fraction * loss / rho;
    let (old_f, old_g) = equilibrium(rho, u, t, gamma, lattice);
    let (new_f, new_g) = equilibrium(rho, updated_u, updated_t, gamma, lattice);
    let residual = moment_residual(&old_f, rho, u, t, lattice)
        .max(moment_residual(&new_f, rho, updated_u, updated_t, lattice));
    let fo = (0..f.len()).map(|i| f[i] + new_f[i] - old_f[i]).collect();
    let go = (0..g.len()).map(|i| g[i] + new_g[i] - old_g[i]).collect();
    (
        fo,
        go,
        DragTransfer {
            solid_impulse: std::array::from_fn(|d| rho * (u[d] - updated_u[d])),
            gas_dissipation_heat: fraction * loss,
            solid_dissipation_heat: (1.0 - fraction) * loss,
            equilibrium_residual: residual,
        },
    )
}

#[must_use]
pub fn porous_drag_vjp(
    f: &[f64],
    g: &[f64],
    gamma: f64,
    exposure: f64,
    fraction: f64,
    lattice: Lattice,
    fo_bar: &[f64],
    go_bar: &[f64],
    impulse_bar: [f64; 3],
    solid_heat_bar: f64,
) -> (Vec<f64>, Vec<f64>, f64) {
    let data = lattice.data();
    let (rho, u, t, _) = macroscopic(f, g, gamma, data);
    let seeds: [Dual<6>; 6] =
        std::array::from_fn(|k| Dual::variable([rho, u[0], u[1], u[2], t, exposure][k], k));
    let (df, dg, extra, _, _) = drag_terms(&seeds, gamma, fraction, lattice);
    let mut s_bar = [0.0; 6];
    for (d, b) in df.iter().zip(fo_bar).chain(dg.iter().zip(go_bar)) {
        if *b != 0.0 {
            for k in 0..6 {
                s_bar[k] += d.eps[k] * b;
            }
        }
    }
    let extra_bar = [impulse_bar[0], impulse_bar[1], impulse_bar[2], solid_heat_bar];
    for (d, b) in extra.iter().zip(extra_bar) {
        if b != 0.0 {
            for k in 0..6 {
                s_bar[k] += d.eps[k] * b;
            }
        }
    }
    let mut f_bar = fo_bar.to_vec();
    let mut g_bar = go_bar.to_vec();
    macroscopic_vjp(
        f,
        g,
        gamma,
        data,
        (s_bar[0], [s_bar[1], s_bar[2], s_bar[3]], s_bar[4], 0.0),
        &mut f_bar,
        &mut g_bar,
    );
    (f_bar, g_bar, s_bar[5])
}

#[must_use]
pub fn prescribed_heat(
    f: &[f64],
    g: &[f64],
    heat: f64,
    gamma: f64,
    lattice: Lattice,
) -> (Vec<f64>, Vec<f64>, f64) {
    let data = lattice.data();
    let (rho, u, t, _) = macroscopic(f, g, gamma, data);
    let new_t = t + heat * (gamma - 1.0) / rho;
    let (old_f, old_g) = equilibrium(rho, u, t, gamma, lattice);
    let (new_f, new_g) = equilibrium(rho, u, new_t, gamma, lattice);
    let residual =
        moment_residual(&old_f, rho, u, t, lattice).max(moment_residual(&new_f, rho, u, new_t, lattice));
    (
        (0..f.len()).map(|i| f[i] + new_f[i] - old_f[i]).collect(),
        (0..g.len()).map(|i| g[i] + new_g[i] - old_g[i]).collect(),
        residual,
    )
}

#[must_use]
pub fn prescribed_heat_vjp(
    f: &[f64],
    g: &[f64],
    heat: f64,
    gamma: f64,
    lattice: Lattice,
    fo_bar: &[f64],
    go_bar: &[f64],
) -> (Vec<f64>, Vec<f64>) {
    let data = lattice.data();
    let (rho, u, t, _) = macroscopic(f, g, gamma, data);
    let s: [Dual<5>; 5] = std::array::from_fn(|k| Dual::variable([rho, u[0], u[1], u[2], t][k], k));
    let ud = [s[1], s[2], s[3]];
    let new_t = s[4] + s[0].recip() * (heat * (gamma - 1.0));
    let (old_f, old_g) = equilibrium(s[0], ud, s[4], gamma, lattice);
    let (new_f, new_g) = equilibrium(s[0], ud, new_t, gamma, lattice);
    let mut s_bar = [0.0; 5];
    let rows = new_f.iter().zip(&old_f).zip(fo_bar).chain(new_g.iter().zip(&old_g).zip(go_bar));
    for ((a, b), bar) in rows {
        if *bar != 0.0 {
            for k in 0..5 {
                s_bar[k] += (a.eps[k] - b.eps[k]) * bar;
            }
        }
    }
    let mut f_bar = fo_bar.to_vec();
    let mut g_bar = go_bar.to_vec();
    macroscopic_vjp(
        f,
        g,
        gamma,
        data,
        (s_bar[0], [s_bar[1], s_bar[2], s_bar[3]], s_bar[4], 0.0),
        &mut f_bar,
        &mut g_bar,
    );
    (f_bar, g_bar)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Exchange {
    pub heat_to_solid: f64,
    pub gas_temperature: f64,
    pub equilibrium_residual: f64,
}

fn exchange_terms<S: Scalar>(
    s: &[S; 5],
    ts: S,
    cs: f64,
    h: f64,
    gamma: f64,
    lattice: Lattice,
) -> (Vec<S>, Vec<S>, S, S, S) {
    let (rho, u, t) = (s[0], [s[1], s[2], s[3]], s[4]);
    let gas_capacity = rho / (gamma - 1.0);
    let relaxed = -((gas_capacity.recip() + 1.0 / cs) * -h).exp_m1();
    let heat = gas_capacity * cs / (gas_capacity + cs) * (t - ts) * relaxed;
    let new_t = t - heat / gas_capacity;
    let new_ts = ts + heat / cs;
    let (old_f, old_g) = equilibrium(rho, u, t, gamma, lattice);
    let (new_f, new_g) = equilibrium(rho, u, new_t, gamma, lattice);
    let df = new_f.iter().zip(&old_f).map(|(a, b)| *a - *b).collect();
    let dg = new_g.iter().zip(&old_g).map(|(a, b)| *a - *b).collect();
    (df, dg, new_ts, heat, new_t)
}

#[must_use]
pub fn gas_solid_exchange(
    f: &[f64],
    g: &[f64],
    ts: f64,
    cs: f64,
    h: f64,
    gamma: f64,
    lattice: Lattice,
) -> (Vec<f64>, Vec<f64>, f64, Exchange) {
    let data = lattice.data();
    let (rho, u, t, _) = macroscopic(f, g, gamma, data);
    let gas_capacity = rho / (gamma - 1.0);
    let relaxed = -(-h * (1.0 / gas_capacity + 1.0 / cs)).exp_m1();
    let heat = gas_capacity * cs / (gas_capacity + cs) * (t - ts) * relaxed;
    let new_t = t - heat / gas_capacity;
    let new_ts = ts + heat / cs;
    let (old_f, old_g) = equilibrium(rho, u, t, gamma, lattice);
    let (new_f, new_g) = equilibrium(rho, u, new_t, gamma, lattice);
    let residual =
        moment_residual(&old_f, rho, u, t, lattice).max(moment_residual(&new_f, rho, u, new_t, lattice));
    (
        (0..f.len()).map(|i| f[i] + new_f[i] - old_f[i]).collect(),
        (0..g.len()).map(|i| g[i] + new_g[i] - old_g[i]).collect(),
        new_ts,
        Exchange { heat_to_solid: heat, gas_temperature: new_t, equilibrium_residual: residual },
    )
}

#[must_use]
pub fn gas_solid_exchange_vjp(
    f: &[f64],
    g: &[f64],
    ts: f64,
    cs: f64,
    h: f64,
    gamma: f64,
    lattice: Lattice,
    fo_bar: &[f64],
    go_bar: &[f64],
    ts_bar: f64,
) -> (Vec<f64>, Vec<f64>, f64) {
    let data = lattice.data();
    let (rho, u, t, _) = macroscopic(f, g, gamma, data);
    let s: [Dual<6>; 5] = std::array::from_fn(|k| Dual::variable([rho, u[0], u[1], u[2], t][k], k));
    let tsd = Dual::<6>::variable(ts, 5);
    let (df, dg, new_ts, _, _) = exchange_terms(&s, tsd, cs, h, gamma, lattice);
    let mut bar = [0.0; 6];
    for (d, b) in df.iter().zip(fo_bar).chain(dg.iter().zip(go_bar)) {
        if *b != 0.0 {
            for k in 0..6 {
                bar[k] += d.eps[k] * b;
            }
        }
    }
    for k in 0..6 {
        bar[k] += new_ts.eps[k] * ts_bar;
    }
    let mut f_bar = fo_bar.to_vec();
    let mut g_bar = go_bar.to_vec();
    macroscopic_vjp(
        f,
        g,
        gamma,
        data,
        (bar[0], [bar[1], bar[2], bar[3]], bar[4], 0.0),
        &mut f_bar,
        &mut g_bar,
    );
    (f_bar, g_bar, bar[5])
}

