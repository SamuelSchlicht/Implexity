// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::marker::PhantomData;

use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};

use super::lattice::{D3Q27, Lattice, dot_c, equilibrium, equilibrium_vjp};

pub const MAGIC_DEFAULT: f64 = 3.0 / 16.0;

#[derive(Clone, Debug, PartialEq)]
pub enum CollisionKind {
    Bgk,
    Trt {
        magic: f64,
    },
    Mrt {
        bulk_rate: Option<f64>,
        odd_magic: f64,
        even_rate: Option<f64>,
    },
    Regularized,
    Cumulant {
        bulk_rate: Option<f64>,
    },
}

impl CollisionKind {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Bgk => "bgk",
            Self::Trt { .. } => "trt",
            Self::Mrt { .. } => "mrt",
            Self::Regularized => "regularized",
            Self::Cumulant { .. } => "cumulant",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MomentGroup {
    Mass,
    Momentum,
    Bulk,
    Shear,
    Odd,
    EvenHigh,
}

#[derive(Clone, Debug)]
pub struct MomentBasis<const Q: usize> {
    pub rows: Vec<[f64; Q]>,
    pub norms: [f64; Q],
    pub groups: [MomentGroup; Q],
}

impl<const Q: usize> MomentBasis<Q> {

    pub fn new<L: Lattice<Q>>() -> CaeResult<Self> {
        let c = L::CF;
        let poly = |f: &dyn Fn([f64; 3]) -> f64| -> [f64; Q] { std::array::from_fn(|i| f(c[i])) };
        let mut candidates: Vec<([f64; Q], MomentGroup)> = vec![
            (poly(&|_| 1.0), MomentGroup::Mass),
            (poly(&|v| v[0]), MomentGroup::Momentum),
            (poly(&|v| v[1]), MomentGroup::Momentum),
            (poly(&|v| v[2]), MomentGroup::Momentum),
            (poly(&|v| v[0] * v[0] + v[1] * v[1] + v[2] * v[2]), MomentGroup::Bulk),
        ];
        if L::DIMENSIONS == 2 {
            candidates.push((poly(&|v| v[0] * v[0] - v[1] * v[1]), MomentGroup::Shear));
        } else {
            candidates.push((poly(&|v| 2.0 * v[0] * v[0] - v[1] * v[1] - v[2] * v[2]), MomentGroup::Shear));
            candidates.push((poly(&|v| v[1] * v[1] - v[2] * v[2]), MomentGroup::Shear));
        }
        candidates.push((poly(&|v| v[0] * v[1]), MomentGroup::Shear));
        candidates.push((poly(&|v| v[0] * v[2]), MomentGroup::Shear));
        candidates.push((poly(&|v| v[1] * v[2]), MomentGroup::Shear));
        for degree in 3..=6 {
            for a in 0..=2i32 {
                for b in 0..=2i32 {
                    let cc = degree - a - b;
                    if !(0..=2).contains(&cc) {
                        continue;
                    }
                    let group = if degree % 2 == 1 { MomentGroup::Odd } else { MomentGroup::EvenHigh };
                    candidates.push((poly(&|v| v[0].powi(a) * v[1].powi(b) * v[2].powi(cc)), group));
                }
            }
        }
        let inner = |x: &[f64; Q], y: &[f64; Q]| -> f64 { (0..Q).map(|i| L::W[i] * x[i] * y[i]).sum() };
        let mut rows: Vec<[f64; Q]> = Vec::with_capacity(Q);
        let mut norms = [0.0; Q];
        let mut groups = [MomentGroup::Mass; Q];
        for (p, group) in candidates {
            if rows.len() == Q {
                break;
            }
            let scale = inner(&p, &p);
            if scale <= 0.0 {
                continue;
            }
            let mut v = p;
            for (k, m) in rows.iter().enumerate() {
                let coef = inner(&p, m) / norms[k];
                for i in 0..Q {
                    v[i] -= coef * m[i];
                }
            }
            let n = inner(&v, &v);
            if n <= 1e-10 * scale {
                continue;
            }
            norms[rows.len()] = n;
            groups[rows.len()] = group;
            rows.push(v);
        }
        if rows.len() != Q {
            return Err(CaeError::contract(format!(
                "MRT moment basis of {} spans {} of {Q} moments",
                L::NAME,
                rows.len()
            )));
        }
        Ok(Self { rows, norms, groups })
    }

