// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use implexity_solve::local_assembly::Kind;
use rayon::prelude::*;

use super::lattice::{
    PortGeometry, Q, collision, physical_scales, pressure_port, quadrature_and_gradient, transport_maps,
};
use super::owner::{Owner, Viscosity};
use super::sp::{self, Sp};
use super::viscous::stress_and_heat;

const LAW: usize = 36;
const PORT: usize = 37;


pub fn local_blocks<const N: usize, F>(args: &[(&[f64], usize)], law: F) -> CaeResult<Vec<Sp>>
where
    F: Fn(&[Dual<N>]) -> Vec<Dual<N>> + Sync,
{
    let width: usize = args.iter().map(|a| a.1).sum();
    debug_assert_eq!(width, N);
    let count = args.first().map_or(0, |a| a.0.len() / a.1);
    let outputs: Vec<Vec<Dual<N>>> = (0..count)
        .into_par_iter()
        .map(|cell| {
            let mut inputs = Vec::with_capacity(N);
            for (values, w) in args {
                for k in 0..*w {
                    let seed = inputs.len();
                    inputs.push(Dual::variable(values[cell * w + k], seed));
                }
            }
            law(&inputs)
        })
        .collect();
    let m = outputs.first().map_or(0, Vec::len);
    let mut blocks = Vec::with_capacity(args.len());
    let mut offset = 0;
    for (_, w) in args {
        let per: Vec<Vec<f64>> = outputs
            .iter()
            .map(|out| {
                let mut b = Vec::with_capacity(m * w);
                for o in out {
                    for k in 0..*w {
                        b.push(o.eps[offset + k]);
                    }
                }
                b
            })
            .collect();
        if per.iter().flatten().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("finite matching local output derivatives required"));
        }
        blocks.push(sp::block_diag(&per, m, *w)?);
        offset += w;
    }
    Ok(blocks)
}

pub struct LawBlocks {
    pub pop: Sp,
    pub qphi: Sp,
    pub gradphi: Sp,
    pub temperature: Sp,
    pub solid_velocity: Sp,
    pub beta: Sp,
}

impl LawBlocks {
    fn from(mut v: Vec<Sp>) -> Self {
        let beta = v.pop().unwrap_or_else(|| sp::zeros(0, 0));
        let solid_velocity = v.pop().unwrap_or_else(|| sp::zeros(0, 0));
        let temperature = v.pop().unwrap_or_else(|| sp::zeros(0, 0));
        let gradphi = v.pop().unwrap_or_else(|| sp::zeros(0, 0));
        let qphi = v.pop().unwrap_or_else(|| sp::zeros(0, 0));
        let pop = v.pop().unwrap_or_else(|| sp::zeros(0, 0));
        Self { pop, qphi, gradphi, temperature, solid_velocity, beta }
    }

    fn chains(&self, c: &Chains) -> CaeResult<[Sp; 3]> {
        let current = sp::add(
            &sp::add(&sp::mm(&self.temperature, &c.tz)?, &sp::mm(&self.solid_velocity, &c.uz)?)?,
            &sp::mm3(&self.beta, &c.bt, &c.tz)?,
        )?;
        let previous = sp::sub(&sp::mm(&self.pop, &c.e)?, &sp::mm(&self.solid_velocity, &c.uz)?)?;
        let design = sp::add(
            &sp::add(&sp::mm(&self.qphi, &c.qx)?, &sp::mm(&self.gradphi, &c.gx)?)?,
            &sp::mm(&self.beta, &c.bx)?,
        )?;
        Ok([current, previous, design])
    }
}

struct LawInputs {
    pop: Vec<f64>,
    q: Vec<f64>,
    g: Vec<f64>,
    t: Vec<f64>,
    us: Vec<f64>,
    beta: Vec<f64>,
}

impl LawInputs {
    fn args(&self) -> Vec<(&[f64], usize)> {
        vec![(&self.pop, Q), (&self.q, 1), (&self.g, 3), (&self.t, 1), (&self.us, 3), (&self.beta, 1)]
    }
}

fn split<const N: usize>(
    x: &[Dual<N>],
) -> (&[Dual<N>], Dual<N>, [Dual<N>; 3], Dual<N>, [Dual<N>; 3], Dual<N>) {
    (&x[..Q], x[27], [x[28], x[29], x[30]], x[31], [x[32], x[33], x[34]], x[35])
}

