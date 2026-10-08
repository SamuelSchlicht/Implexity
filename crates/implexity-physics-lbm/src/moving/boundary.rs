// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};

use super::lattice::{Lattice, equilibrium, equilibrium_vjp, moments};
use super::pushforward::NONE;
use crate::d3q19::Grid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    XMin,
    XMax,
    YMin,
    YMax,
    ZMin,
    ZMax,
}

impl Face {

    pub fn parse(name: &str) -> CaeResult<Self> {
        Ok(match name {
            "xmin" | "x-" => Self::XMin,
            "xmax" | "x+" => Self::XMax,
            "ymin" | "y-" => Self::YMin,
            "ymax" | "y+" => Self::YMax,
            "zmin" | "z-" => Self::ZMin,
            "zmax" | "z+" => Self::ZMax,
            _ => return Err(CaeError::contract(format!("unknown lattice face {name:?}"))),
        })
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::XMin => "xmin",
            Self::XMax => "xmax",
            Self::YMin => "ymin",
            Self::YMax => "ymax",
            Self::ZMin => "zmin",
            Self::ZMax => "zmax",
        }
    }

    #[must_use]
    pub fn axis(self) -> usize {
        match self {
            Self::XMin | Self::XMax => 0,
            Self::YMin | Self::YMax => 1,
            Self::ZMin | Self::ZMax => 2,
        }
    }

    #[must_use]
    pub fn inward(self) -> i64 {
        match self {
            Self::XMin | Self::YMin | Self::ZMin => 1,
            _ => -1,
        }
    }

    #[must_use]
    pub fn layer(self, shape: [usize; 3]) -> usize {
        if self.inward() > 0 { 0 } else { shape[self.axis()] - 1 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Signal {
    pub frequency_hz: f64,
    pub phase_rad: f64,
    pub ramp_s: f64,
}

impl Signal {
    #[must_use]
    pub const fn steady() -> Self {
        Self { frequency_hz: 0.0, phase_rad: 0.0, ramp_s: 0.0 }
    }

    #[must_use]
    pub fn factors<S: Scalar>(&self, t: S) -> (S, S) {
        let ramp = if self.ramp_s > 0.0 && t.value() < self.ramp_s {
            let s = t / self.ramp_s;
            let s = if s.value() < 0.0 { S::zero() } else { s };
            s * s * s * (S::from_f64(10.0) - s * 15.0 + s * s * 6.0)
        } else {
            S::one()
        };
        let wave = if self.frequency_hz == 0.0 {
            S::zero()
        } else {
            (t * (2.0 * std::f64::consts::PI * self.frequency_hz) + self.phase_rad).sin()
        };
        (ramp, wave)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PortKind {
    Velocity {
        mean_m_s: [f64; 3],
        amplitude_m_s: [f64; 3],
        profile: Vec<f64>,
    },
    Pressure {
        mean_pa: f64,
        amplitude_pa: f64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct PortSpec {
    pub id: String,
    pub face: Face,
    pub cells: Vec<usize>,
    pub kind: PortKind,
    pub signal: Signal,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PortCell {
    pub cell: usize,
    pub neighbour: usize,
    pub port: usize,
    pub profile: f64,
}

#[derive(Clone, Debug)]
pub struct Topology<const Q: usize> {
    pub grid: Grid,
    pub wall: Vec<bool>,
    pub simple: Vec<bool>,
    pub offsets: [isize; Q],
    pub table_slot: Vec<u32>,
    pub pull: Vec<u32>,
    pub push: Vec<u32>,
    pub ports: Vec<PortCell>,
    pub port_slot: Vec<u32>,
    pub periodic: [bool; 3],
}

impl<const Q: usize> Topology<Q> {

    pub fn new<L: Lattice<Q>>(
        grid: Grid,
        periodic: [bool; 3],
        wall: Vec<bool>,
        ports: &[PortSpec],
    ) -> CaeResult<Self> {
        Self::with_symmetry::<L>(grid, periodic, wall, ports, &[])
    }


    pub fn with_symmetry<L: Lattice<Q>>(
        grid: Grid,
        periodic: [bool; 3],
        wall: Vec<bool>,
        ports: &[PortSpec],
        symmetry: &[Face],
    ) -> CaeResult<Self> {
        let n = grid.cells();
        let mut mirror_side = [[false; 2]; 3];
        for f in symmetry {
            let a = f.axis();
            let side = usize::from(f.inward() < 0);
            if periodic[a] || grid.shape[a] < 2 {
                return Err(CaeError::contract(format!(
                    "symmetry face {} lies on a periodic (or one-cell) axis",
                    f.name()
                )));
            }
            if mirror_side[a][side] {
                return Err(CaeError::contract(format!("symmetry face {} is listed twice", f.name())));
            }
            if ports.iter().any(|p| p.face == *f) {
                return Err(CaeError::contract(format!("symmetry face {} also carries a port", f.name())));
            }
            mirror_side[a][side] = true;
        }

        let flipped = |i: usize, flip: [bool; 3]| -> usize {
            let c = L::C[i];
            let t = [
                if flip[0] { -c[0] } else { c[0] },
                if flip[1] { -c[1] } else { c[1] },
                if flip[2] { -c[2] } else { c[2] },
            ];
            (0..Q).find(|&j| L::C[j] == t).unwrap_or(i)
        };

        let link = |x: usize, i: usize, s: i64| -> Option<(usize, [bool; 3])> {
            let c = L::C[i];
            let at = grid.coords(x);
            let mut o = [s * i64::from(c[0]), s * i64::from(c[1]), s * i64::from(c[2])];
            let mut flip = [false; 3];
            for a in 0..3 {
                if o[a] == 0 || periodic[a] {
                    continue;
                }
                #[allow(clippy::cast_possible_wrap)]
                let t = at[a] as i64 + o[a];
                #[allow(clippy::cast_possible_wrap)]
                let len = grid.shape[a] as i64;
                if t < 0 || t >= len {
                    if mirror_side[a][usize::from(t >= len)] {
                        flip[a] = true;
                        o[a] = 0;
                    } else {
                        return None;
                    }
                }
            }
            if !grid.inside(x, o, periodic) {
                return None;
            }
            let y = grid.wrap(x, o);
            (!wall[y]).then_some((y, flip))
        };
        if wall.len() != n {
            return Err(CaeError::contract("solid_mask must have one entry per lattice cell"));
        }
        let shape = grid.shape;
        #[allow(clippy::cast_possible_wrap)]
        let strides = [
            if shape[0] == 1 && periodic[0] { 0 } else { (shape[1] * shape[2]) as isize },
            if shape[1] == 1 && periodic[1] { 0 } else { shape[2] as isize },
            isize::from(!(shape[2] == 1 && periodic[2])),
        ];
        let offsets: [isize; Q] = std::array::from_fn(|i| {
            let c = L::C[i];
            c[0] as isize * strides[0] + c[1] as isize * strides[1] + c[2] as isize * strides[2]
        });
        let neighbour = |x: usize, i: usize, s: i64| -> Option<usize> {
            let c = L::C[i];
            let o = [s * i64::from(c[0]), s * i64::from(c[1]), s * i64::from(c[2])];
            grid.inside(x, o, periodic).then(|| grid.wrap(x, o))
        };
        let mut simple = vec![false; n];
        for x in 0..n {
            if wall[x] {
                continue;
            }
            simple[x] = (0..Q).all(|i| {
                [-1i64, 1].iter().all(|&s| match neighbour(x, i, s) {
                    Some(y) => {
                        #[allow(clippy::cast_possible_wrap)]
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        let same = {
                            let direct = x as isize + (s as isize) * offsets[i];
                            direct >= 0 && direct as usize == y
                        };
                        !wall[y] && same
                    }
                    None => false,
                })
            });
        }
        let mut table_slot = vec![NONE; n];
        let mut pull = Vec::new();
        let mut push = Vec::new();
        for x in 0..n {
            if wall[x] || simple[x] {
                continue;
            }
            #[allow(clippy::cast_possible_truncation)]
            {
                table_slot[x] = (pull.len() / Q) as u32;
            }
            for i in 0..Q {
                let o = L::OPPOSITE[i];
                let src = match link(x, i, -1) {
                    Some((y, flip)) => flipped(i, flip) * n + y,
                    None => o * n + x,
                };
                let reader = match link(x, i, 1) {
                    Some((y, flip)) => flipped(i, flip) * n + y,
                    None => o * n + x,
                };
                #[allow(clippy::cast_possible_truncation)]
                {
                    pull.push(src as u32);
                    push.push(reader as u32);
                }
            }
        }

        let mut port_cells = Vec::new();
        let mut port_slot = vec![NONE; n];
        for (p, spec) in ports.iter().enumerate() {
            let axis = spec.face.axis();
            if periodic[axis] {
                return Err(CaeError::contract(format!("port {:?} lies on periodic axis {axis}", spec.id)));
            }
            if shape[axis] < 3 {
                return Err(CaeError::contract(format!(
                    "port {:?} needs at least three cells along its axis",
                    spec.id
                )));
            }
            let layer = spec.face.layer(shape);
            let cells: Vec<usize> = if spec.cells.is_empty() {
                (0..n).filter(|&x| grid.coords(x)[axis] == layer && !wall[x]).collect()
            } else {
                spec.cells.clone()
            };
            if cells.is_empty() {
                return Err(CaeError::contract(format!("port {:?} has no cells", spec.id)));
            }
            if let PortKind::Velocity { profile, .. } = &spec.kind
                && !profile.is_empty()
                && profile.len() != cells.len()
            {
                return Err(CaeError::contract(format!(
                    "port {:?} profile needs one factor per port cell",
                    spec.id
                )));
            }
            for (r, &x) in cells.iter().enumerate() {
                if x >= n || grid.coords(x)[axis] != layer || wall[x] || port_slot[x] != NONE {
                    return Err(CaeError::contract(format!(
                        "port {:?}: cell {x} is not a free cell of face {}",
                        spec.id,
                        spec.face.name()
                    )));
                }
                let mut o = [0i64; 3];
                o[axis] = spec.face.inward();
                let nb = grid.wrap(x, o);
                if wall[nb] {
                    return Err(CaeError::contract(format!(
                        "port {:?}: inward neighbour of cell {x} is a wall",
                        spec.id
                    )));
                }
                let profile = match &spec.kind {
                    PortKind::Velocity { profile, .. } if !profile.is_empty() => profile[r],
                    _ => 1.0,
                };
                if !profile.is_finite() {
                    return Err(CaeError::contract(format!("port {:?} profile must be finite", spec.id)));
                }
                #[allow(clippy::cast_possible_truncation)]
                {
                    port_slot[x] = port_cells.len() as u32;
                }
                port_cells.push(PortCell { cell: x, neighbour: nb, port: p, profile });
            }
        }
        for pc in &port_cells {
            if port_slot[pc.neighbour] != NONE {
                return Err(CaeError::contract(format!(
                    "port cell {}: its inward neighbour {} is itself a port cell",
                    pc.cell, pc.neighbour
                )));
            }
        }
        Ok(Self {
            grid,
            wall,
            simple,
            offsets,
            table_slot,
            pull,
            push,
            ports: port_cells,
            port_slot,
            periodic,
        })
    }

    #[inline]
    #[must_use]
    pub fn cells(&self) -> usize {
        self.grid.cells()
    }

    #[inline]
    #[must_use]
    pub fn pull_cell<S: Scalar>(&self, g: &[S], x: usize) -> [S; Q] {
        let n = self.grid.cells();
        if self.simple[x] {
            std::array::from_fn(|i| {
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
                let y = (x as isize - self.offsets[i]) as usize;
                g[i * n + y]
            })
        } else {
            let row = self.table_slot[x] as usize * Q;
            std::array::from_fn(|i| g[self.pull[row + i] as usize])
        }
    }

    #[inline]
    #[must_use]
    pub fn reader(&self, y: usize, i: usize) -> usize {
        let n = self.grid.cells();
        if self.simple[y] {
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
            let x = (y as isize + self.offsets[i]) as usize;
            i * n + x
        } else {
            self.push[self.table_slot[y] as usize * Q + i] as usize
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum PortValue<S> {
    Velocity([S; 3]),
    Density(S),
}

#[inline]
#[must_use]
pub fn port_populations<S: Scalar, const Q: usize, L: Lattice<Q>>(
    nb: &[S; Q],
    value: PortValue<S>,
) -> [S; Q] {
    let (rho_n, j) = moments::<S, Q, L>(nb);
    let u_n: [S; 3] = std::array::from_fn(|d| j[d] / rho_n);
    let (rho_b, u_b) = match value {
        PortValue::Velocity(u) => (rho_n, u),
        PortValue::Density(r) => (r, u_n),
    };
    let eb = equilibrium::<S, Q, L>(rho_b, u_b);
    let en = equilibrium::<S, Q, L>(rho_n, u_n);
    std::array::from_fn(|i| eb[i] + nb[i] - en[i])
}

#[inline]
#[must_use]
pub fn port_populations_vjp<S: Scalar, const Q: usize, L: Lattice<Q>>(
    nb: &[S; Q],
    value: PortValue<S>,
    g: &[S; Q],
) -> ([S; Q], [S; 3]) {
    let (rho_n, j) = moments::<S, Q, L>(nb);
    let u_n: [S; 3] = std::array::from_fn(|d| j[d] / rho_n);
    let (rho_b, u_b) = match value {
        PortValue::Velocity(u) => (rho_n, u),
        PortValue::Density(r) => (r, u_n),
    };
    let (mut rb_bar, mut ub_bar) = (S::zero(), [S::zero(); 3]);
    equilibrium_vjp::<S, Q, L>(rho_b, u_b, g, &mut rb_bar, &mut ub_bar);
    let neg: [S; Q] = std::array::from_fn(|i| -g[i]);
    let (mut rn_bar, mut un_bar) = (S::zero(), [S::zero(); 3]);
    equilibrium_vjp::<S, Q, L>(rho_n, u_n, &neg, &mut rn_bar, &mut un_bar);
    let value_bar = match value {
        PortValue::Velocity(_) => {
            rn_bar += rb_bar;
            ub_bar
        }
        PortValue::Density(_) => {
            for d in 0..3 {
                un_bar[d] += ub_bar[d];
            }
            [rb_bar, S::zero(), S::zero()]
        }
    };
    let mut j_bar = [S::zero(); 3];
    for d in 0..3 {
        j_bar[d] = un_bar[d] / rho_n;
        rn_bar -= un_bar[d] * j[d] / (rho_n * rho_n);
    }
    let nb_bar = std::array::from_fn(|i| {
        let c = L::CF[i];
        let mut v = g[i] + rn_bar;
        for d in 0..3 {
            if c[d] != 0.0 {
                v += j_bar[d] * c[d];
            }
        }
        v
    });
    (nb_bar, value_bar)
}