    #[inline]
    fn forward<S: Scalar>(&self, v: &[S; Q]) -> [S; Q] {
        std::array::from_fn(|k| {
            let row = &self.rows[k];
            let mut acc = S::zero();
            for i in 0..Q {
                if row[i] != 0.0 {
                    acc += v[i] * row[i];
                }
            }
            acc
        })
    }

    #[inline]
    fn inverse<S: Scalar, L: Lattice<Q>>(&self, m: &[S; Q]) -> [S; Q] {
        let scaled: [S; Q] = std::array::from_fn(|k| m[k] / self.norms[k]);
        std::array::from_fn(|i| {
            let mut acc = S::zero();
            for k in 0..Q {
                let r = self.rows[k][i];
                if r != 0.0 {
                    acc += scaled[k] * r;
                }
            }
            acc * L::W[i]
        })
    }

    #[inline]
    fn transpose<S: Scalar>(&self, m: &[S; Q]) -> [S; Q] {
        std::array::from_fn(|i| {
            let mut acc = S::zero();
            for k in 0..Q {
                let r = self.rows[k][i];
                if r != 0.0 {
                    acc += m[k] * r;
                }
            }
            acc
        })
    }
}

#[derive(Clone, Debug)]
pub struct Collision<const Q: usize, L: Lattice<Q>> {
    kind: CollisionKind,
    basis: Option<MomentBasis<Q>>,
    wick: Vec<Vec<Vec<(usize, usize)>>>,
    _lattice: PhantomData<L>,
}

fn rate_ok(r: Option<f64>) -> bool {
    r.is_none_or(|v| v.is_finite() && v > 0.0 && v < 2.0)
}

#[inline]
const fn exponents(k: usize) -> [usize; 3] {
    [k / 9, (k / 3) % 3, k % 3]
}

fn matchings(items: &[usize]) -> Vec<Vec<(usize, usize)>> {
    if items.is_empty() {
        return vec![Vec::new()];
    }
    let first = items[0];
    let mut out = Vec::new();
    for j in 1..items.len() {
        let mut rest: Vec<usize> = items[1..].to_vec();
        let partner = rest.remove(j - 1);
        for mut m in matchings(&rest) {
            m.insert(0, (first.min(partner), first.max(partner)));
            out.push(m);
        }
    }
    out
}

#[derive(Clone, Copy, Debug)]
pub struct CollisionBar<S, const Q: usize> {
    pub f: [S; Q],
    pub rho: S,
    pub u: [S; 3],
    pub force: [S; 3],
    pub omega: S,
}

impl<const Q: usize, L: Lattice<Q>> Collision<Q, L> {

    pub fn new(kind: CollisionKind) -> CaeResult<Self> {
        let mut basis = None;
        let mut wick = Vec::new();
        match &kind {
            CollisionKind::Bgk => {}
            CollisionKind::Trt { magic } => {
                if !(magic.is_finite() && *magic > 0.0) {
                    return Err(CaeError::contract("TRT magic parameter must be finite and positive"));
                }
            }
            CollisionKind::Mrt { bulk_rate, odd_magic, even_rate } => {
                if !(odd_magic.is_finite() && *odd_magic > 0.0) {
                    return Err(CaeError::contract("MRT odd_magic must be finite and positive"));
                }
                if !rate_ok(*bulk_rate) || !rate_ok(*even_rate) {
                    return Err(CaeError::contract("MRT relaxation rates must lie in (0, 2)"));
                }
                basis = Some(MomentBasis::new::<L>()?);
            }
            CollisionKind::Regularized => {
                basis = Some(MomentBasis::new::<L>()?);
            }
            CollisionKind::Cumulant { bulk_rate } => {
                if L::NAME != D3Q27::NAME || Q != 27 {
                    return Err(CaeError::contract(format!(
                        "cumulant collision requires the D3Q27 velocity set (got {})",
                        L::NAME
                    )));
                }
                if !rate_ok(*bulk_rate) {
                    return Err(CaeError::contract("cumulant bulk_rate must lie in (0, 2)"));
                }
                wick = (0..27)
                    .map(|k| {
                        let e = exponents(k);
                        let mut axes = Vec::new();
                        for (d, &n) in e.iter().enumerate() {
                            axes.extend(std::iter::repeat_n(d, n));
                        }
                        if axes.len() % 2 == 1 || axes.len() < 4 { Vec::new() } else { matchings(&axes) }
                    })
                    .collect();
            }
        }
        Ok(Self { kind, basis, wick, _lattice: PhantomData })
    }

