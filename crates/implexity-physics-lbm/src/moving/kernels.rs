// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;

use super::lattice::Lattice;
use super::psm::beta;

pub(crate) const RUN: usize = 64;

#[must_use]
pub(crate) fn forward_lanes<S>() -> usize {
    (RUN * 8 / size_of::<S>().max(8)).clamp(8, RUN)
}

#[must_use]
pub(crate) fn reverse_lanes<S>() -> usize {
    (forward_lanes::<S>() / 2).max(8)
}

pub(crate) struct SolidRun<'a, S> {
    pub(crate) d: [&'a [S]; 2],
    pub(crate) m: [&'a [[S; 3]]; 2],
    pub(crate) theta: f64,
    pub(crate) kappa: f64,
    pub(crate) superposition: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Rates<S> {
    pub(crate) plus: S,
    pub(crate) minus: S,
    pub(crate) dminus: S,
}

struct SolidLanes<S> {
    d: [S; RUN],
    bod: [S; RUN],
    bod_d: [S; RUN],
    bod_t: [S; RUN],
    bb: [S; RUN],
    invd: [S; RUN],
    m: [[S; RUN]; 3],
    m2: [S; RUN],
    k1: [S; RUN],
}

impl<S: Scalar> SolidLanes<S> {
    #[inline]
    fn new(sr: &SolidRun<'_, S>, len: usize, t: S, usq: &[S; RUN], partials: bool) -> Self {
        let th = sr.theta;
        let z = S::zero();
        let mut s = Self {
            d: [z; RUN],
            bod: [z; RUN],
            bod_d: [z; RUN],
            bod_t: [z; RUN],
            bb: [z; RUN],
            invd: [z; RUN],
            m: [[z; RUN]; 3],
            m2: [z; RUN],
            k1: [z; RUN],
        };
        for k in 0..len {
            let d = sr.d[0][k] * (1.0 - th) + sr.d[1][k] * th;
            let (b, bd, bt) = beta(d, t, sr.kappa);
            s.d[k] = d;
            s.bod[k] = b;
            if partials {
                s.bod_d[k] = bd;
                s.bod_t[k] = bt;
            }
            s.bb[k] = b * d;
            s.invd[k] = S::one() / d;
            for e in 0..3 {
                s.m[e][k] = sr.m[0][k][e] * (1.0 - th) + sr.m[1][k][e] * th;
            }
            s.m2[k] = s.m[0][k] * s.m[0][k] + s.m[1][k] * s.m[1][k] + s.m[2][k] * s.m[2][k];
            s.k1[k] = (b * s.m2[k] * s.invd[k] - s.bb[k] * usq[k]) * 1.5;
        }
        s
    }
}

#[inline]
fn gather<S: Scalar, const Q: usize>(
    v: &[S],
    n: usize,
    x0: usize,
    len: usize,
    offsets: &[isize; Q],
    sign: isize,
    out: &mut [[S; RUN]; Q],
) {
    for q in 0..Q {
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
        let src = q * n + (x0 as isize + sign * offsets[q]) as usize;
        out[q][..len].copy_from_slice(&v[src..src + len]);
    }
}

struct Moments<S> {
    rho: [S; RUN],
    j: [[S; RUN]; 3],
    u: [[S; RUN]; 3],
    usq: [S; RUN],
    ua: [S; RUN],
}

#[inline]
fn macroscopic<S: Scalar, const Q: usize, L: Lattice<Q>>(
    f: &[[S; RUN]; Q],
    len: usize,
    accel: [S; 3],
) -> Moments<S> {
    let z = S::zero();
    let mut mo = Moments { rho: [z; RUN], j: [[z; RUN]; 3], u: [[z; RUN]; 3], usq: [z; RUN], ua: [z; RUN] };
    for q in 0..Q {
        let c = L::CF[q];
        for k in 0..len {
            mo.rho[k] += f[q][k];
            mo.j[0][k] += f[q][k] * c[0];
            mo.j[1][k] += f[q][k] * c[1];
            mo.j[2][k] += f[q][k] * c[2];
        }
    }
    for k in 0..len {
        for d in 0..3 {
            mo.u[d][k] = mo.j[d][k] / mo.rho[k] + accel[d] * 0.5;
        }
        mo.usq[k] = mo.u[0][k] * mo.u[0][k] + mo.u[1][k] * mo.u[1][k] + mo.u[2][k] * mo.u[2][k];
        mo.ua[k] = mo.u[0][k] * accel[0] + mo.u[1][k] * accel[1] + mo.u[2][k] * accel[2];
    }
    mo
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines, clippy::needless_range_loop)]
#[inline]
pub(crate) fn forward_run<S: Scalar, const Q: usize, L: Lattice<Q>>(
    g: &[S],
    n: usize,
    x0: usize,
    len: usize,
    offsets: &[isize; Q],
    rates: Rates<S>,
    accel: [S; 3],
    out: &mut [&mut [S]],
    local0: usize,
    rho_out: Option<&mut [S]>,
    u_out: Option<&mut [[S; 3]]>,
    solid: Option<(&SolidRun<'_, S>, &mut [[S; 3]])>,
) -> f64 {
    let z = S::zero();
    let (omega, om) = (rates.plus, rates.minus);
    let mut f = [[z; RUN]; Q];
    gather::<S, Q>(g, n, x0, len, offsets, -1, &mut f);
    let mo = macroscopic::<S, Q, L>(&f, len, accel);
    let (rho, u, usq, ua) = (&mo.rho, &mo.u, &mo.usq, &mo.ua);
    let mut vmax: f64 = 0.0;
    for k in 0..len {
        vmax = vmax.max(usq[k].value());
    }
    let mut post = [[z; RUN]; Q];
    let kp = S::one() - omega * 0.5;
    let km = S::one() - om * 0.5;
    let w0 = L::W[0];
    for k in 0..len {
        let eq0 = rho[k] * w0 * (S::one() - usq[k] * 1.5);
        post[0][k] = f[0][k] - omega * (f[0][k] - eq0) - kp * (rho[k] * ua[k]) * (3.0 * w0);
    }
    for i in 1..Q {
        let o = L::OPPOSITE[i];
        if o < i {
            continue;
        }
        let w = L::W[i];
        let c = L::CF[i];
        let ca = accel[0] * c[0] + accel[1] * c[1] + accel[2] * c[2];
        for k in 0..len {
            let cu = u[0][k] * c[0] + u[1][k] * c[1] + u[2][k] * c[2];
            let cf = rho[k] * ca;
            let wr = rho[k] * w;
            let eqp = wr * (S::one() - usq[k] * 1.5 + cu * cu * 4.5);
            let eqm = wr * cu * 3.0;
            let sp = (cu * cf * 9.0 - rho[k] * ua[k] * 3.0) * w;
            let sm = cf * (3.0 * w);
            let fp = (f[i][k] + f[o][k]) * 0.5;
            let fm = (f[i][k] - f[o][k]) * 0.5;
            let a = kp * sp - omega * (fp - eqp);
            let b = km * sm - om * (fm - eqm);
            post[i][k] = f[i][k] + a + b;
            post[o][k] = f[o][k] + a - b;
        }
    }
    if let Some((sr, dp)) = solid {
        let t = S::one() / omega - 0.5;
        let s = SolidLanes::new(sr, len, t, usq, false);
        for k in 0..len {
            let o0 = -(rho[k] * s.k1[k] * w0);
            post[0][k] = if sr.superposition {
                post[0][k] + o0
            } else {
                f[0][k] + (S::one() - s.bb[k]) * (post[0][k] - f[0][k]) + o0
            };
        }
        for i in 1..Q {
            let o = L::OPPOSITE[i];
            if o < i {
                continue;
            }
            let w = L::W[i];
            let c = L::CF[i];
            if sr.superposition {
                for k in 0..len {
                    let cu = u[0][k] * c[0] + u[1][k] * c[1] + u[2][k] * c[2];
                    let cm = s.m[0][k] * c[0] + s.m[1][k] * c[1] + s.m[2][k] * c[2];
                    let wr = rho[k] * w;
                    let e = wr * ((s.bod[k] * cm * cm * s.invd[k] - s.bb[k] * cu * cu) * 4.5 - s.k1[k]);
                    let ob = wr * (s.bod[k] * cm - s.bb[k] * cu) * 3.0;
                    post[i][k] += e + ob;
                    post[o][k] += e - ob;
                }
            } else {
                for k in 0..len {
                    let cu = u[0][k] * c[0] + u[1][k] * c[1] + u[2][k] * c[2];
                    let cm = s.m[0][k] * c[0] + s.m[1][k] * c[1] + s.m[2][k] * c[2];
                    let wr = rho[k] * w;
                    let bb = s.bb[k];
                    let e = wr * ((s.bod[k] * cm * cm * s.invd[k] - bb * cu * cu) * 4.5 - s.k1[k]);
                    let ob = wr * (s.bod[k] * cm + bb * cu) * 3.0;
                    let nb = bb * (f[o][k] - f[i][k]);
                    post[i][k] = f[i][k] + (S::one() - bb) * (post[i][k] - f[i][k]) + nb + e + ob;
                    post[o][k] = f[o][k] + (S::one() - bb) * (post[o][k] - f[o][k]) - nb + e - ob;
                }
            }
        }

        for k in 0..len {
            let r = rho[k];
            dp[k] = std::array::from_fn(|e| {
                if e >= L::DIMENSIONS {
                    z
                } else if sr.superposition {
                    r * (s.bod[k] * s.m[e][k] - s.bb[k] * u[e][k])
                } else {
                    r * (s.bod[k] * s.m[e][k] + s.bb[k] * u[e][k]) - s.bb[k] * mo.j[e][k] * 2.0
                }
            });
        }
    }
    for q in 0..Q {
        out[q][local0..local0 + len].copy_from_slice(&post[q][..len]);
    }
    if let Some(r) = rho_out {
        r[local0..local0 + len].copy_from_slice(&rho[..len]);
    }
    if let Some(uo) = u_out {
        for k in 0..len {
            uo[local0 + k] = [u[0][k], u[1][k], u[2][k]];
        }
    }
    vmax
}

pub(crate) struct ReverseSolid<'a, S> {
    pub(crate) run: SolidRun<'a, S>,
    pub(crate) gk: [&'a [[S; 3]]; 2],
    pub(crate) dp_bar: [S; 3],
    pub(crate) m_bar: &'a mut [[S; 3]],
    pub(crate) d_bar: &'a mut [S],
}

#[derive(Clone, Copy)]
pub(crate) enum CotangentSource<'a, S> {
    Direct(&'a [S]),
    Pulled(&'a [S]),
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::needless_range_loop,
    clippy::type_complexity
)]
#[inline]
pub(crate) fn reverse_run<S: Scalar, const Q: usize, L: Lattice<Q>>(
    g: &[S],
    n: usize,
    x0: usize,
    len: usize,
    offsets: &[isize; Q],
    source: CotangentSource<'_, S>,
    rates: Rates<S>,
    accel: [S; 3],
    external: Option<(&[S], &[[S; 3]])>,
    solid: Option<ReverseSolid<'_, S>>,
    out: &mut [&mut [S]],
    local0: usize,
) -> (S, [S; 3]) {
    let z = S::zero();
    let (omega, om, dom) = (rates.plus, rates.minus, rates.dminus);
    let mut f = [[z; RUN]; Q];
    gather::<S, Q>(g, n, x0, len, offsets, -1, &mut f);
    let mut gb = [[z; RUN]; Q];
    match source {
        CotangentSource::Direct(v) => gather::<S, Q>(v, n, x0, len, offsets, 0, &mut gb),
        CotangentSource::Pulled(v) => gather::<S, Q>(v, n, x0, len, offsets, 1, &mut gb),
    }
    let mo = macroscopic::<S, Q, L>(&f, len, accel);
    let (rho, j, u, usq, ua) = (&mo.rho, &mo.j, &mo.u, &mo.usq, &mo.ua);
    let kp = S::one() - omega * 0.5;
    let km = S::one() - om * 0.5;

    let mut fb = [[z; RUN]; Q];
    let mut rb = [z; RUN];
    let mut ub = [[z; RUN]; 3];
    let mut usqb = [z; RUN];
    let mut ufb = [z; RUN];
    let mut fcb = [[z; RUN]; 3];
    let mut omb = [z; RUN];
    let mut ommb = [z; RUN];
    if let Some((r, v)) = external {
        for k in 0..len {
            rb[k] = r[x0 + k];
            for d in 0..3 {
                ub[d][k] = v[x0 + k][d];
            }
        }
    }

    let w0 = L::W[0];
    for k in 0..len {
        let g0 = gb[0][k];
        let base = S::one() - usq[k] * 1.5;
        let eq0 = rho[k] * w0 * base;
        let uf = rho[k] * ua[k];
        let s0 = -(uf * (3.0 * w0));
        fb[0][k] = g0 * (S::one() - omega);
        rb[k] += g0 * omega * w0 * base;
        usqb[k] -= g0 * omega * rho[k] * (1.5 * w0);
        ufb[k] -= g0 * kp * (3.0 * w0);
        omb[k] -= g0 * ((f[0][k] - eq0) + s0 * 0.5);
    }
    for i in 1..Q {
        let o = L::OPPOSITE[i];
        if o < i {
            continue;
        }
        let w = L::W[i];
        let c = L::CF[i];
        let ca = accel[0] * c[0] + accel[1] * c[1] + accel[2] * c[2];
        for k in 0..len {
            let base = S::one() - usq[k] * 1.5;
            let cu = u[0][k] * c[0] + u[1][k] * c[1] + u[2][k] * c[2];
            let cf = rho[k] * ca;
            let wr = rho[k] * w;
            let uf = rho[k] * ua[k];
            let eqp = wr * (base + cu * cu * 4.5);
            let eqm = wr * cu * 3.0;
            let sp = (cu * cf * 9.0 - uf * 3.0) * w;
            let sm = cf * (3.0 * w);
            let fp = (f[i][k] + f[o][k]) * 0.5;
            let fm = (f[i][k] - f[o][k]) * 0.5;
            let a_bar = gb[i][k] + gb[o][k];
            let b_bar = gb[i][k] - gb[o][k];
            omb[k] -= a_bar * ((fp - eqp) + sp * 0.5);
            ommb[k] -= b_bar * ((fm - eqm) + sm * 0.5);
            let fp_bar = -(omega * a_bar);
            let fm_bar = -(om * b_bar);
            let eqp_bar = omega * a_bar;
            let eqm_bar = om * b_bar;
            let sp_bar = kp * a_bar;
            let sm_bar = km * b_bar;
            fb[i][k] = gb[i][k] + (fp_bar + fm_bar) * 0.5;
            fb[o][k] = gb[o][k] + (fp_bar - fm_bar) * 0.5;
            rb[k] += eqp_bar * w * (base + cu * cu * 4.5) + eqm_bar * cu * (3.0 * w);
            usqb[k] -= eqp_bar * wr * 1.5;
            let cu_bar = eqp_bar * wr * cu * 9.0 + eqm_bar * wr * 3.0 + sp_bar * cf * (9.0 * w);
            let cf_bar = sp_bar * cu * (9.0 * w) + sm_bar * (3.0 * w);
            ufb[k] -= sp_bar * (3.0 * w);
            for d in 0..3 {
                ub[d][k] += cu_bar * c[d];
                fcb[d][k] += cf_bar * c[d];
            }
        }
    }
    for k in 0..len {
        let force = [rho[k] * accel[0], rho[k] * accel[1], rho[k] * accel[2]];
        for d in 0..3 {
            ub[d][k] += usqb[k] * u[d][k] * 2.0 + ufb[k] * force[d];
            fcb[d][k] += ufb[k] * u[d][k];
        }
    }
    let mut accel_bar = [z; 3];
    let mut omega_bar = z;
    if let Some(rs) = solid {
        let sr = &rs.run;
        let t = S::one() / omega - 0.5;
        let s = SolidLanes::new(sr, len, t, usq, true);
        let th = sr.theta;

        let mut dpb = [[z; RUN]; 3];
        let mut extra_d = [z; RUN];
        for k in 0..len {
            for e in 0..3 {
                let gt = rs.gk[0][k][e] * (1.0 - th) + rs.gk[1][k][e] * th;
                let dp =
                    if e < L::DIMENSIONS { rho[k] * (s.bod[k] * s.m[e][k] - s.bb[k] * u[e][k]) } else { z };
                dpb[e][k] = rs.dp_bar[e] - gt * s.invd[k];
                extra_d[k] += gt * dp * s.invd[k] * s.invd[k];
            }
        }
        let mut bodb = [z; RUN];
        let mut bbb = [z; RUN];
        let mut db = [z; RUN];
        let mut mb = [[z; RUN]; 3];
        let mut sw = [z; RUN];
        for k in 0..len {
            let h0 = gb[0][k];
            sw[k] = h0 * w0;
            rb[k] -= h0 * w0 * s.k1[k];
        }
        for i in 1..Q {
            let o = L::OPPOSITE[i];
            if o < i {
                continue;
            }
            let w = L::W[i];
            let c = L::CF[i];
            for k in 0..len {
                let hc = dpb[0][k] * c[0] + dpb[1][k] * c[1] + dpb[2][k] * c[2];
                let he = gb[i][k] + gb[o][k];
                let ho = gb[i][k] - gb[o][k] + hc * 2.0;
                let cu = u[0][k] * c[0] + u[1][k] * c[1] + u[2][k] * c[2];
                let cm = s.m[0][k] * c[0] + s.m[1][k] * c[1] + s.m[2][k] * c[2];
                let wr = rho[k] * w;
                let (bod, bb, invd) = (s.bod[k], s.bb[k], s.invd[k]);
                sw[k] += he * w;
                rb[k] += he * w * ((bod * cm * cm * invd - bb * cu * cu) * 4.5 - s.k1[k])
                    + ho * w * (bod * cm - bb * cu) * 3.0;
                bodb[k] += he * wr * cm * cm * invd * 4.5 + ho * wr * cm * 3.0;
                bbb[k] -= he * wr * cu * cu * 4.5 + ho * wr * cu * 3.0;
                db[k] -= he * wr * bod * cm * cm * invd * invd * 4.5;
                let cm_bar = he * wr * bod * cm * invd * 9.0 + ho * wr * bod * 3.0;
                let cu_bar = -(he * wr * bb * cu * 9.0 + ho * wr * bb * 3.0);
                for d in 0..3 {
                    mb[d][k] += cm_bar * c[d];
                    ub[d][k] += cu_bar * c[d];
                }
            }
        }
        for k in 0..len {
            let k1b = -(rho[k] * sw[k]);
            let (bod, bb, invd) = (s.bod[k], s.bb[k], s.invd[k]);
            bodb[k] += k1b * s.m2[k] * invd * 1.5;
            bbb[k] -= k1b * usq[k] * 1.5;
            db[k] -= k1b * bod * s.m2[k] * invd * invd * 1.5;
            for d in 0..3 {
                mb[d][k] += k1b * bod * s.m[d][k] * invd * 3.0;
                ub[d][k] -= k1b * bb * u[d][k] * 3.0;
            }
            bodb[k] += bbb[k] * s.d[k];
            db[k] += bbb[k] * bod + bodb[k] * s.bod_d[k];
            let t_bar = bodb[k] * s.bod_t[k];
            omega_bar -= t_bar / (omega * omega);
            rs.m_bar[k] = [mb[0][k], mb[1][k], mb[2][k]];
            rs.d_bar[k] = db[k] + extra_d[k];
        }
    }
    for k in 0..len {
        omega_bar += omb[k] + ommb[k] * dom;
        let r = rho[k];
        for d in 0..3 {
            rb[k] += fcb[d][k] * accel[d];
            accel_bar[d] += fcb[d][k] * r + ub[d][k] * 0.5;
        }
        let jb = [ub[0][k] / r, ub[1][k] / r, ub[2][k] / r];
        rb[k] -= (ub[0][k] * j[0][k] + ub[1][k] * j[1][k] + ub[2][k] * j[2][k]) / (r * r);
        for q in 0..Q {
            let c = L::CF[q];
            out[q][local0 + k] = fb[q][k] + rb[k] + jb[0] * c[0] + jb[1] * c[1] + jb[2] * c[2];
        }
    }
    (omega_bar, accel_bar)
}