fn tau<S: Scalar>(v: &Viscosity, t: S, h: f64, dt: f64) -> S {
    v.nu(t) * 3.0 * dt / (h * h) + 0.5
}

pub struct Chains {
    pub e: Sp,
    pub tz: Sp,
    pub uz: Sp,
    pub u: Sp,
    pub m: Sp,
    pub qx: Sp,
    pub gx: Sp,
    pub bx: Sp,
    pub bt: Sp,
    pub x: Sp,
}

pub struct FlowPartials {
    pub flow: [Sp; 3],
    pub post: [Sp; 3],
    pub expected: [Sp; 3],
    pub streamed: Sp,
    pub escaped: Sp,
    pub rates: Vec<Sp>,
    pub wall: Option<([Sp; 3], [Sp; 3])>,
}

impl Owner {

    pub fn chains(&self, x: &[f64]) -> CaeResult<Chains> {
        let s = &self.s;
        let nt = self.state_size;
        let nc = self.nc();
        let nt_free = s.free_t.len();
        let e = sp::triplets(
            self.nf,
            nt,
            &(0..self.nf).collect::<Vec<_>>(),
            &(self.ns..nt).collect::<Vec<_>>(),
            &vec![1.0; self.nf],
        )?;
        let m =
            sp::triplets(s.nn, nt, &s.free_t, &(0..nt_free).collect::<Vec<_>>(), &vec![s.model.ts; nt_free])?;
        let nu = s.free_u.len();
        let u = sp::triplets(
            3 * s.nn,
            nt,
            &s.free_u,
            &(nt_free..nt_free + nu).collect::<Vec<_>>(),
            &vec![s.model.us / self.dt; nu],
        )?;
        let tz = sp::mm(&self.cal.l, &m)?;
        let uz = sp::mm(&sp::kron_eye(&self.drag.q, 3)?, &u)?;
        let (rho, c) = self.coordinates(x);
        let (native, qx, gx) = self.geometry.partials(&rho, &c, &self.rx, &self.cx)?;
        Ok(Chains {
            e,
            tz,
            uz,
            u,
            m,
            qx,
            gx,
            bx: sp::scale(&self.rx, self.beta),
            bt: sp::zeros(nc, nc),
            x: native,
        })
    }