    #[must_use]
    pub fn kind(&self) -> &CollisionKind {
        &self.kind
    }

    #[must_use]
    pub fn has_second_order(&self) -> bool {
        !matches!(self.kind, CollisionKind::Cumulant { .. })
    }

    #[inline]
    fn odd_rate<S: Scalar>(omega: S, magic: f64) -> S {
        let tp = S::one() / omega - 0.5;
        S::one() / (S::from_f64(0.5) + S::from_f64(magic) / tp)
    }

    #[inline]
    fn odd_rate_derivative<S: Scalar>(omega: S, magic: f64) -> S {
        let tau = S::one() / omega;
        let tp = tau - 0.5;
        let tm = S::from_f64(0.5) + S::from_f64(magic) / tp;
        -(tau * tau * magic) / (tm * tm * tp * tp)
    }

    #[inline]
    fn source<S: Scalar>(u: [S; 3], force: [S; 3]) -> [S; Q] {
        let uf = u[0] * force[0] + u[1] * force[1] + u[2] * force[2];
        std::array::from_fn(|i| {
            let cu = dot_c::<S, Q, L>(i, u);
            let cf = dot_c::<S, Q, L>(i, force);
            (cf * 3.0 + cu * cf * 9.0 - uf * 3.0) * L::W[i]
        })
    }

    #[inline]
    fn relax<S: Scalar>(&self, omega: S, y: &[S; Q], derivative: bool) -> ([S; Q], [S; Q]) {
        match &self.kind {
            CollisionKind::Bgk => {
                let r = std::array::from_fn(|i| y[i] * omega);
                let d = if derivative { *y } else { [S::zero(); Q] };
                (r, d)
            }
            CollisionKind::Trt { magic } => {
                let om = Self::odd_rate(omega, *magic);
                let dom = if derivative { Self::odd_rate_derivative(omega, *magic) } else { S::zero() };
                let mut r = [S::zero(); Q];
                let mut d = [S::zero(); Q];
                for i in 0..Q {
                    let o = L::OPPOSITE[i];
                    let plus = (y[i] + y[o]) * 0.5;
                    let minus = (y[i] - y[o]) * 0.5;
                    r[i] = plus * omega + minus * om;
                    if derivative {
                        d[i] = plus + minus * dom;
                    }
                }
                (r, d)
            }
            CollisionKind::Mrt { bulk_rate, odd_magic, even_rate } => {
                let Some(basis) = &self.basis else { return ([S::zero(); Q], [S::zero(); Q]) };
                let m = basis.forward(y);
                let om = Self::odd_rate(omega, *odd_magic);
                let dom = if derivative { Self::odd_rate_derivative(omega, *odd_magic) } else { S::zero() };
                let mut rm = [S::zero(); Q];
                let mut dm = [S::zero(); Q];
                for k in 0..Q {
                    let (rate, drate) = match basis.groups[k] {
                        MomentGroup::Mass | MomentGroup::Shear => (omega, S::one()),
                        MomentGroup::Momentum | MomentGroup::Odd => (om, dom),
                        MomentGroup::Bulk => match bulk_rate {
                            Some(b) => (S::from_f64(*b), S::zero()),
                            None => (omega, S::one()),
                        },
                        MomentGroup::EvenHigh => match even_rate {
                            Some(b) => (S::from_f64(*b), S::zero()),
                            None => (omega, S::one()),
                        },
                    };
                    rm[k] = m[k] * rate;
                    dm[k] = m[k] * drate;
                }
                let r = basis.inverse::<S, L>(&rm);
                let d = if derivative { basis.inverse::<S, L>(&dm) } else { [S::zero(); Q] };
                (r, d)
            }
            CollisionKind::Regularized => {
                let Some(basis) = &self.basis else { return ([S::zero(); Q], [S::zero(); Q]) };
                let m = basis.forward(y);
                let rm = std::array::from_fn(|k| match basis.groups[k] {
                    MomentGroup::Bulk | MomentGroup::Shear => m[k] * omega,
                    _ => m[k],
                });
                let r = basis.inverse::<S, L>(&rm);
                let d = if derivative {
                    let dm = std::array::from_fn(|k| match basis.groups[k] {
                        MomentGroup::Bulk | MomentGroup::Shear => m[k],
                        _ => S::zero(),
                    });
                    basis.inverse::<S, L>(&dm)
                } else {
                    [S::zero(); Q]
                };
                (r, d)
            }
            CollisionKind::Cumulant { .. } => ([S::zero(); Q], [S::zero(); Q]),
        }
    }

