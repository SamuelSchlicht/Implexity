// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};

use super::boundary::Topology;
use crate::d3q19::Grid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeQuantity {
    Pressure,
    Velocity(usize),
    Density,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ObservableSpec {
    SectionMassFlux { axis: usize, index: usize },
    SectionFlux {
        axis: usize,
        index: usize,
    },
    Probe {
        point_m: [f64; 3],
        quantity: ProbeQuantity,
    },
    PortPower {
        port: usize,
    },
    KineticEnergy,
    SolidForce {
        component: usize,
    },
    SectionOpenArea {
        axis: usize,
        range: [usize; 2],
        beta: f64,
    },
}

#[derive(Clone, Debug)]
pub struct Observable {
    pub population_links: Vec<(usize, usize, f64)>,
    pub exterior_cells: Vec<usize>,
    pub name: String,
    pub spec: ObservableSpec,
    pub cells: Vec<(usize, f64)>,
    pub normal: Vec<[f64; 3]>,
    pub degree: i32,
    pub sections: Vec<Vec<usize>>,
    pub face_area: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct SampleScales<S> {
    pub density: f64,
    pub spacing: f64,
    pub velocity: S,
    pub macro_step: S,
    pub fluid_step: S,
}

impl Observable {

    pub fn new<const Q: usize>(
        name: &str,
        spec: ObservableSpec,
        topology: &Topology<Q>,
        spacing_m: f64,
        origin_m: [f64; 3],
    ) -> CaeResult<Self> {
        let grid: Grid = topology.grid;
        let shape = grid.shape;
        let bad = |m: &str| CaeError::contract(format!("observable {name:?}: {m}"));
        let mut cells = Vec::new();
        let mut normal = Vec::new();
        let mut sections = Vec::new();
        let mut face_area = 0.0;
        let mut population_links = Vec::new();
        let mut exterior_cells = Vec::new();
        let degree = match &spec {
            ObservableSpec::SectionMassFlux { axis, index } => {
                if *axis > 2 || *index >= shape[*axis].saturating_sub(1) || topology.periodic[*axis] {
                    return Err(bad("mass-flux face must be internal to a nonperiodic axis"));
                }
                for x in 0..grid.cells() {
                    if topology.wall[x] { continue; }
                    for q in 0..Q {
                        let pulled = topology.reader(x, q);
                        let y = pulled % grid.cells();
                        if topology.wall[y] { continue; }
                        let a = grid.coords(x)[*axis];
                        let b = grid.coords(y)[*axis];
                        let sign = if a == *index && b == *index + 1 { 1.0 }
                            else if b == *index && a == *index + 1 { -1.0 } else { continue; };
                        if topology.ports.iter().any(|p| p.cell == x || p.cell == y) {
                            return Err(bad("mass-flux face must exclude port reconstruction cells"));
                        }
                        population_links.push((q * grid.cells() + x, pulled, sign));
                        exterior_cells.extend([x, y]);
                    }
                }
                if population_links.is_empty() { return Err(bad("mass-flux face has no fluid links")); }
                exterior_cells.sort_unstable();
                exterior_cells.dedup();
                1
            }
            ObservableSpec::SectionOpenArea { axis, range, beta } => {
                if *axis > 2 || range[0] > range[1] || range[1] >= shape[*axis] {
                    return Err(bad("section range outside the lattice"));
                }
                if range[1] > range[0] && !(beta.is_finite() && *beta > 0.0) {
                    return Err(bad("the smooth minimum over several sections needs a finite beta > 0"));
                }
                for i in range[0]..=range[1] {
                    sections.push(
                        (0..grid.cells())
                            .filter(|&x| grid.coords(x)[*axis] == i && !topology.wall[x])
                            .collect(),
                    );
                }
                face_area = (0..3)
                    .filter(|&a| a != *axis && !(shape[a] == 1 && topology.periodic[a]))
                    .fold(1.0, |acc, _| acc * spacing_m);
                0
            }
            ObservableSpec::SectionFlux { axis, index } => {
                if *axis > 2 || *index >= shape[*axis] {
                    return Err(bad("section plane outside the lattice"));
                }
                for x in 0..grid.cells() {
                    if grid.coords(x)[*axis] == *index && !topology.wall[x] {
                        cells.push((x, 1.0));
                    }
                }
                1
            }
            ObservableSpec::Probe { point_m, quantity } => {
                if let ProbeQuantity::Velocity(d) = quantity
                    && *d > 2
                {
                    return Err(bad("velocity component must be 0, 1 or 2"));
                }
                let xi: [f64; 3] = std::array::from_fn(|a| (point_m[a] - origin_m[a]) / spacing_m - 0.5);
                let mut corners: Vec<(usize, f64)> = Vec::new();
                let mut lo = [0usize; 3];
                let mut frac = [0.0; 3];
                for a in 0..3 {
                    if !xi[a].is_finite() {
                        return Err(bad("probe point must be finite"));
                    }
                    let n = shape[a];
                    if n == 1 {
                        continue;
                    }
                    let v = xi[a].clamp(0.0, (n - 1) as f64);
                    if (v - xi[a]).abs() > 0.5 {
                        return Err(bad("probe point outside the lattice"));
                    }
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let l = (v.floor() as usize).min(n.saturating_sub(2));
                    lo[a] = l;
                    frac[a] = v - l as f64;
                }
                for corner in 0..8usize {
                    let mut w = 1.0;
                    let mut c = [0usize; 3];
                    let mut skip = false;
                    for a in 0..3 {
                        let hi = (corner >> a) & 1 == 1;
                        if shape[a] == 1 {
                            if hi {
                                skip = true;
                            }
                            continue;
                        }
                        c[a] = lo[a] + usize::from(hi);
                        w *= if hi { frac[a] } else { 1.0 - frac[a] };
                    }
                    if skip || w == 0.0 {
                        continue;
                    }
                    let x = grid.index(c[0], c[1], c[2]);
                    if !topology.wall[x] {
                        corners.push((x, w));
                    }
                }
                let total: f64 = corners.iter().map(|c| c.1).sum();
                if corners.is_empty() || total <= 0.0 {
                    return Err(bad("probe point has no fluid neighbour"));
                }
                cells = corners.into_iter().map(|(x, w)| (x, w / total)).collect();
                match quantity {
                    ProbeQuantity::Pressure => 2,
                    ProbeQuantity::Velocity(_) => 1,
                    ProbeQuantity::Density => 0,
                }
            }
            ObservableSpec::PortPower { port } => {
                let mut found = false;
                for pc in &topology.ports {
                    if pc.port == *port {
                        found = true;
                        cells.push((pc.cell, 1.0));
                        let a = (0..3)
                            .find(|&a| grid.coords(pc.cell)[a] != grid.coords(pc.neighbour)[a])
                            .unwrap_or(0);
                        let mut nrm = [0.0; 3];
                        nrm[a] =
                            if grid.coords(pc.neighbour)[a] > grid.coords(pc.cell)[a] { 1.0 } else { -1.0 };
                        normal.push(nrm);
                    }
                }
                if !found {
                    return Err(bad("unknown port"));
                }
                3
            }
            ObservableSpec::KineticEnergy => 2,
            ObservableSpec::SolidForce { component } => {
                if *component > 2 {
                    return Err(bad("force component must be 0, 1 or 2"));
                }
                2
            }
        };
        Ok(Self { population_links, exterior_cells, name: name.to_string(), spec, cells, normal, degree, sections, face_area })
    }

    #[must_use]
    pub fn reads_cells(&self) -> bool {
        !matches!(self.spec, ObservableSpec::SolidForce { .. } | ObservableSpec::SectionOpenArea { .. } | ObservableSpec::SectionMassFlux { .. })
    }

    pub fn reads_populations(&self) -> bool {
        matches!(self.spec, ObservableSpec::SectionMassFlux { .. })
    }

    pub fn population_value<S: Scalar>(&self, scales: &SampleScales<S>, stored: &[S]) -> S {
        let mut v = S::zero();
        for &(source, _, sign) in &self.population_links { v += stored[source] * sign; }
        v * scales.velocity * (scales.density * scales.spacing * scales.spacing)
    }

    pub fn population_pull_vjp<S: Scalar>(&self, scales: &SampleScales<S>, bar: S, pulled_bar: &mut [S]) {
        let k = bar * scales.velocity * (scales.density * scales.spacing * scales.spacing);
        for &(_, pulled, sign) in &self.population_links { pulled_bar[pulled] += k * sign; }
    }

    #[must_use]
    pub fn reads_occupancy(&self) -> bool {
        matches!(self.spec, ObservableSpec::SectionOpenArea { .. })
    }

    pub fn open_area<S: Scalar>(
        &self,
        eps: impl Fn(usize) -> S,
        bar: Option<S>,
        mut eps_bar: impl FnMut(usize, S),
    ) -> S {
        let ObservableSpec::SectionOpenArea { beta, .. } = self.spec else {
            return S::zero();
        };
        let areas: Vec<S> = self
            .sections
            .iter()
            .map(|cells| {
                let mut a = S::zero();
                for &x in cells {
                    a += S::one() - eps(x);
                }
                a * self.face_area
            })
            .collect();
        if areas.len() == 1 {
            if let Some(b) = bar {
                for &x in &self.sections[0] {
                    eps_bar(x, -(b * self.face_area));
                }
            }
            return areas[0];
        }

        let lo = areas.iter().fold(f64::INFINITY, |m, a| m.min(a.value()));
        let e: Vec<S> = areas.iter().map(|a| ((*a - lo) * -beta).exp()).collect();
        let mut sum = S::zero();
        for v in &e {
            sum += *v;
        }
        let value = S::from_f64(lo) - sum.ln() / beta;
        if let Some(b) = bar {
            for (cells, w) in self.sections.iter().zip(&e) {
                let k = -(b * *w / sum * self.face_area);
                for &x in cells {
                    eps_bar(x, k);
                }
            }
        }
        value
    }

    #[must_use]
    pub fn value<S: Scalar>(
        &self,
        s: &SampleScales<S>,
        rho: &[S],
        u: &[[S; 3]],
        fluid: &[bool],
        dp_sum: [S; 3],
    ) -> S {
        let c = s.velocity;
        let dx = s.spacing;
        match &self.spec {
            ObservableSpec::SectionMassFlux { .. } => S::zero(),
            ObservableSpec::SectionFlux { axis, .. } => {
                let mut acc = S::zero();
                for &(x, _) in &self.cells {
                    acc += rho[x] * u[x][*axis];
                }
                acc * c * (dx * dx)
            }
            ObservableSpec::Probe { quantity, .. } => {
                let mut acc = S::zero();
                for &(x, w) in &self.cells {
                    acc += match quantity {
                        ProbeQuantity::Pressure => (rho[x] - 1.0) / 3.0,
                        ProbeQuantity::Velocity(d) => u[x][*d],
                        ProbeQuantity::Density => rho[x],
                    } * w;
                }
                acc * match quantity {
                    ProbeQuantity::Pressure => c * c * s.density,
                    ProbeQuantity::Velocity(_) => c,
                    ProbeQuantity::Density => S::from_f64(s.density),
                }
            }
            ObservableSpec::PortPower { .. } => {
                let mut acc = S::zero();
                for (k, &(x, _)) in self.cells.iter().enumerate() {
                    let n = self.normal[k];
                    let un = u[x][0] * n[0] + u[x][1] * n[1] + u[x][2] * n[2];
                    let u2 = u[x][0] * u[x][0] + u[x][1] * u[x][1] + u[x][2] * u[x][2];
                    acc += ((rho[x] - 1.0) / 3.0 + rho[x] * u2 * 0.5) * un;
                }
                acc * c * c * c * (s.density * dx * dx)
            }
            ObservableSpec::KineticEnergy => {
                let mut acc = S::zero();
                for x in 0..rho.len() {
                    if fluid[x] {
                        let u2 = u[x][0] * u[x][0] + u[x][1] * u[x][1] + u[x][2] * u[x][2];
                        acc += rho[x] * u2 * 0.5;
                    }
                }
                acc * c * c * (s.density * dx * dx * dx)
            }
            ObservableSpec::SolidForce { component } => {
                -dp_sum[*component] * (s.density * dx.powi(4)) / (s.fluid_step * s.macro_step)
            }

            ObservableSpec::SectionOpenArea { .. } => S::zero(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn vjp<S: Scalar>(
        &self,
        s: &SampleScales<S>,
        rho: &[S],
        u: &[[S; 3]],
        fluid: &[bool],
        bar: S,
        rho_bar: &mut [S],
        u_bar: &mut [[S; 3]],
    ) -> [S; 3] {
        let c = s.velocity;
        let dx = s.spacing;
        let zero = [S::zero(); 3];
        match &self.spec {
            ObservableSpec::SectionMassFlux { .. } => zero,
            ObservableSpec::SectionFlux { axis, .. } => {
                let k = bar * c * (dx * dx);
                for &(x, _) in &self.cells {
                    rho_bar[x] += k * u[x][*axis];
                    u_bar[x][*axis] += k * rho[x];
                }
                zero
            }
            ObservableSpec::Probe { quantity, .. } => {
                let k = bar
                    * match quantity {
                        ProbeQuantity::Pressure => c * c * s.density,
                        ProbeQuantity::Velocity(_) => c,
                        ProbeQuantity::Density => S::from_f64(s.density),
                    };
                for &(x, w) in &self.cells {
                    match quantity {
                        ProbeQuantity::Pressure => rho_bar[x] += k * (w / 3.0),
                        ProbeQuantity::Velocity(d) => u_bar[x][*d] += k * w,
                        ProbeQuantity::Density => rho_bar[x] += k * w,
                    }
                }
                zero
            }
            ObservableSpec::PortPower { .. } => {
                let k = bar * c * c * c * (s.density * dx * dx);
                for (e, &(x, _)) in self.cells.iter().enumerate() {
                    let n = self.normal[e];
                    let un = u[x][0] * n[0] + u[x][1] * n[1] + u[x][2] * n[2];
                    let u2 = u[x][0] * u[x][0] + u[x][1] * u[x][1] + u[x][2] * u[x][2];
                    let head = (rho[x] - 1.0) / 3.0 + rho[x] * u2 * 0.5;
                    rho_bar[x] += k * (u2 * 0.5 + 1.0 / 3.0) * un;
                    for d in 0..3 {
                        u_bar[x][d] += k * (rho[x] * u[x][d] * un + head * n[d]);
                    }
                }
                zero
            }
            ObservableSpec::KineticEnergy => {
                let k = bar * c * c * (s.density * dx * dx * dx);
                for x in 0..rho.len() {
                    if fluid[x] {
                        let u2 = u[x][0] * u[x][0] + u[x][1] * u[x][1] + u[x][2] * u[x][2];
                        rho_bar[x] += k * u2 * 0.5;
                        for d in 0..3 {
                            u_bar[x][d] += k * rho[x] * u[x][d];
                        }
                    }
                }
                zero
            }
            ObservableSpec::SolidForce { component } => {
                let mut out = zero;
                out[*component] = -bar * (s.density * dx.powi(4)) / (s.fluid_step * s.macro_step);
                out
            }

            ObservableSpec::SectionOpenArea { .. } => zero,
        }
    }
}
