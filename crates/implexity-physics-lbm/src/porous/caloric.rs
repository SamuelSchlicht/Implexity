// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};

use super::lattice::{C, Grid, POSITIVE};
use super::sp::{self, Sp, SpMap};

pub const SOURCE_PROFILE: &str = "lbm_shared_nodal_T4_conservative_donor_enthalpy_v2";

#[derive(Clone, Debug)]
pub struct Transport<S> {
    pub advective_power_divergence_w: Vec<S>,
    pub boundary_power_w: Vec<S>,
    pub net_mass_rate_kg_s: Vec<S>,
    pub capacity_rate_w_k: Vec<S>,
    pub outgoing_conductance_w_k: Vec<S>,
}

fn offset(i: usize) -> [i64; 3] {
    C[i].map(i64::from)
}

fn neg3(c: [i64; 3]) -> [i64; 3] {
    c.map(|v| -v)
}

fn roll<S: Scalar>(grid: Grid, x: &[S], shift: [i64; 3]) -> Vec<S> {
    (0..grid.cells()).map(|c| x[grid.wrap(c, neg3(shift))]).collect()
}

fn link_divergence<S: Scalar>(grid: Grid, rates: &[Vec<S>]) -> Vec<S> {
    let mut out = vec![S::zero(); grid.cells()];
    for (k, m) in rates.iter().enumerate() {
        let back = roll(grid, m, offset(POSITIVE[k]));
        for x in 0..grid.cells() {

            out[x] += m[x] - back[x];
        }
    }
    out
}

#[must_use]
pub fn sensible_transport<S: Scalar>(
    grid: Grid,
    temperature: &[S],
    rates: &[Vec<S>],
    cp: f64,
    boundary: &[S],
    reservoir: &[f64],
) -> Transport<S> {
    let n = grid.cells();
    let mut outgoing = vec![S::zero(); n];
    let mut advective = Vec::with_capacity(rates.len());
    for (k, m) in rates.iter().enumerate() {
        let c = offset(POSITIVE[k]);
        let other = roll(grid, temperature, neg3(c));
        let m_back = roll(grid, m, c);
        advective.push(
            (0..n)
                .map(|x| if m[x].value() >= 0.0 { m[x] * temperature[x] * cp } else { m[x] * other[x] * cp })
                .collect::<Vec<S>>(),
        );
        for x in 0..n {
            outgoing[x] += (m[x].max_f64(0.0) + (-m_back[x]).max_f64(0.0)) * cp;
        }
    }
    let boundary_power: Vec<S> = (0..n)
        .map(|x| {
            let e = boundary[x];
            if e.value() >= 0.0 { e * reservoir[x] * cp } else { e * temperature[x] * cp }
        })
        .collect();
    for x in 0..n {
        outgoing[x] += (-boundary[x]).max_f64(0.0) * cp;
    }
    let divergence = link_divergence(grid, rates);
    let net: Vec<S> = (0..n).map(|x| boundary[x] - divergence[x]).collect();
    let capacity: Vec<S> = net.iter().map(|v| *v * cp).collect();
    Transport {
        advective_power_divergence_w: link_divergence(grid, &advective),
        boundary_power_w: boundary_power,
        net_mass_rate_kg_s: net,
        capacity_rate_w_k: capacity,
        outgoing_conductance_w_k: outgoing,
    }
}

#[derive(Clone, Debug)]
pub struct CaloricLedger<S> {
    pub nodal_residual_w: Vec<S>,
    pub nodal_transport_outward_w: Vec<S>,
    pub nodal_outgoing_conductance_w_k: Vec<S>,
    pub fluid_energy_new_j: S,
    pub fluid_energy_old_j: S,
    pub fluid_energy_increment_j: S,
    pub boundary_power_into_w: S,
    pub cell_mass_balance_error_kg: Vec<S>,
    pub nodal_capacity_new_j_k: Vec<S>,
    pub transport: Transport<S>,
    pub port_boundary_power_into_w: Option<S>,
    pub wall_reference_caloric_power_into_w: Option<S>,
    pub wall_reference_nodal_power_w: Option<Vec<S>>,
    pub port_net_mass_rate_kg_s: Option<Vec<S>>,
}

#[derive(Clone, Debug)]
pub struct SharedFluidCaloric {
    pub grid: Grid,
    pub nn: usize,
    pub l: Sp,
    lmap: SpMap,
}

impl SharedFluidCaloric {

    pub fn new(grid: Grid, nn: usize, tets: &[[usize; 4]], owners: &[usize]) -> CaeResult<Self> {
        let rows: Vec<usize> = owners.iter().flat_map(|o| [*o; 4]).collect();
        let cols: Vec<usize> = tets.iter().flatten().copied().collect();
        let l = sp::triplets(grid.cells(), nn, &rows, &cols, &vec![1.0 / 24.0; rows.len()])?;
        let sums = sp::mv(&l, &vec![1.0; nn])?;
        if l.data().iter().any(|v| *v <= 0.0) || sums.iter().any(|v| (v - 1.0).abs() > 1e-15) {
            return Err(CaeError::contract("invalid native caloric quadrature"));
        }
        Ok(Self { grid, nn, lmap: SpMap::new(&l), l })
    }

