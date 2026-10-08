// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_core::{CaeError, CaeResult};

use super::equilibrium::{Lattice, checked_equilibrium};

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

fn coordinates(shape: [usize; 3]) -> Vec<[i64; 3]> {
    let mut out = Vec::with_capacity(shape.iter().product());
    for i in 0..shape[0] {
        for j in 0..shape[1] {
            for k in 0..shape[2] {
                #[allow(clippy::cast_possible_wrap)]
                out.push([i as i64, j as i64, k as i64]);
            }
        }
    }
    out
}

#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
fn ravel(p: [i64; 3], shape: [usize; 3]) -> usize {
    ((p[0] as usize) * shape[1] + p[1] as usize) * shape[2] + p[2] as usize
}

fn lookup(velocities: &[[i32; 3]]) -> std::collections::BTreeMap<[i32; 3], usize> {
    velocities.iter().enumerate().map(|(q, c)| (*c, q)).collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Reservoirs {
    pub incoming_face: Vec<i8>,
    pub outgoing_face: Vec<i8>,
    pub reservoir_f: Vec<Vec<f64>>,
    pub reservoir_g: Vec<Vec<f64>>,
    pub gamma: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TransportKind {
    Box,
    Reservoir(Reservoirs),
    Slab {
        axis: usize,
        destinations: Vec<[usize; 4]>,
        weights: Vec<[f64; 4]>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct WallEvents {
    pub source_flat_indices: Vec<usize>,
    pub positions_lattice: Vec<[f64; 3]>,
    pub impulse_per_population: Vec<[f64; 3]>,
    pub face_indices: Vec<usize>,
    pub interval_fraction: Vec<f64>,
    pub localization: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Transport {
    pub shape: [usize; 3],
    pub lattice: Lattice,
    pub periodic: [bool; 3],
    pub source_indices: Vec<usize>,
    pub wall_impulse: Vec<[f64; 3]>,
    pub face_normal_impulse: Vec<[f64; 6]>,
    pub kind: TransportKind,
    pub wall_events: Option<WallEvents>,
}

impl Transport {
    #[must_use]
    pub fn q(&self) -> usize {
        self.lattice.data().q()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.shape.iter().product::<usize>() * self.q()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}


pub fn box_transport_map(shape: [usize; 3], periodic: [bool; 3], lattice: Lattice) -> CaeResult<Transport> {
    if shape.contains(&0) {
        return Err(err("Box shape requires three positive integers"));
    }
    let velocities = &lattice.data().velocities;
    let q = velocities.len();
    let table = lookup(velocities);
    let coords = coordinates(shape);
    let n_cells = coords.len();
    let mut destination = vec![0usize; n_cells * q];
    let mut impulse = vec![[0.0; 3]; n_cells * q];
    let mut face = vec![[0.0; 6]; n_cells * q];
    for (cell, x) in coords.iter().enumerate() {
        for (vq, c) in velocities.iter().enumerate() {
            let mut endpoint = [0i64; 3];
            let mut arrival = *c;
            for axis in 0..3 {
                #[allow(clippy::cast_possible_wrap)]
                let n = shape[axis] as i64;
                let ca = i64::from(c[axis]);
                let e = x[axis] + ca;
                if periodic[axis] {
                    endpoint[axis] = e.rem_euclid(n);
                } else {
                    #[allow(clippy::cast_possible_truncation)]
                    let crossings = (((x[axis] + ca) as f64 + 0.5) / n as f64).floor().abs() as i64;
                    let plus = i64::midpoint(crossings, i64::from(c[axis] > 0));
                    let minus = crossings - plus;
                    let mag = 2.0 * ca.abs() as f64;
                    face[cell * q + vq][2 * axis] = mag * minus as f64;
                    face[cell * q + vq][2 * axis + 1] = mag * plus as f64;
                    let folded = e.rem_euclid(2 * n);
                    let reflected = folded >= n;
                    endpoint[axis] = if reflected { 2 * n - 1 - folded } else { folded };
                    if reflected {
                        arrival[axis] = -arrival[axis];
                    }
                }
            }
            let aq = table[&arrival];
            destination[cell * q + vq] = ravel(endpoint, shape) * q + aq;
            impulse[cell * q + vq] = std::array::from_fn(|d| f64::from(c[d] - arrival[d]));
        }
    }
    let mut source = vec![usize::MAX; destination.len()];
    for (i, d) in destination.iter().enumerate() {
        if source[*d] != usize::MAX {
            return Err(err("Specular transport did not form a phase-space permutation"));
        }
        source[*d] = i;
    }
    Ok(Transport {
        shape,
        lattice,
        periodic,
        source_indices: source,
        wall_impulse: impulse,
        face_normal_impulse: face,
        kind: TransportKind::Box,
        wall_events: None,
    })
}


pub fn equilibrium_reservoir_boundary(
    shape: [usize; 3],
    density: [f64; 2],
    velocity: [[f64; 3]; 2],
    temperature: [f64; 2],
    gamma: f64,
    lattice: Lattice,
) -> CaeResult<Transport> {
    if shape.contains(&0) {
        return Err(err("Reservoir grid requires three positive integers"));
    }
    let states = (0..2)
        .map(|i| checked_equilibrium(density[i], velocity[i], temperature[i], gamma, lattice))
        .collect::<CaeResult<Vec<_>>>()?;
    let velocities = &lattice.data().velocities;
    let q = velocities.len();
    let coords = coordinates(shape);
    let mut source = Vec::with_capacity(coords.len() * q);
    let mut incoming = Vec::with_capacity(coords.len() * q);
    let mut outgoing = Vec::with_capacity(coords.len() * q);
    #[allow(clippy::cast_possible_wrap)]
    let n = shape.map(|v| v as i64);
    for x in &coords {
        for (vq, c) in velocities.iter().enumerate() {
            let mut s = [x[0] - i64::from(c[0]), x[1] - i64::from(c[1]), x[2] - i64::from(c[2])];
            incoming.push(if s[0] < 0 {
                0
            } else if s[0] >= n[0] {
                1
            } else {
                -1
            });
            s[0] = s[0].clamp(0, n[0] - 1);
            s[1] = s[1].rem_euclid(n[1]);
            s[2] = s[2].rem_euclid(n[2]);
            source.push(ravel(s, shape) * q + vq);
            let target = x[0] + i64::from(c[0]);
            outgoing.push(if target < 0 {
                0
            } else if target >= n[0] {
                1
            } else {
                -1
            });
        }
    }
    Ok(Transport {
        shape,
        lattice,
        periodic: [false, true, true],
        source_indices: source,
        wall_impulse: vec![[0.0; 3]; coords.len() * q],
        face_normal_impulse: vec![[0.0; 6]; coords.len() * q],
        kind: TransportKind::Reservoir(Reservoirs {
            incoming_face: incoming,
            outgoing_face: outgoing,
            reservoir_f: states.iter().map(|s| s.f.clone()).collect(),
            reservoir_g: states.iter().map(|s| s.g.clone()).collect(),
            gamma,
        }),
        wall_events: None,
    })
}


pub fn slab_map(shape: [usize; 3], axis: usize, lattice: Lattice) -> CaeResult<Transport> {
    if shape.contains(&0) {
        return Err(err("Slab shape requires three positive integers"));
    }
    if axis > 2 {
        return Err(err("Slab normal axis must be 0, 1 or 2"));
    }
    let velocities = &lattice.data().velocities;
    let q = velocities.len();
    let table = lookup(velocities);
    let coords = coordinates(shape);
    #[allow(clippy::cast_possible_wrap)]
    let n = shape[axis] as i64;
    let transverse: Vec<usize> = (0..3).filter(|a| *a != axis).collect();
    let total = coords.len() * q;
    let mut destinations = vec![[0usize; 4]; total];
    let mut weights = vec![[0.0; 4]; total];
    let mut impulse = vec![[0.0; 3]; total];
    let mut face = vec![[0.0; 6]; total];
    for (cell, x) in coords.iter().enumerate() {
        for (i, v) in velocities.iter().enumerate() {
            let va = i64::from(v[axis]);
            let folded = (x[axis] + va).rem_euclid(2 * n);
            let reflected = folded >= n;
            let end_normal = if reflected { 2 * n - 1 - folded } else { folded };
            let signed_time = if v[axis] == 0 { 1.0 } else { (end_normal - x[axis]) as f64 / va as f64 };
            let mut endpoint: [f64; 3] = std::array::from_fn(|d| x[d] as f64 + signed_time * f64::from(v[d]));
            endpoint[axis] = end_normal as f64;
            let arrival = if reflected { v.map(|c| -c) } else { *v };
            let aq = table[&arrival];
            let idx = cell * q + i;
            impulse[idx] = std::array::from_fn(|d| f64::from(v[d] - arrival[d]));
            #[allow(clippy::cast_possible_truncation)]
            let crossings = (((x[axis] + va) as f64 + 0.5) / n as f64).floor().abs() as i64;
            let plus = i64::midpoint(crossings, i64::from(v[axis] > 0));
            let mag = 2.0 * va.abs() as f64;
            face[idx][2 * axis] = mag * (crossings - plus) as f64;
            face[idx][2 * axis + 1] = mag * plus as f64;
            #[allow(clippy::cast_possible_truncation)]
            let lower: [i64; 3] = endpoint.map(|e| e.floor() as i64);
            let fraction: [f64; 3] = std::array::from_fn(|d| endpoint[d] - lower[d] as f64);
            for (k, (oa, ob)) in [(0, 0), (0, 1), (1, 0), (1, 1)].into_iter().enumerate() {
                let mut position = lower;
                let mut w = 1.0;
                for (dim, offset) in transverse.iter().zip([oa, ob]) {
                    #[allow(clippy::cast_possible_wrap)]
                    let m = shape[*dim] as i64;
                    position[*dim] = (position[*dim] + offset).rem_euclid(m);
                    w *= if offset == 1 { fraction[*dim] } else { 1.0 - fraction[*dim] };
                }
                destinations[idx][k] = ravel(position, shape) * q + aq;
                weights[idx][k] = w;
            }
        }
    }
    if weights.iter().any(|w| (w.iter().sum::<f64>() - 1.0).abs() > 1e-14 || w.iter().any(|v| *v < 0.0)) {
        return Err(err("Invalid conservative return interpolation"));
    }
    Ok(Transport {
        shape,
        lattice,
        periodic: std::array::from_fn(|a| a != axis),
        source_indices: Vec::new(),
        wall_impulse: impulse,
        face_normal_impulse: face,
        kind: TransportKind::Slab { axis, destinations, weights },
        wall_events: None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct FaceExchange {
    pub mass: [f64; 2],
    pub momentum: [[f64; 3]; 2],
    pub energy: [f64; 2],
}

#[derive(Clone, Debug, PartialEq)]
pub struct Streamed {
    pub f: Vec<f64>,
    pub g: Vec<f64>,
    pub wall_impulse: [f64; 3],
    pub exchange: FaceExchange,
}

#[must_use]
pub fn stream_pair(f: &[f64], g: &[f64], t: &Transport) -> Streamed {
    let q = t.q();
    let data = t.lattice.data();
    let mut wall = [0.0; 3];
    for (i, v) in f.iter().enumerate() {
        for d in 0..3 {
            wall[d] += v * t.wall_impulse[i][d];
        }
    }
    match &t.kind {
        TransportKind::Box => Streamed {
            f: t.source_indices.iter().map(|s| f[*s]).collect(),
            g: t.source_indices.iter().map(|s| g[*s]).collect(),
            wall_impulse: wall,
            exchange: FaceExchange::default(),
        },
        TransportKind::Slab { destinations, weights, .. } => {
            let mut a = vec![0.0; f.len()];
            let mut b = vec![0.0; g.len()];
            for j in 0..f.len() {
                for k in 0..4 {
                    a[destinations[j][k]] += f[j] * weights[j][k];
                    b[destinations[j][k]] += g[j] * weights[j][k];
                }
            }
            Streamed { f: a, g: b, wall_impulse: wall, exchange: FaceExchange::default() }
        }
        TransportKind::Reservoir(r) => {
            let n = f.len();
            let mut a = vec![0.0; n];
            let mut b = vec![0.0; n];
            for i in 0..n {
                let face = r.incoming_face[i];
                if face >= 0 {
                    #[allow(clippy::cast_sign_loss)]
                    let fi = face as usize;
                    a[i] = r.reservoir_f[fi][i % q];
                    b[i] = r.reservoir_g[fi][i % q];
                } else {
                    a[i] = f[t.source_indices[i]];
                    b[i] = g[t.source_indices[i]];
                }
            }
            let mut exchange = FaceExchange::default();
            for boundary in 0..2 {
                let bi = i8::try_from(boundary).unwrap_or(0);
                let (mut mass, mut momentum, mut energy) = (0.0, [0.0; 3], 0.0);
                for i in 0..n {
                    let vq = i % q;
                    let df = if r.incoming_face[i] == bi { a[i] } else { 0.0 }
                        - if r.outgoing_face[i] == bi { f[i] } else { 0.0 };
                    let dg = if r.incoming_face[i] == bi { b[i] } else { 0.0 }
                        - if r.outgoing_face[i] == bi { g[i] } else { 0.0 };
                    mass += df;
                    let c = data.velocities[vq];
                    for d in 0..3 {
                        momentum[d] += df * f64::from(c[d]);
                    }
                    energy += df * data.speed2[vq] + dg;
                }
                exchange.mass[boundary] = mass;
                exchange.momentum[boundary] = momentum;
                exchange.energy[boundary] = 0.5 * energy;
            }
            Streamed { f: a, g: b, wall_impulse: [0.0; 3], exchange }
        }
    }
}

#[must_use]
pub fn stream_pair_vjp(
    t: &Transport,
    fo_bar: &[f64],
    go_bar: &[f64],
    exchange_bar: Option<&FaceExchange>,
) -> (Vec<f64>, Vec<f64>) {
    let n = fo_bar.len();
    let q = t.q();
    let data = t.lattice.data();
    let mut f_bar = vec![0.0; n];
    let mut g_bar = vec![0.0; n];
    match &t.kind {
        TransportKind::Box => {
            for (i, s) in t.source_indices.iter().enumerate() {
                f_bar[*s] += fo_bar[i];
                g_bar[*s] += go_bar[i];
            }
        }
        TransportKind::Slab { destinations, weights, .. } => {
            for j in 0..n {
                for k in 0..4 {
                    f_bar[j] += fo_bar[destinations[j][k]] * weights[j][k];
                    g_bar[j] += go_bar[destinations[j][k]] * weights[j][k];
                }
            }
        }
        TransportKind::Reservoir(r) => {
            for i in 0..n {
                if r.incoming_face[i] < 0 {
                    f_bar[t.source_indices[i]] += fo_bar[i];
                    g_bar[t.source_indices[i]] += go_bar[i];
                }
            }
            if let Some(e) = exchange_bar {
                for i in 0..n {
                    let face = r.outgoing_face[i];
                    if face >= 0 {
                        #[allow(clippy::cast_sign_loss)]
                        let b = face as usize;
                        let vq = i % q;
                        let c = data.velocities[vq];
                        let mut v = e.mass[b] + 0.5 * e.energy[b] * data.speed2[vq];
                        for d in 0..3 {
                            v += e.momentum[b][d] * f64::from(c[d]);
                        }
                        f_bar[i] -= v;
                        g_bar[i] -= 0.5 * e.energy[b];
                    }
                }
            }
        }
    }
    (f_bar, g_bar)
}

#[must_use]
pub fn box_face_impulses(f: &[f64], t: &Transport) -> [f64; 6] {
    let mut out = [0.0; 6];
    for (v, w) in f.iter().zip(&t.face_normal_impulse) {
        for k in 0..6 {
            out[k] += v * w[k];
        }
    }
    out
}

#[must_use]
pub fn periodic_seam_images(position: [f64; 3], shape: [usize; 3], periodic: [bool; 3]) -> Vec<[f64; 3]> {
    let mut images = vec![position];
    for axis in 0..3 {
        let length = shape[axis] as f64;
        if periodic[axis] && position[axis].abs().min((position[axis] - length).abs()) <= 1e-12 {
            let mut expanded = Vec::with_capacity(images.len() * 2);
            for point in &images {
                for endpoint in [0.0, length] {
                    let mut c = *point;
                    c[axis] = endpoint;
                    expanded.push(c);
                }
            }
            images = expanded;
        }
    }
    images
}


pub fn box_wall_events(t: &Transport, maximum_events: usize) -> CaeResult<WallEvents> {
    if maximum_events < 1 {
        return Err(err("Positive integer wall-event budget required"));
    }
    if !matches!(t.kind, TransportKind::Box) {
        return Err(err("This transport does not support specular wall events"));
    }
    let velocities = &t.lattice.data().velocities;
    let q = velocities.len();
    let mut ev = empty_events();
    for (cell, x) in coordinates(t.shape).iter().enumerate() {
        let start: [f64; 3] = x.map(|v| v as f64 + 0.5);
        for (vq, c) in velocities.iter().enumerate() {
            for axis in 0..3 {
                if t.periodic[axis] || c[axis] == 0 {
                    continue;
                }
                let n = t.shape[axis] as f64;
                let ca = f64::from(c[axis]);
                #[allow(clippy::cast_possible_truncation)]
                let count = ((start[axis] + ca) / n).floor().abs() as i64;
                for hit in 0..count {
                    let plus = (c[axis] > 0) == (hit % 2 == 0);
                    let boundary = if c[axis] > 0 { (hit + 1) as f64 * n } else { -(hit as f64) * n };
                    let fraction = (boundary - start[axis]) / ca;
                    let mut position: [f64; 3] =
                        std::array::from_fn(|d| start[d] + fraction * f64::from(c[d]));
                    for a in 0..3 {
                        let length = t.shape[a] as f64;
                        if t.periodic[a] {
                            position[a] = position[a].rem_euclid(length);
                        } else {
                            let folded = position[a].rem_euclid(2.0 * length);
                            position[a] = folded.min(2.0 * length - folded);
                        }
                    }
                    let mut impulse = [0.0; 3];
                    impulse[axis] = if plus { 1.0 } else { -1.0 } * 2.0 * ca.abs();
                    let images = periodic_seam_images(position, t.shape, t.periodic);
                    if ev.source_flat_indices.len() + images.len() > maximum_events {
                        return Err(err("Specular wall events exceed the explicit budget"));
                    }
                    let share = images.len() as f64;
                    for image in images {
                        ev.source_flat_indices.push(cell * q + vq);
                        ev.positions_lattice.push(image.map(|v| v - 0.5));
                        ev.impulse_per_population.push(impulse.map(|v| v / share));
                        ev.face_indices.push(2 * axis + usize::from(plus));
                        ev.interval_fraction.push(fraction);
                    }
                }
            }
        }
    }
    Ok(ev)
}

fn empty_events() -> WallEvents {
    WallEvents {
        source_flat_indices: Vec::new(),
        positions_lattice: Vec::new(),
        impulse_per_population: Vec::new(),
        face_indices: Vec::new(),
        interval_fraction: Vec::new(),
        localization: "symmetric_periodic_seams_v2".into(),
    }
}


pub fn slab_wall_events(t: &Transport, maximum_events: usize) -> CaeResult<WallEvents> {
    if maximum_events < 1 {
        return Err(err("Positive integer slab wall-event budget required"));
    }
    let TransportKind::Slab { axis, .. } = t.kind else {
        return Err(err("This transport does not support slab wall events"));
    };
    let velocities = &t.lattice.data().velocities;
    let q = velocities.len();
    let n = t.shape[axis] as f64;
    let mut ev = empty_events();
    for (cell, x) in coordinates(t.shape).iter().enumerate() {
        let start: [f64; 3] = x.map(|v| v as f64 + 0.5);
        for (vq, v) in velocities.iter().enumerate() {
            if v[axis] == 0 {
                continue;
            }
            let va = f64::from(v[axis]);
            #[allow(clippy::cast_possible_truncation)]
            let count = ((start[axis] + va) / n).floor().abs() as i64;
            for hit in 0..count {
                let plus = (v[axis] > 0) == (hit % 2 == 0);
                let plane = if plus { n } else { 0.0 };
                let mut position: [f64; 3] =
                    std::array::from_fn(|d| start[d] + f64::from(v[d]) * ((plane - start[axis]) / va));
                for a in 0..3 {
                    if a != axis {
                        position[a] = position[a].rem_euclid(t.shape[a] as f64);
                    }
                }
                let unfolded = if v[axis] > 0 { (hit + 1) as f64 * n } else { -(hit as f64) * n };
                let fraction = (unfolded - start[axis]) / va;
                let images = periodic_seam_images(position, t.shape, t.periodic);
                if ev.source_flat_indices.len() + images.len() > maximum_events {
                    return Err(err("Slab wall events exceed the explicit budget"));
                }
                let sign = if hit % 2 == 0 { 1.0 } else { -1.0 };
                let share = images.len() as f64;
                for image in images {
                    ev.source_flat_indices.push(cell * q + vq);
                    ev.positions_lattice.push(image.map(|c| c - 0.5));
                    ev.impulse_per_population
                        .push(std::array::from_fn(|d| 2.0 * f64::from(v[d]) * sign / share));
                    ev.face_indices.push(2 * axis + usize::from(plus));
                    ev.interval_fraction.push(fraction);
                }
            }
        }
    }
    Ok(ev)
}

#[must_use]
pub fn wall_event_impulses(f: &[f64], events: &WallEvents) -> Vec<[f64; 3]> {
    events
        .source_flat_indices
        .iter()
        .zip(&events.impulse_per_population)
        .map(|(s, w)| w.map(|v| f[*s] * v))
        .collect()
}

pub fn wall_event_impulses_vjp(events: &WallEvents, bar: &[[f64; 3]], f_bar: &mut [f64]) {
    for ((s, w), b) in events.source_flat_indices.iter().zip(&events.impulse_per_population).zip(bar) {
        f_bar[*s] += w[0] * b[0] + w[1] * b[1] + w[2] * b[2];
    }
}