    fn law_inputs(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> LawInputs {
        let (_, phi, halo) = self.phase(x);
        let (q, g) = quadrature_and_gradient(self.grid, &halo);
        let (t, u) = self.nodal_fields(n, z);
        let (_, uo) = self.nodal_fields(n - 1, old);
        let tc = self.cal.cell_temperature(&t);
        let v: Vec<[f64; 3]> =
            u.iter().zip(&uo).map(|(a, b)| std::array::from_fn(|k| (a[k] - b[k]) / self.dt)).collect();
        let us = self.drag.solid_cell_velocity(&v);
        LawInputs {
            pop: old[self.ns..].to_vec(),
            q,
            g: g.iter().flatten().copied().collect(),
            t: tc,
            us: us.iter().flatten().copied().collect(),
            beta: self.drag_beta(&phi),
        }
    }

    fn collision_blocks(&self, inputs: &LawInputs) -> CaeResult<LawBlocks> {
        let (visc, h, dt) = (self.viscosity, self.h, self.dt);
        local_blocks::<LAW, _>(&inputs.args(), |x| {
            let (f, q, g, t, us, beta) = split(x);
            let us = us.map(|v| v * dt / h);
            collision(f, q, &g, tau(&visc, t, h, dt), &us, beta).post.to_vec()
        })
        .map(LawBlocks::from)
    }

    fn exchange_blocks(&self, inputs: &LawInputs) -> CaeResult<LawBlocks> {
        let (visc, h, dt, rho) = (self.viscosity, self.h, self.dt, self.rho);
        let (fs, ps) = physical_scales(rho, h, dt);
        local_blocks::<LAW, _>(&inputs.args(), |x| {
            let (f, q, g, t, us, beta) = split(x);
            let us = us.map(|v| v * dt / h);
            let d = collision(f, q, &g, tau(&visc, t, h, dt), &us, beta).drag;
            let mut out: Vec<Dual<LAW>> = d.solid_reaction_force_density.iter().map(|v| *v * fs).collect();
            out.push(d.relative_work_dissipation * ps);
            out
        })
        .map(LawBlocks::from)
    }

    fn viscous_blocks(&self, inputs: &LawInputs) -> CaeResult<LawBlocks> {
        let (visc, h, dt, rho) = (self.viscosity, self.h, self.dt, self.rho);
        local_blocks::<LAW, _>(&inputs.args(), |x| {
            let (f, q, g, t, us, beta) = split(x);
            let usl = us.map(|v| v * dt / h);
            let ta = tau(&visc, t, h, dt);
            let d = collision(f, q, &g, ta, &usl, beta).drag;
            let out =
                stress_and_heat(f, q, ta, &d.intrinsic_velocity, &d.total_fluid_force_density, rho, h, dt);
            let mut v = out.intrinsic_stress.to_vec();
            v.push(out.heat);
            v
        })
        .map(LawBlocks::from)
    }

    fn port_partials(
        &self,
        geo: &PortGeometry,
        cells: &[usize],
        expected: &[f64],
        p: f64,
        q: &[f64],
        g: &[[f64; 3]],
    ) -> CaeResult<[Sp; 4]> {
        let nc = self.nc();
        let k = cells.len();
        let streamed: Vec<f64> = cells.iter().flat_map(|c| expected[c * Q..(c + 1) * Q].to_vec()).collect();
        let pressure = vec![p; k];
        let qs: Vec<f64> = cells.iter().map(|c| q[*c]).collect();
        let gs: Vec<f64> = cells.iter().flat_map(|c| g[*c]).collect();
        let zeros3 = vec![0.0; 3 * k];
        let zeros2 = vec![0.0; 2 * k];
        let blocks = local_blocks::<PORT, _>(
            &[(&streamed, Q), (&pressure, 1), (&qs, 1), (&gs, 3), (&zeros3, 3), (&zeros2, 2)],
            |x| {
                let f = &x[..Q];
                pressure_port(
                    geo,
                    f,
                    x[27],
                    x[28],
                    &[x[29], x[30], x[31]],
                    &[x[32], x[33], x[34]],
                    &[x[35], x[36]],
                )
                .to_vec()
            },
        )?;
        let selected: Vec<usize> = cells.iter().flat_map(|c| (0..Q).map(move |i| c * Q + i)).collect();
        let scatter = sp::t(&sp::selector(&selected, Q * nc)?);
        let sel = |width: usize| -> CaeResult<Sp> {
            let cols: Vec<usize> =
                cells.iter().flat_map(|c| (0..width).map(move |i| c * width + i)).collect();
            sp::selector(&cols, nc * width)
        };
        let ones = sp::triplets(k, 1, &(0..k).collect::<Vec<_>>(), &vec![0; k], &vec![1.0; k])?;
        let mut outside = vec![1.0; Q * nc];
        for i in &selected {
            outside[*i] = 0.0;
        }
        let streamed_block = sp::add(&sp::mm3(&scatter, &blocks[0], &sel(Q)?)?, &sp::diag(&outside))?;
        let pressure_block = sp::mm3(&scatter, &blocks[1], &ones)?;
        let q_block = sp::mm3(&scatter, &blocks[2], &sel(1)?)?;
        let g_block = sp::mm3(&scatter, &blocks[3], &sel(3)?)?;
        Ok([streamed_block, q_block, g_block, pressure_block])
    }


    pub fn flow_partials(
        &self,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        c: &Chains,
    ) -> CaeResult<FlowPartials> {
        let nt = self.state_size;
        let nd = self.design_size;
        let nf = self.nf;
        let inputs = self.law_inputs(n, z, old, x);
        let blocks = self.collision_blocks(&inputs)?;
        let post = blocks.chains(c)?;
        let scale = self.rho * self.h.powi(3) / self.dt;
        let (streamed, escaped, rates) = transport_maps(self.grid, scale)?;
        let ledger = self.transport_interval(n, z, old, x);
        let values = ledger.drag.post.clone();
        let mut expected = sp::mv(&streamed, &values)?;
        let mut derivatives =
            [sp::mm(&streamed, &post[0])?, sp::mm(&streamed, &post[1])?, sp::mm(&streamed, &post[2])?];
        let mut wall_blocks = None;
        if let Some(wall) = &self.wall {
            let velocity: Vec<[f64; 3]> = ledger
                .displacement
                .iter()
                .zip(&ledger.previous_displacement)
                .map(|(a, b)| std::array::from_fn(|k| (a[k] - b[k]) / self.dt))
                .collect();
            let exchanged = wall.exchange(&values, &velocity);
            wall.return_populations(&mut expected, &exchanged);
            let wb = wall.partials(&values, &velocity)?;
            let chains = |pk: &Sp, vk: &Sp| -> CaeResult<[Sp; 3]> {
                Ok([
                    sp::add(&sp::mm(pk, &post[0])?, &sp::mm(vk, &c.u)?)?,
                    sp::sub(&sp::mm(pk, &post[1])?, &sp::mm(vk, &c.u)?)?,
                    sp::mm(pk, &post[2])?,
                ])
            };
            let returned = chains(&wb.returned_post, &wb.returned_velocity)?;
            let inject = wall.inject()?;
            for k in 0..3 {
                derivatives[k] = sp::add(&derivatives[k], &sp::mm(&inject, &returned[k])?)?;
            }
            wall_blocks = Some((
                chains(&wb.solid_force_post, &wb.solid_force_velocity)?,
                chains(&wb.mass_rate_post, &wb.mass_rate_velocity)?,
            ));
        }
        let pscale = self.rho * (self.h / self.dt).powi(2);
        let faces = self.face_pressure[n];
        let px = sp::zeros(1, nd);
        for (face, index) in [(0usize, 0usize), (1, self.grid.n[0] - 1)] {
            if self.wall.as_ref().is_some_and(|w| w.face == face) {
                continue;
            }
            let geo = &self.ports[face];
            let pressure = 1.0 / 3.0 + faces[face] / pscale;
            let cells = self.grid.plane(index);
            let [ps, pq, pg, pp] =
                self.port_partials(geo, &cells, &expected, pressure, &ledger.q, &ledger.g)?;
            derivatives = [
                sp::mm(&ps, &derivatives[0])?,
                sp::mm(&ps, &derivatives[1])?,
                sp::sum(&[
                    sp::mm(&ps, &derivatives[2])?,
                    sp::mm(&pq, &c.qx)?,
                    sp::mm(&pg, &c.gx)?,
                    sp::scale(&sp::mm(&pp, &px)?, 1.0 / pscale),
                ])?,
            ];
            for cell in cells {
                let out = pressure_port(
                    geo,
                    &expected[cell * Q..(cell + 1) * Q],
                    pressure,
                    ledger.q[cell],
                    &ledger.g[cell],
                    &[0.0; 3],
                    &[0.0; 2],
                );
                expected[cell * Q..(cell + 1) * Q].copy_from_slice(&out);
            }
        }
        let flow = [
            sp::checked(sp::sub(&c.e, &derivatives[0])?, (nf, nt))?,
            sp::checked(sp::neg(&derivatives[1]), (nf, nt))?,
            sp::checked(sp::neg(&derivatives[2]), (nf, nd))?,
        ];
        Ok(FlowPartials { flow, post, expected: derivatives, streamed, escaped, rates, wall: wall_blocks })
    }


    pub fn full_partials(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<[Sp; 3]> {
        let s = &self.s;
        let nc = self.nc();
        let nf = self.nf;
        let ns = self.ns;
        let nt = self.state_size;
        let nd = self.design_size;
        let c = self.chains(x)?;
        let flow = self.flow_partials(n, z, old, x, &c)?;
        let m = &s.model;
        let nt_free = s.free_t.len();
        let nu = s.free_u.len();
        let sel = sp::hstack(&[&sp::eye(ns), &sp::zeros(ns, nf)])?;
        let heat_rows = sp::triplets(
            ns,
            s.nn,
            &(0..nt_free).collect::<Vec<_>>(),
            &s.free_t,
            &vec![1.0 / (m.ks * m.ts * m.ls); nt_free],
        )?;
        let force_rows = sp::triplets(
            ns,
            3 * s.nn,
            &(nt_free..nt_free + nu).collect::<Vec<_>>(),
            &s.free_u,
            &vec![-1.0 / (m.ss * m.ls * m.ls); nu],
        )?;
        let pops: Vec<usize> = (0..nf).map(|k| k / Q).collect();
        let sum_pop = sp::triplets(nc, nf, &pops, &(0..nf).collect::<Vec<_>>(), &vec![1.0; nf])?;
        let mass = sp::scale(&sum_pop, self.rho * self.h.powi(3));
        let ledger = self.transport_interval(n, z, old, x);
        let (to, _) = self.nodal_fields(n - 1, old);
        let post_primal = &ledger.drag.post;
        let rates: Vec<Vec<f64>> =
            flow.rates.iter().map(|a| sp::mv(a, post_primal)).collect::<CaeResult<_>>()?;
        let mut delta: Vec<f64> = s.fixed_t[n].iter().zip(&s.fixed_t[n - 1]).map(|(a, b)| a - b).collect();
        for (k, node) in s.free_t.iter().enumerate() {
            delta[*node] = m.ts * (z[k] - old[k]);
        }
        let cell_mass = |f: &[f64]| -> Vec<f64> {
            (0..nc).map(|c| f[c * Q..(c + 1) * Q].iter().sum::<f64>() * self.rho * self.h.powi(3)).collect()
        };
        let cp = self.cal.partials(
            &cell_mass(&z[ns..]),
            &cell_mass(&old[ns..]),
            &to,
            &delta,
            &rates,
            &ledger.port_boundary,
            &self.reservoir,
            self.cp,
            self.dt,
        )?;
        let open = sp::add(&flow.streamed, &flow.escaped)?;
        let mass_dt = sp::scale(&mass, 1.0 / self.dt);
        let mut boundary = Vec::with_capacity(3);
        for k in 0..3 {
            let b = sp::mm(&mass_dt, &sp::sub(&flow.expected[k], &sp::mm(&open, &flow.post[k])?)?)?;
            boundary.push(match &self.wall {
                None => b,
                Some(w) => sp::mm(&sp::diag(&w.port_mask), &b)?,
            });
        }
        let lt = sp::t(&self.cal.l);
        let mut caloric = Vec::with_capacity(3);
        for (kind, width) in [(0, nt), (1, nt), (2, nd)] {
            let mut a = sp::mm(&cp.boundary_mass_rate, &boundary[kind])?;
            for (partial, rate) in cp.rates.iter().zip(&flow.rates) {
                a = sp::add(&a, &sp::mm3(partial, rate, &flow.post[kind])?)?;
            }
            if kind == 0 {
                a = sp::add(&a, &sp::mm(&cp.temperature_increment, &c.m)?)?;
                a = sp::add(&a, &sp::mm3(&cp.mnew, &mass, &c.e)?)?;
            }
            if kind == 1 {
                let tm = sp::sub(&sp::add(&cp.told, &cp.transport_temperature)?, &cp.temperature_increment)?;
                a = sp::add(&a, &sp::mm(&tm, &c.m)?)?;
                a = sp::add(&a, &sp::mm3(&cp.mold, &mass, &c.e)?)?;
            }
            if let Some((_, mass_rate)) = &flow.wall {
                a = sp::sub(&a, &sp::scale(&sp::mm3(&sp::diag(&to), &lt, &mass_rate[kind])?, self.cp))?;
                if kind == 1 {
                    let e =
                        ledger.wall.as_ref().map(|e| e.reference_mass_rate_kg_s.clone()).unwrap_or_default();
                    let lr: Vec<f64> = sp::mv(&lt, &e)?.iter().map(|v| v * self.cp).collect();
                    a = sp::sub(&a, &sp::mm(&sp::diag(&lr), &c.m)?)?;
                }
            }
            caloric.push(sp::checked(a, (s.nn, width))?);
        }
        let inputs = self.law_inputs(n, z, old, x);
        let exchange = self.exchange_blocks(&inputs)?.chains(&c)?;
        let force_select =
            sp::selector(&(0..nc).flat_map(|c| (0..3).map(move |a| 4 * c + a)).collect::<Vec<_>>(), 4 * nc)?;
        let heat_select = sp::selector(&(0..nc).map(|c| 4 * c + 3).collect::<Vec<_>>(), 4 * nc)?;
        let drag_force = sp::mm(&sp::kron_eye(&sp::t(&self.drag.q), 3)?, &force_select)?;
        let drag_heat = sp::mm(&lt, &heat_select)?;
        let interface_force: [Sp; 3] = match (&self.trace, &flow.wall) {
            (Some(tr), _) => {
                let p2f = tr.pressure_force_matrix()?;
                let ps = self.rho * (self.h / self.dt).powi(2) / 3.0;
                let mflat: Vec<f64> = (0..nc).map(|c| z[ns + c * Q..ns + (c + 1) * Q].iter().sum()).collect();
                let dq: Vec<f64> = ledger.q.iter().map(|q| ps / q).collect();
                let dm: Vec<f64> = ledger.q.iter().zip(&mflat).map(|(q, m)| -ps * m / (q * q)).collect();
                let pressure = [
                    sp::mm3(&sp::diag(&dq), &sum_pop, &c.e)?,
                    sp::zeros(nc, nt),
                    sp::mm(&sp::diag(&dm), &c.qx)?,
                ];
                [sp::mm(&p2f, &pressure[0])?, sp::mm(&p2f, &pressure[1])?, sp::mm(&p2f, &pressure[2])?]
            }
            (None, Some((force, _))) => force.clone(),
            (None, None) => {
                return Err(CaeError::contract(
                    "pressure_trace or an explicitly authored reference wall is required",
                ));
            }
        };
        let xs = &ledger.phase_design;
        let native = [
            sp::mm(&s.jacobian(Kind::Current, n, &z[..ns], &old[..ns], xs)?, &sel)?,
            sp::mm(&s.jacobian(Kind::Previous, n, &z[..ns], &old[..ns], xs)?, &sel)?,
            sp::mm(&s.jacobian(Kind::Design, n, &z[..ns], &old[..ns], xs)?, &c.x)?,
        ];
        let viscous = if self.viscous.is_some() {
            let blocks = self.viscous_blocks(&inputs)?.chains(&c)?;
            let stress_select = sp::selector(
                &(0..nc).flat_map(|c| (0..9).map(move |a| 10 * c + a)).collect::<Vec<_>>(),
                10 * nc,
            )?;
            let heat_select = sp::selector(&(0..nc).map(|c| 10 * c + 9).collect::<Vec<_>>(), 10 * nc)?;
            let to_force = match (&self.trace, self.viscous_flag("trace_traction")) {
                (Some(tr), true) => Some(sp::mm(&tr.viscous_force_matrix()?, &stress_select)?),
                _ => None,
            };
            Some((blocks, to_force, sp::mm(&lt, &heat_select)?))
        } else {
            None
        };
        let mut full = Vec::with_capacity(3);
        for (kind, width) in [(0, nt), (1, nt), (2, nd)] {
            let heat = sp::sub(&caloric[kind], &sp::mm(&drag_heat, &exchange[kind])?)?;
            let force = sp::add(&sp::mm(&drag_force, &exchange[kind])?, &interface_force[kind])?;
            let mut solid = sp::add(
                &sp::add(&native[kind], &sp::mm(&heat_rows, &heat)?)?,
                &sp::mm(&force_rows, &force)?,
            )?;
            if let Some((blocks, to_force, to_heat)) = &viscous {
                if self.viscous_flag("heating") {
                    solid = sp::sub(&solid, &sp::mm3(&heat_rows, to_heat, &blocks[kind])?)?;
                }
                if let Some(tf) = to_force {
                    solid = sp::add(&solid, &sp::mm3(&force_rows, tf, &blocks[kind])?)?;
                }
            }
            full.push(sp::checked(sp::vstack(&[&solid, &flow.flow[kind]])?, (nt, width))?);
        }
        let mut it = full.into_iter();
        let (a, b, cm) = (it.next(), it.next(), it.next());
        match (a, b, cm) {
            (Some(a), Some(b), Some(cm)) => Ok([a, b, cm]),
            _ => Err(CaeError::contract("incomplete porous partials")),
        }
    }
}