    #[inline]
    fn relax_transpose<S: Scalar>(&self, omega: S, g: &[S; Q]) -> [S; Q] {
        match &self.kind {
            CollisionKind::Bgk | CollisionKind::Trt { .. } => self.relax(omega, g, false).0,
            CollisionKind::Mrt { bulk_rate, odd_magic, even_rate } => {
                let Some(basis) = &self.basis else { return [S::zero(); Q] };
                let wg: [S; Q] = std::array::from_fn(|i| g[i] * L::W[i]);
                let m = basis.forward(&wg);
                let om = Self::odd_rate(omega, *odd_magic);
                let scaled: [S; Q] = std::array::from_fn(|k| {
                    let rate = match basis.groups[k] {
                        MomentGroup::Mass | MomentGroup::Shear => omega,
                        MomentGroup::Momentum | MomentGroup::Odd => om,
                        MomentGroup::Bulk => bulk_rate.map_or(omega, S::from_f64),
                        MomentGroup::EvenHigh => even_rate.map_or(omega, S::from_f64),
                    };
                    m[k] * rate / basis.norms[k]
                });
                basis.transpose(&scaled)
            }
            CollisionKind::Regularized => {
                let Some(basis) = &self.basis else { return [S::zero(); Q] };
                let wg = std::array::from_fn(|i| g[i] * L::W[i]);
                let m = basis.forward(&wg);
                let scaled = std::array::from_fn(|k| {
                    let rate = match basis.groups[k] {
                        MomentGroup::Bulk | MomentGroup::Shear => omega,
                        _ => S::one(),
                    };
                    m[k] * rate / basis.norms[k]
                });
                basis.transpose(&scaled)
            }
            CollisionKind::Cumulant { .. } => [S::zero(); Q],
        }
    }

    #[inline]
    #[must_use]
    pub fn collide<S: Scalar>(&self, f: &[S; Q], rho: S, u: [S; 3], force: [S; 3], omega: S) -> [S; Q] {
        if let CollisionKind::Cumulant { bulk_rate } = &self.kind {
            return self.cumulant(f, rho, u, force, omega, bulk_rate.unwrap_or(1.0));
        }
        match &self.kind {
            CollisionKind::Bgk => return Self::collide_pairs(f, rho, u, force, omega, omega),
            CollisionKind::Trt { magic } => {
                return Self::collide_pairs(f, rho, u, force, omega, Self::odd_rate(omega, *magic));
            }
            _ => {}
        }
        let eq = equilibrium::<S, Q, L>(rho, u);
        let src = Self::source(u, force);
        let y: [S; Q] = std::array::from_fn(|i| f[i] - eq[i] + src[i] * 0.5);
        let (r, _) = self.relax(omega, &y, false);
        std::array::from_fn(|i| f[i] - r[i] + src[i])
    }