    #[must_use]
    pub fn lmap(&self) -> &SpMap {
        &self.lmap
    }

    #[must_use]
    pub fn cell_temperature<S: Scalar>(&self, t: &[S]) -> Vec<S> {
        self.lmap.apply(t)
    }

    #[must_use]
    pub fn interval<S: Scalar>(
        &self,
        tnew: &[S],
        told: &[S],
        mnew: &[S],
        mold: &[S],
        rates: &[Vec<S>],
        boundary: &[S],
        reservoir: &[f64],
        cp: f64,
        dt: f64,
        delta: &[S],
    ) -> CaloricLedger<S> {
        let l = &self.lmap;
        let cpm = |m: &[S]| m.iter().map(|v| *v * cp).collect::<Vec<S>>();
        let cn = l.apply_t(&cpm(mnew));
        let co = l.apply_t(&cpm(mold));
        let tc = self.cell_temperature(told);
        let transport = sensible_transport(self.grid, &tc, rates, cp, boundary, reservoir);
        let outward: Vec<S> = transport
            .advective_power_divergence_w
            .iter()
            .zip(&transport.boundary_power_w)
            .map(|(a, b)| *a - *b)
            .collect();
        let mut nodal_outward = l.apply_t(&outward);
        let outgoing = &transport.outgoing_conductance_w_k;
        let nodal_outgoing = l.apply_t(outgoing);
        let (rows, cols, data) = l.coo();
        let mut correction = vec![S::zero(); self.nn];
        for ((r, c), d) in rows.iter().zip(cols).zip(data) {
            correction[*c] += outgoing[*r] * *d * (told[*c] - tc[*r]);
        }
        for (o, c) in nodal_outward.iter_mut().zip(&correction) {
            *o += *c;
        }
        let dm: Vec<S> = mnew.iter().zip(mold).map(|(a, b)| (*a - *b) * cp).collect();
        let dc = l.apply_t(&dm);
        let storage: Vec<S> = (0..self.nn).map(|i| (cn[i] * delta[i] + dc[i] * told[i]) / dt).collect();
        let residual: Vec<S> = storage.iter().zip(&nodal_outward).map(|(a, b)| *a + *b).collect();
        let total = |v: &[S]| v.iter().fold(S::zero(), |acc, x| acc + *x);
        let new_t: Vec<S> = (0..self.nn).map(|i| cn[i] * (told[i] + delta[i])).collect();
        let old_e: Vec<S> = (0..self.nn).map(|i| co[i] * told[i]).collect();
        let inc: Vec<S> = (0..self.nn).map(|i| cn[i] * delta[i] + dc[i] * told[i]).collect();
        let balance: Vec<S> = (0..self.grid.cells())
            .map(|x| mnew[x] - mold[x] - transport.net_mass_rate_kg_s[x] * dt)
            .collect();
        let _ = tnew;
        CaloricLedger {
            nodal_residual_w: residual,
            nodal_transport_outward_w: nodal_outward,
            nodal_outgoing_conductance_w_k: nodal_outgoing,
            fluid_energy_new_j: total(&new_t),
            fluid_energy_old_j: total(&old_e),
            fluid_energy_increment_j: total(&inc),
            boundary_power_into_w: total(&transport.boundary_power_w),
            cell_mass_balance_error_kg: balance,
            nodal_capacity_new_j_k: cn,
            transport,
            port_boundary_power_into_w: None,
            wall_reference_caloric_power_into_w: None,
            wall_reference_nodal_power_w: None,
            port_net_mass_rate_kg_s: None,
        }
    }


    pub fn partials(
        &self,
        mnew: &[f64],
        mold: &[f64],
        told: &[f64],
        delta: &[f64],
        rates: &[Vec<f64>],
        boundary: &[f64],
        reservoir: &[f64],
        cp: f64,
        dt: f64,
    ) -> CaeResult<CaloricPartials> {
        let nc = self.grid.cells();
        let nn = self.nn;
        let all = mnew.iter().chain(mold).chain(told).chain(delta).chain(boundary).chain(reservoir);
        if all.clone().any(|v| !v.is_finite()) || rates.iter().flatten().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("shared caloric partials require exact-shape finite real data"));
        }
        let l = &self.l;
        let lt = sp::t(l);
        let tt = told;
        let tc = sp::mv(l, tt)?;
        let eye = sp::eye(nc);
        let mut k = sp::zeros(nc, nc);
        let mut outgoing = vec![0.0; nc];
        let mut rate_blocks = Vec::with_capacity(rates.len());

        let donor_correction = sp::sub(&sp::mm(&sp::diag(tt), &lt)?, &sp::mm(&lt, &sp::diag(&tc))?)?;
        for (idx, m) in rates.iter().enumerate() {
            let c = offset(POSITIVE[idx]);

            let other: Vec<usize> = (0..nc).map(|x| self.grid.wrap(x, c)).collect();
            let p = sp::triplets(nc, nc, &(0..nc).collect::<Vec<_>>(), &other, &vec![1.0; nc])?;
            let pt = sp::t(&p);
            let d = sp::sub(&eye, &pt)?;
            let incoming: Vec<bool> = m.iter().map(|v| *v >= 0.0).collect();
            let donor: Vec<f64> = (0..nc).map(|x| if incoming[x] { tc[x] } else { tc[other[x]] }).collect();
            let a1: Vec<f64> = (0..nc).map(|x| cp * if incoming[x] { m[x] } else { 0.0 }).collect();
            let a2: Vec<f64> = (0..nc).map(|x| cp * if incoming[x] { 0.0 } else { m[x] }).collect();
            k = sp::add(&k, &sp::mm(&d, &sp::add(&sp::diag(&a1), &sp::mm(&sp::diag(&a2), &p)?)?)?)?;
            let negm: Vec<f64> = m.iter().map(|v| (-v).max(0.0)).collect();
            let back = sp::mv(&pt, &negm)?;
            for x in 0..nc {
                outgoing[x] += cp * (m[x].max(0.0) + back[x]);
            }
            let pos: Vec<f64> = m
                .iter()
                .map(|v| {
                    if *v > 0.0 {
                        1.0
                    } else if *v < 0.0 {
                        0.0
                    } else {
                        0.5
                    }
                })
                .collect();
            let negd: Vec<f64> = m
                .iter()
                .map(|v| {
                    if *v < 0.0 {
                        -1.0
                    } else if *v > 0.0 {
                        0.0
                    } else {
                        -0.5
                    }
                })
                .collect();
            let dout = sp::scale(&sp::add(&sp::diag(&pos), &sp::mm(&pt, &sp::diag(&negd))?)?, cp);
            let cd: Vec<f64> = donor.iter().map(|v| cp * v).collect();
            rate_blocks
                .push(sp::add(&sp::mm3(&lt, &d, &sp::diag(&cd))?, &sp::mm(&donor_correction, &dout)?)?);
        }
        let bneg: Vec<f64> = boundary.iter().map(|b| cp * if *b >= 0.0 { 0.0 } else { *b }).collect();
        k = sp::sub(&k, &sp::diag(&bneg))?;
        for x in 0..nc {
            outgoing[x] += cp * (-boundary[x]).max(0.0);
        }
        let bod: Vec<f64> = boundary
            .iter()
            .map(|b| {
                cp * if *b < 0.0 {
                    -1.0
                } else if *b > 0.0 {
                    0.0
                } else {
                    -0.5
                }
            })
            .collect();
        let bres: Vec<f64> =
            (0..nc).map(|x| cp * if boundary[x] >= 0.0 { reservoir[x] } else { tc[x] }).collect();
        let jb =
            sp::add(&sp::neg(&sp::mm(&lt, &sp::diag(&bres))?), &sp::mm(&donor_correction, &sp::diag(&bod))?)?;
        let ltmn = sp::mv(&lt, mnew)?;
        let dm: Vec<f64> = mnew.iter().zip(mold).map(|(a, b)| a - b).collect();
        let told_diag: Vec<f64> = sp::mv(&lt, &dm)?.iter().map(|v| cp * v / dt).collect();
        let transport_temperature = sp::sub(
            &sp::add(&sp::mm3(&lt, &k, l)?, &sp::diag(&sp::mv(&lt, &outgoing)?))?,
            &sp::mm3(&lt, &sp::diag(&outgoing), l)?,
        )?;
        let mnew_scale: Vec<f64> = (0..nn).map(|i| cp * (told[i] + delta[i]) / dt).collect();
        let mold_scale: Vec<f64> = (0..nn).map(|i| -cp * told[i] / dt).collect();
        let inc: Vec<f64> = ltmn.iter().map(|v| cp * v / dt).collect();
        Ok(CaloricPartials {
            told: sp::diag(&told_diag),
            transport_temperature,
            mnew: sp::mm(&sp::diag(&mnew_scale), &lt)?,
            mold: sp::mm(&sp::diag(&mold_scale), &lt)?,
            rates: rate_blocks,
            boundary_mass_rate: jb,
            temperature_increment: sp::diag(&inc),
        })
    }
}

#[derive(Clone, Debug)]
pub struct CaloricPartials {
    pub told: Sp,
    pub transport_temperature: Sp,
    pub mnew: Sp,
    pub mold: Sp,
    pub rates: Vec<Sp>,
    pub boundary_mass_rate: Sp,
    pub temperature_increment: Sp,
}