    #[inline]
    fn collide_pairs<S: Scalar>(f: &[S; Q], rho: S, u: [S; 3], force: [S; 3], omega: S, om: S) -> [S; Q] {
        let usq = u[0] * u[0] + u[1] * u[1] + u[2] * u[2];
        let uf = u[0] * force[0] + u[1] * force[1] + u[2] * force[2];
        let base = S::one() - usq * 1.5;
        let kp = S::one() - omega * 0.5;
        let km = S::one() - om * 0.5;
        let mut out = [S::zero(); Q];
        let w0 = L::W[0];
        let eq0 = rho * w0 * base;
        out[0] = f[0] - omega * (f[0] - eq0) - kp * uf * (3.0 * w0);
        for i in 1..Q {
            let o = L::OPPOSITE[i];
            if o < i {
                continue;
            }
            let w = L::W[i];
            let cu = dot_c::<S, Q, L>(i, u);
            let cf = dot_c::<S, Q, L>(i, force);
            let wr = rho * w;
            let eqp = wr * (base + cu * cu * 4.5);
            let eqm = wr * cu * 3.0;
            let sp = (cu * cf * 9.0 - uf * 3.0) * w;
            let sm = cf * (3.0 * w);
            let fp = (f[i] + f[o]) * 0.5;
            let fm = (f[i] - f[o]) * 0.5;
            let a = kp * sp - omega * (fp - eqp);
            let b = km * sm - om * (fm - eqm);
            out[i] = f[i] + a + b;
            out[o] = f[o] + a - b;
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn collide_pairs_vjp<S: Scalar>(
        f: &[S; Q],
        rho: S,
        u: [S; 3],
        force: [S; 3],
        omega: S,
        om: S,
        dom: S,
        g: &[S; Q],
    ) -> CollisionBar<S, Q> {
        let usq = u[0] * u[0] + u[1] * u[1] + u[2] * u[2];
        let uf = u[0] * force[0] + u[1] * force[1] + u[2] * force[2];
        let base = S::one() - usq * 1.5;
        let kp = S::one() - omega * 0.5;
        let km = S::one() - om * 0.5;
        let mut f_bar = [S::zero(); Q];
        let mut rho_bar = S::zero();
        let mut usq_bar = S::zero();
        let mut uf_bar = S::zero();
        let mut u_bar = [S::zero(); 3];
        let mut force_bar = [S::zero(); 3];
        let mut omega_bar = S::zero();
        let mut om_bar = S::zero();

        let w0 = L::W[0];
        let eq0 = rho * w0 * base;
        let s0 = -uf * (3.0 * w0);
        let g0 = g[0];
        f_bar[0] = g0 * (S::one() - omega);
        rho_bar += g0 * omega * w0 * base;
        usq_bar -= g0 * omega * rho * (1.5 * w0);
        uf_bar -= g0 * kp * (3.0 * w0);
        omega_bar -= g0 * ((f[0] - eq0) + s0 * 0.5);
        for i in 1..Q {
            let o = L::OPPOSITE[i];
            if o < i {
                continue;
            }
            let w = L::W[i];
            let cu = dot_c::<S, Q, L>(i, u);
            let cf = dot_c::<S, Q, L>(i, force);
            let wr = rho * w;
            let eqp = wr * (base + cu * cu * 4.5);
            let eqm = wr * cu * 3.0;
            let sp = (cu * cf * 9.0 - uf * 3.0) * w;
            let sm = cf * (3.0 * w);
            let fp = (f[i] + f[o]) * 0.5;
            let fm = (f[i] - f[o]) * 0.5;
            let a_bar = g[i] + g[o];
            let b_bar = g[i] - g[o];
            omega_bar -= a_bar * ((fp - eqp) + sp * 0.5);
            om_bar -= b_bar * ((fm - eqm) + sm * 0.5);
            let fp_bar = -omega * a_bar;
            let fm_bar = -om * b_bar;
            let eqp_bar = omega * a_bar;
            let eqm_bar = om * b_bar;
            let sp_bar = kp * a_bar;
            let sm_bar = km * b_bar;
            f_bar[i] = g[i] + (fp_bar + fm_bar) * 0.5;
            f_bar[o] = g[o] + (fp_bar - fm_bar) * 0.5;
            rho_bar += eqp_bar * w * (base + cu * cu * 4.5) + eqm_bar * cu * (3.0 * w);
            usq_bar -= eqp_bar * wr * 1.5;
            let cu_bar = eqp_bar * wr * cu * 9.0 + eqm_bar * wr * 3.0 + sp_bar * cf * (9.0 * w);
            let cf_bar = sp_bar * cu * (9.0 * w) + sm_bar * (3.0 * w);
            uf_bar -= sp_bar * (3.0 * w);
            let c = L::CF[i];
            for d in 0..3 {
                if c[d] != 0.0 {
                    u_bar[d] += cu_bar * c[d];
                    force_bar[d] += cf_bar * c[d];
                }
            }
        }
        for d in 0..3 {
            u_bar[d] += usq_bar * u[d] * 2.0 + uf_bar * force[d];
            force_bar[d] += uf_bar * u[d];
        }
        CollisionBar { f: f_bar, rho: rho_bar, u: u_bar, force: force_bar, omega: omega_bar + om_bar * dom }
    }

    #[inline]
    #[must_use]
    pub fn collide_vjp<S: Scalar>(
        &self,
        f: &[S; Q],
        rho: S,
        u: [S; 3],
        force: [S; 3],
        omega: S,
        g: &[S; Q],
    ) -> CollisionBar<S, Q> {
        match &self.kind {
            CollisionKind::Cumulant { bulk_rate } => {
                return self.cumulant_vjp(f, rho, u, force, omega, bulk_rate.unwrap_or(1.0), g);
            }
            CollisionKind::Bgk => {
                return Self::collide_pairs_vjp(f, rho, u, force, omega, omega, S::one(), g);
            }
            CollisionKind::Trt { magic } => {
                let om = Self::odd_rate(omega, *magic);
                let dom = Self::odd_rate_derivative(omega, *magic);
                return Self::collide_pairs_vjp(f, rho, u, force, omega, om, dom, g);
            }
            CollisionKind::Mrt { .. } | CollisionKind::Regularized => {}
        }
        let eq = equilibrium::<S, Q, L>(rho, u);
        let src = Self::source(u, force);
        let y: [S; Q] = std::array::from_fn(|i| f[i] - eq[i] + src[i] * 0.5);
        let (_, dy) = self.relax(omega, &y, true);
        let rt = self.relax_transpose(omega, g);
        let mut omega_bar = S::zero();
        for i in 0..Q {
            omega_bar -= g[i] * dy[i];
        }
        let f_bar: [S; Q] = std::array::from_fn(|i| g[i] - rt[i]);
        let eq_bar: [S; Q] = rt;
        let src_bar: [S; Q] = std::array::from_fn(|i| g[i] - rt[i] * 0.5);
        let mut rho_bar = S::zero();
        let mut u_bar = [S::zero(); 3];
        equilibrium_vjp::<S, Q, L>(rho, u, &eq_bar, &mut rho_bar, &mut u_bar);
        let mut force_bar = [S::zero(); 3];
        let mut uf_bar = S::zero();
        for i in 0..Q {
            let h = src_bar[i] * L::W[i];
            let cu = dot_c::<S, Q, L>(i, u);
            let cf = dot_c::<S, Q, L>(i, force);
            let cf_bar = h * (cu * 9.0 + 3.0);
            let cu_bar = h * cf * 9.0;
            uf_bar -= h * 3.0;
            let c = L::CF[i];
            for d in 0..3 {
                if c[d] != 0.0 {
                    force_bar[d] += cf_bar * c[d];
                    u_bar[d] += cu_bar * c[d];
                }
            }
        }
        for d in 0..3 {
            u_bar[d] += uf_bar * force[d];
            force_bar[d] += uf_bar * u[d];
        }
        CollisionBar { f: f_bar, rho: rho_bar, u: u_bar, force: force_bar, omega: omega_bar }
    }

    fn cumulant<S: Scalar>(
        &self,
        f: &[S; Q],
        rho: S,
        u: [S; 3],
        force: [S; 3],
        omega: S,
        bulk: f64,
    ) -> [S; Q] {

        let mut cube = [S::zero(); 27];
        for i in 0..Q {
            let c = L::C[i];
            #[allow(clippy::cast_sign_loss)]
            let k = ((c[0] + 1) * 9 + (c[1] + 1) * 3 + (c[2] + 1)) as usize;
            cube[k] = f[i];
        }
        let stride = [9usize, 3, 1];
        for axis in [2usize, 1, 0] {
            let s = stride[axis];
            for base in 0..27 {
                if !(base / s).is_multiple_of(3) {
                    continue;
                }
                let (fm, f0, fp) = (cube[base], cube[base + s], cube[base + 2 * s]);
                let k0 = fm + f0 + fp;
                let diff = fp - fm;
                let ua = u[axis];
                let k1 = diff - ua * k0;
                let k2 = (fp + fm) - ua * diff * 2.0 + ua * ua * k0;
                cube[base] = k0;
                cube[base + s] = k1;
                cube[base + 2 * s] = k2;
            }
        }

        let idx = |a: usize, b: usize, c: usize| a * 9 + b * 3 + c;
        let kxx = cube[idx(2, 0, 0)];
        let kyy = cube[idx(0, 2, 0)];
        let kzz = cube[idx(0, 0, 2)];
        let one_minus = S::one() - omega;
        let dxy = (kxx - kyy) * one_minus;
        let dxz = (kxx - kzz) * one_minus;
        let trace = kxx + kyy + kzz;
        let trace = trace + (rho - trace) * bulk;
        let post_xx = (trace + dxy + dxz) / 3.0;
        let post_yy = (trace - dxy * 2.0 + dxz) / 3.0;
        let post_zz = (trace + dxy - dxz * 2.0) / 3.0;
        let post_xy = cube[idx(1, 1, 0)] * one_minus;
        let post_xz = cube[idx(1, 0, 1)] * one_minus;
        let post_yz = cube[idx(0, 1, 1)] * one_minus;
        let sigma = [
            [post_xx / rho, post_xy / rho, post_xz / rho],
            [post_xy / rho, post_yy / rho, post_yz / rho],
            [post_xz / rho, post_yz / rho, post_zz / rho],
        ];
        let mut post = [S::zero(); 27];
        for k in 0..27 {
            let e = exponents(k);
            let order = e[0] + e[1] + e[2];
            post[k] = match order {
                0 => rho,
                1 => {
                    let axis = if e[0] == 1 {
                        0
                    } else if e[1] == 1 {
                        1
                    } else {
                        2
                    };

                    force[axis] * 0.5
                }
                2 => match e {
                    [2, 0, 0] => post_xx,
                    [0, 2, 0] => post_yy,
                    [0, 0, 2] => post_zz,
                    [1, 1, 0] => post_xy,
                    [1, 0, 1] => post_xz,
                    _ => post_yz,
                },
                _ => {
                    let mut acc = S::zero();
                    for pairing in &self.wick[k] {
                        let mut p = S::one();
                        for &(a, b) in pairing {
                            p *= sigma[a][b];
                        }
                        acc += p;
                    }
                    acc * rho
                }
            };
        }

        for axis in [0usize, 1, 2] {
            let s = stride[axis];
            for base in 0..27 {
                if !(base / s).is_multiple_of(3) {
                    continue;
                }
                let (k0, k1, k2) = (post[base], post[base + s], post[base + 2 * s]);
                let ua = u[axis];
                let m1 = k1 + ua * k0;
                let m2 = k2 + ua * k1 * 2.0 + ua * ua * k0;
                post[base] = (m2 - m1) * 0.5;
                post[base + s] = k0 - m2;
                post[base + 2 * s] = (m2 + m1) * 0.5;
            }
        }
        std::array::from_fn(|i| {
            let c = L::C[i];
            #[allow(clippy::cast_sign_loss)]
            let k = ((c[0] + 1) * 9 + (c[1] + 1) * 3 + (c[2] + 1)) as usize;
            post[k]
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn cumulant_vjp<S: Scalar>(
        &self,
        f: &[S; Q],
        rho: S,
        u: [S; 3],
        force: [S; 3],
        omega: S,
        bulk: f64,
        g: &[S; Q],
    ) -> CollisionBar<S, Q> {
        const N: usize = 35;
        let fd: [Dual<N>; Q] = std::array::from_fn(|i| Dual::variable(f[i].value(), i));
        let rd = Dual::variable(rho.value(), 27);
        let ud: [Dual<N>; 3] = std::array::from_fn(|d| Dual::variable(u[d].value(), 28 + d));
        let fo: [Dual<N>; 3] = std::array::from_fn(|d| Dual::variable(force[d].value(), 31 + d));
        let od = Dual::variable(omega.value(), 34);
        let post = self.cumulant(&fd, rd, ud, fo, od, bulk);
        let mut bar = [0.0; N];
        for i in 0..Q {
            let gi = g[i].value();
            if gi != 0.0 {
                for k in 0..N {
                    bar[k] += gi * post[i].eps[k];
                }
            }
        }
        CollisionBar {
            f: std::array::from_fn(|i| S::from_f64(bar[i])),
            rho: S::from_f64(bar[27]),
            u: std::array::from_fn(|d| S::from_f64(bar[28 + d])),
            force: std::array::from_fn(|d| S::from_f64(bar[31 + d])),
            omega: S::from_f64(bar[34]),
        }
    }
}

