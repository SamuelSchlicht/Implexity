// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

// Copyright 2009-2022 the scikit-image team.




#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

#[rustfmt::skip]
mod luts;

use crate::MeshError;
use crate::grid::Field3;

const TINY: f64 = f64::EPSILON;

pub(crate) struct Lut {
    dims: [usize; 3],
    values: &'static [i8],
}

impl Lut {
    fn get1(&self, i0: usize) -> i32 {
        i32::from(self.values[i0])
    }
    fn get2(&self, i0: usize, i1: usize) -> i32 {
        i32::from(self.values[i0 * self.dims[1] + i1])
    }
    fn get3(&self, i0: usize, i1: usize, i2: usize) -> i32 {
        i32::from(self.values[i0 * self.dims[1] * self.dims[2] + i1 * self.dims[2] + i2])
    }
}

const EDGE_REL_X: [[i32; 2]; 12] =
    [[0, 1], [1, 1], [1, 0], [0, 0], [0, 1], [1, 1], [1, 0], [0, 0], [0, 0], [1, 1], [1, 1], [0, 0]];
const EDGE_REL_Y: [[i32; 2]; 12] =
    [[0, 0], [0, 1], [1, 1], [1, 0], [0, 0], [0, 1], [1, 1], [1, 0], [0, 0], [0, 0], [1, 1], [1, 1]];
const EDGE_REL_Z: [[i32; 2]; 12] =
    [[0, 0], [0, 0], [0, 0], [0, 0], [1, 1], [1, 1], [1, 1], [1, 1], [0, 1], [0, 1], [0, 1], [0, 1]];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct McMesh {
    pub vertices: Vec<[f32; 3]>,
    pub faces: Vec<[u32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub values: Vec<f32>,
}

impl McMesh {
    #[must_use]
    pub fn len(&self) -> usize {
        self.faces.len()
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GradientDirection {
    Descent,
    Ascent,
}




pub fn marching_cubes(
    volume: &Field3<'_>,
    level: f64,
    direction: GradientDirection,
    allow_degenerate: bool,
) -> Result<McMesh, MeshError> {
    let [n0, n1, n2] = volume.shape;
    if n0 < 2 || n1 < 2 || n2 < 2 {
        return Err(MeshError::invalid("Input array must be at least 2x2x2."));
    }
    let im: Vec<f32> = volume.data.iter().map(|&v| v as f32).collect();
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    for &v in &im {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if level < f64::from(lo) || level > f64::from(hi) {
        return Err(MeshError::invalid("Surface level must be within volume data range."));
    }
    let mut mesh = march(&im, [n0, n1, n2], level);
    if mesh.vertices.is_empty() {
        return Err(MeshError::invalid("No surface found at the given iso value."));
    }
    if direction == GradientDirection::Descent {
        for f in &mut mesh.faces {
            f.swap(0, 2);
        }
    }
    if !allow_degenerate {
        mesh = remove_degenerate_faces(mesh);
    }
    Ok(mesh)
}

fn march(im: &[f32], shape: [usize; 3], isovalue: f64) -> McMesh {
    let (nz, ny, nx) = (shape[0], shape[1], shape[2]);
    let at = |z: usize, y: usize, x: usize| f64::from(im[(z * ny + y) * nx + x]);
    let mut cell = Cell::new(nx, ny);
    for z in 0..nz - 1 {
        cell.new_z_value();
        for y in 0..ny - 1 {
            for x in 0..nx - 1 {
                let corners = [
                    at(z, y, x),
                    at(z, y, x + 1),
                    at(z, y + 1, x + 1),
                    at(z, y + 1, x),
                    at(z + 1, y, x),
                    at(z + 1, y, x + 1),
                    at(z + 1, y + 1, x + 1),
                    at(z + 1, y + 1, x),
                ];
                cell.set_cube(isovalue, x, y, z, corners);
                let case = luts::CASES.get2(cell.index, 0);
                if case > 0 {
                    let config = usize::try_from(luts::CASES.get2(cell.index, 1)).unwrap_or(0);
                    the_big_switch(&mut cell, case, config);
                }
            }
        }
    }
    cell.finish()
}

struct Cell {
    x: usize,
    y: usize,
    z: usize,
    v: [f64; 8],
    vv: [f64; 8],
    vg: [f64; 24],
    vmax: f64,
    v12: [f64; 3],
    v12g: [f64; 3],
    v12_calculated: bool,
    index: usize,
    nx: usize,
    layer1: Vec<i32>,
    layer2: Vec<i32>,
    vertices: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    values: Vec<f32>,
    faces: Vec<i32>,
}

impl Cell {
    fn new(nx: usize, ny: usize) -> Self {
        Self {
            x: 0,
            y: 0,
            z: 0,
            v: [0.0; 8],
            vv: [0.0; 8],
            vg: [0.0; 24],
            vmax: 0.0,
            v12: [0.0; 3],
            v12g: [0.0; 3],
            v12_calculated: false,
            index: 0,
            nx,
            layer1: vec![-1; nx * ny * 4],
            layer2: vec![-1; nx * ny * 4],
            vertices: Vec::new(),
            normals: Vec::new(),
            values: Vec::new(),
            faces: Vec::new(),
        }
    }

    fn new_z_value(&mut self) {
        std::mem::swap(&mut self.layer1, &mut self.layer2);
        self.layer2.fill(-1);
    }

    fn set_cube(&mut self, isovalue: f64, x: usize, y: usize, z: usize, corners: [f64; 8]) {
        self.x = x;
        self.y = y;
        self.z = z;
        let mut index = 0;
        for (i, c) in corners.iter().enumerate() {
            self.v[i] = c - isovalue;
            if self.v[i] > 0.0 {
                index += 1 << i;
            }
        }
        self.index = index;
        self.v12_calculated = false;
    }

    fn add_vertex(&mut self, x: f32, y: f32, z: f32) -> i32 {
        self.vertices.push([x, y, z]);
        self.normals.push([0.0; 3]);
        self.values.push(0.0);
        i32::try_from(self.vertices.len() - 1).unwrap_or(i32::MAX)
    }

    fn add_gradient(&mut self, vertex: i32, g: [f32; 3]) {
        let n = &mut self.normals[vertex as usize];
        n[0] += g[0];
        n[1] += g[1];
        n[2] += g[2];
    }

    fn add_gradient_from_index(&mut self, vertex: i32, i: usize, strength: f32) {
        let s = f64::from(strength);
        let g =
            [(self.vg[i * 3] * s) as f32, (self.vg[i * 3 + 1] * s) as f32, (self.vg[i * 3 + 2] * s) as f32];
        self.add_gradient(vertex, g);
    }

    fn add_face(&mut self, index: i32) {
        self.faces.push(index);
        let slot = &mut self.values[index as usize];
        if self.vmax > f64::from(*slot) {
            *slot = self.vmax as f32;
        }
    }

    fn add_triangles(&mut self, lut: &Lut, lut_index: usize, nt: usize) {
        self.prepare_for_adding_triangles();
        for i in 0..nt {
            for j in 0..3 {
                let vi = lut.get2(lut_index, i * 3 + j);
                self.add_face_from_edge_index(vi);
            }
        }
    }

    fn add_triangles2(&mut self, lut: &Lut, lut_index: usize, lut_index2: usize, nt: usize) {
        self.prepare_for_adding_triangles();
        for i in 0..nt {
            for j in 0..3 {
                let vi = lut.get3(lut_index, lut_index2, i * 3 + j);
                self.add_face_from_edge_index(vi);
            }
        }
    }

    fn add_face_from_edge_index(&mut self, vi: i32) {
        let (second, slot) = self.index_in_face_layer(vi);
        let existing = if second { self.layer2[slot] } else { self.layer1[slot] };
        if vi == 12 {
            if !self.v12_calculated {
                self.calculate_center_vertex();
            }
            let g = [self.v12g[0] as f32, self.v12g[1] as f32, self.v12g[2] as f32];
            let vertex = if existing >= 0 {
                existing
            } else {
                let created = self.add_vertex(self.v12[0] as f32, self.v12[1] as f32, self.v12[2] as f32);
                self.set_layer(second, slot, created);
                created
            };
            self.add_face(vertex);
            self.add_gradient(vertex, g);
            return;
        }
        let e = usize::try_from(vi).unwrap_or(0);
        let (dx1, dx2) = (EDGE_REL_X[e][0], EDGE_REL_X[e][1]);
        let (dy1, dy2) = (EDGE_REL_Y[e][0], EDGE_REL_Y[e][1]);
        let (dz1, dz2) = (EDGE_REL_Z[e][0], EDGE_REL_Z[e][1]);
        let index1 = usize::try_from(dz1 * 4 + dy1 * 2 + dx1).unwrap_or(0);
        let index2 = usize::try_from(dz2 * 4 + dy2 * 2 + dx2).unwrap_or(0);
        let tmpf1 = 1.0 / (TINY + self.vv[index1].abs());
        let tmpf2 = 1.0 / (TINY + self.vv[index2].abs());
        let vertex = if existing >= 0 {
            existing
        } else {
            let (mut fx, mut fy, mut fz, mut ff) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
            fx += f64::from(dx1) * tmpf1;
            fy += f64::from(dy1) * tmpf1;
            fz += f64::from(dz1) * tmpf1;
            ff += tmpf1;
            fx += f64::from(dx2) * tmpf2;
            fy += f64::from(dy2) * tmpf2;
            fz += f64::from(dz2) * tmpf2;
            ff += tmpf2;
            let created = self.add_vertex(
                (self.x as f64 + fx / ff) as f32,
                (self.y as f64 + fy / ff) as f32,
                (self.z as f64 + fz / ff) as f32,
            );
            self.set_layer(second, slot, created);
            created
        };
        self.add_face(vertex);
        self.add_gradient_from_index(vertex, index1, tmpf1 as f32);
        self.add_gradient_from_index(vertex, index2, tmpf2 as f32);
    }

    fn set_layer(&mut self, second: bool, slot: usize, value: i32) {
        if second {
            self.layer2[slot] = value;
        } else {
            self.layer1[slot] = value;
        }
    }

    fn index_in_face_layer(&self, vi: i32) -> (bool, usize) {
        let mut i = self.nx * self.y + self.x;
        let mut j = 0;
        let mut second = false;
        if vi < 8 {
            let mut v = vi;
            if v >= 4 {
                v -= 4;
                second = true;
            }
            match v {
                1 => {
                    i += 1;
                    j = 1;
                }
                2 => i += self.nx,
                3 => j = 1,
                _ => {}
            }
        } else if vi < 12 {
            j = 2;
            match vi {
                9 => i += 1,
                10 => i += self.nx + 1,
                11 => i += self.nx,
                _ => {}
            }
        } else {
            j = 3;
        }
        (second, 4 * i + j)
    }

    fn prepare_for_adding_triangles(&mut self) {
        let v = self.v;
        self.vv = [v[0], v[1], v[3], v[2], v[4], v[5], v[7], v[6]];
        let (mut vmin, mut vmax) = (0.0_f64, 0.0_f64);
        for &x in &self.vv {
            if x > vmax {
                vmax = x;
            }
            if x < vmin {
                vmin = x;
            }
        }
        self.vmax = vmax - vmin;
        let g = [
            [v[0] - v[1], v[0] - v[3], v[0] - v[4]],
            [v[0] - v[1], v[1] - v[2], v[1] - v[5]],
            [v[3] - v[2], v[1] - v[2], v[2] - v[6]],
            [v[3] - v[2], v[0] - v[3], v[3] - v[7]],
            [v[4] - v[5], v[4] - v[7], v[0] - v[4]],
            [v[4] - v[5], v[5] - v[6], v[1] - v[5]],
            [v[7] - v[6], v[5] - v[6], v[2] - v[6]],
            [v[7] - v[6], v[4] - v[7], v[3] - v[7]],
        ];
        for (k, row) in g.iter().enumerate() {
            self.vg[k * 3..k * 3 + 3].copy_from_slice(row);
        }
    }

    fn calculate_center_vertex(&mut self) {
        let s: [f64; 8] = std::array::from_fn(|i| 1.0 / (TINY + self.v[i].abs()));
        let pos = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        let (mut fx, mut fy, mut fz, mut ff) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
        for k in 0..8 {
            fx += pos[k][0] * s[k];
            fy += pos[k][1] * s[k];
            fz += pos[k][2] * s[k];
            ff += s[k];
        }
        self.v12 = [self.x as f64 + fx / ff, self.y as f64 + fy / ff, self.z as f64 + fz / ff];
        let vg = &self.vg;
        let comp = |c: usize| {
            s[0] * vg[c]
                + s[1] * vg[3 + c]
                + s[2] * vg[6 + c]
                + s[3] * vg[9 + c]
                + s[4] * vg[12 + c]
                + s[5] * vg[15 + c]
                + s[6] * vg[18 + c]
                + s[7] * vg[21 + c]
        };
        self.v12g = [comp(0), comp(1), comp(2)];
        self.v12_calculated = true;
    }

    fn finish(self) -> McMesh {
        let vertices = self.vertices.iter().map(|v| [v[2], v[1], v[0]]).collect();
        let normals = self
            .normals
            .iter()
            .map(|n| {
                let mut length = 0.0_f64;
                for &c in n {
                    let d = f64::from(c);
                    length += d * d;
                }
                if length > 0.0 {
                    length = 1.0 / length.powf(0.5);
                }
                [
                    (f64::from(n[2]) * length) as f32,
                    (f64::from(n[1]) * length) as f32,
                    (f64::from(n[0]) * length) as f32,
                ]
            })
            .collect();
        let faces = self.faces.chunks_exact(3).map(|t| [t[0] as u32, t[1] as u32, t[2] as u32]).collect();
        McMesh { vertices, faces, normals, values: self.values }
    }
}

fn test_face(cell: &Cell, face: i32) -> bool {
    let v = &cell.v;
    let (a, b, c, d) = match face.abs() {
        1 => (v[0], v[4], v[5], v[1]),
        2 => (v[1], v[5], v[6], v[2]),
        3 => (v[2], v[6], v[7], v[3]),
        4 => (v[3], v[7], v[4], v[0]),
        5 => (v[0], v[3], v[2], v[1]),
        6 => (v[4], v[7], v[6], v[5]),
        _ => (0.0, 0.0, 0.0, 0.0),
    };
    let ac_bd = a * c - b * d;
    if ac_bd > -TINY && ac_bd < TINY { face >= 0 } else { f64::from(face) * a * ac_bd >= 0.0 }
}

#[allow(clippy::too_many_lines)]
fn test_internal(cell: &Cell, case: i32, config: usize, subconfig: usize, s: i32) -> bool {
    let v = &cell.v;
    let (at, bt, ct, dt);
    if case == 4 || case == 10 {
        let a = (v[4] - v[0]) * (v[6] - v[2]) - (v[7] - v[3]) * (v[5] - v[1]);
        let b = v[2] * (v[4] - v[0]) + v[0] * (v[6] - v[2]) - v[1] * (v[7] - v[3]) - v[3] * (v[5] - v[1]);
        let t = -b / (2.0 * a + TINY);
        if !(0.0..=1.0).contains(&t) {
            return s > 0;
        }
        at = v[0] + (v[4] - v[0]) * t;
        bt = v[3] + (v[7] - v[3]) * t;
        ct = v[2] + (v[6] - v[2]) * t;
        dt = v[1] + (v[5] - v[1]) * t;
    } else {
        let edge = match case {
            6 => luts::TEST6.get2(config, 2),
            7 => luts::TEST7.get2(config, 4),
            12 => luts::TEST12.get2(config, 3),
            13 => luts::TILING13_5_1.get3(config, subconfig, 0),
            _ => -1,
        };
        let spec: Option<[usize; 8]> = match edge {
            0 => Some([0, 1, 3, 2, 7, 6, 4, 5]),
            1 => Some([1, 2, 0, 3, 4, 7, 5, 6]),
            2 => Some([2, 3, 1, 0, 5, 4, 6, 7]),
            3 => Some([3, 0, 2, 1, 6, 5, 7, 4]),
            4 => Some([4, 5, 7, 6, 3, 2, 0, 1]),
            5 => Some([5, 6, 4, 7, 0, 3, 1, 2]),
            6 => Some([6, 7, 5, 4, 1, 0, 2, 3]),
            7 => Some([7, 4, 6, 5, 2, 1, 3, 0]),
            8 => Some([0, 4, 3, 7, 2, 6, 1, 5]),
            9 => Some([1, 5, 0, 4, 3, 7, 2, 6]),
            10 => Some([2, 6, 1, 5, 0, 4, 3, 7]),
            11 => Some([3, 7, 2, 6, 1, 5, 0, 4]),
            _ => None,
        };
        if let Some([p, q, r0, r1, s0, s1, u0, u1]) = spec {
            let t = v[p] / (v[p] - v[q] + TINY);
            at = 0.0;
            bt = v[r0] + (v[r1] - v[r0]) * t;
            ct = v[s0] + (v[s1] - v[s0]) * t;
            dt = v[u0] + (v[u1] - v[u0]) * t;
        } else {
            {

                at = 0.0;
                bt = 0.0;
                ct = 0.0;
                dt = 0.0;
            }
        }
    }
    let mut test = 0;
    if at >= 0.0 {
        test += 1;
    }
    if bt >= 0.0 {
        test += 2;
    }
    if ct >= 0.0 {
        test += 4;
    }
    if dt >= 0.0 {
        test += 8;
    }
    match test {
        0..=4 | 6 | 8 | 9 | 12 => s > 0,

        5 => at * ct - bt * dt < TINY && s > 0,
        10 => at * ct - bt * dt >= TINY && s > 0,
        _ => s < 0,
    }
}

#[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
fn the_big_switch(cell: &mut Cell, case: i32, config: usize) {
    #[allow(clippy::wildcard_imports)]
    use luts::*;
    match case {
        1 => cell.add_triangles(&TILING1, config, 1),
        2 => cell.add_triangles(&TILING2, config, 2),
        3 => {
            if test_face(cell, TEST3.get1(config)) {
                cell.add_triangles(&TILING3_2, config, 4);
            } else {
                cell.add_triangles(&TILING3_1, config, 2);
            }
        }
        4 => {
            if test_internal(cell, case, config, 0, TEST4.get1(config)) {
                cell.add_triangles(&TILING4_1, config, 2);
            } else {
                cell.add_triangles(&TILING4_2, config, 6);
            }
        }
        5 => cell.add_triangles(&TILING5, config, 3),
        6 => {
            if test_face(cell, TEST6.get2(config, 0)) {
                cell.add_triangles(&TILING6_2, config, 5);
            } else if test_internal(cell, case, config, 0, TEST6.get2(config, 1)) {
                cell.add_triangles(&TILING6_1_1, config, 3);
            } else {
                cell.add_triangles(&TILING6_1_2, config, 9);
            }
        }
        7 => {
            let mut sub = 0;
            if test_face(cell, TEST7.get2(config, 0)) {
                sub += 1;
            }
            if test_face(cell, TEST7.get2(config, 1)) {
                sub += 2;
            }
            if test_face(cell, TEST7.get2(config, 2)) {
                sub += 4;
            }
            match sub {
                0 => cell.add_triangles(&TILING7_1, config, 3),
                1 => cell.add_triangles2(&TILING7_2, config, 0, 5),
                2 => cell.add_triangles2(&TILING7_2, config, 1, 5),
                3 => cell.add_triangles2(&TILING7_3, config, 0, 9),
                4 => cell.add_triangles2(&TILING7_2, config, 2, 5),
                5 => cell.add_triangles2(&TILING7_3, config, 1, 9),
                6 => cell.add_triangles2(&TILING7_3, config, 2, 9),
                _ => {
                    if test_internal(cell, case, config, sub, TEST7.get2(config, 3)) {
                        cell.add_triangles(&TILING7_4_2, config, 9);
                    } else {
                        cell.add_triangles(&TILING7_4_1, config, 5);
                    }
                }
            }
        }
        8 => cell.add_triangles(&TILING8, config, 2),
        9 => cell.add_triangles(&TILING9, config, 4),
        10 | 12 => {
            let (test, t11_, t2, t2_, t11, t12) = if case == 10 {
                (&TEST10, &TILING10_1_1_, &TILING10_2, &TILING10_2_, &TILING10_1_1, &TILING10_1_2)
            } else {
                (&TEST12, &TILING12_1_1_, &TILING12_2, &TILING12_2_, &TILING12_1_1, &TILING12_1_2)
            };
            if test_face(cell, test.get2(config, 0)) {
                if test_face(cell, test.get2(config, 1)) {
                    cell.add_triangles(t11_, config, 4);
                } else {
                    cell.add_triangles(t2, config, 8);
                }
            } else if test_face(cell, test.get2(config, 1)) {
                cell.add_triangles(t2_, config, 8);
            } else if test_internal(cell, case, config, 0, test.get2(config, 2)) {
                cell.add_triangles(t11, config, 4);
            } else {
                cell.add_triangles(t12, config, 8);
            }
        }
        11 => cell.add_triangles(&TILING11, config, 4),
        13 => {
            let mut sub = 0usize;
            for k in 0..6 {
                if test_face(cell, TEST13.get2(config, k)) {
                    sub += 1 << k;
                }
            }
            let sub = usize::try_from(SUBCONFIG13.get1(sub)).unwrap_or(usize::MAX);
            match sub {
                0 => cell.add_triangles(&TILING13_1, config, 4),
                1..=6 => cell.add_triangles2(&TILING13_2, config, sub - 1, 6),
                7..=18 => cell.add_triangles2(&TILING13_3, config, sub - 7, 10),
                19..=22 => cell.add_triangles2(&TILING13_4, config, sub - 19, 12),
                23..=26 => {
                    let k = sub - 23;
                    if test_internal(cell, case, config, k, TEST13.get2(config, 6)) {
                        cell.add_triangles2(&TILING13_5_1, config, k, 6);
                    } else {
                        cell.add_triangles2(&TILING13_5_2, config, k, 10);
                    }
                }
                27..=38 => cell.add_triangles2(&TILING13_3_, config, sub - 27, 10),
                39..=44 => cell.add_triangles2(&TILING13_2_, config, sub - 39, 6),
                45 => cell.add_triangles(&TILING13_1_, config, 4),

                _ => {}
            }
        }
        14 => cell.add_triangles(&TILING14, config, 4),
        _ => {}
    }
}

#[allow(clippy::float_cmp, clippy::needless_pass_by_value)]
fn remove_degenerate_faces(mesh: McMesh) -> McMesh {
    let n = mesh.vertices.len();
    let mut map1: Vec<usize> = (0..n).collect();
    let mut ok = vec![true; mesh.faces.len()];
    let v = &mesh.vertices;
    for (j, f) in mesh.faces.iter().enumerate() {
        let (i1, i2, i3) = (f[0] as usize, f[1] as usize, f[2] as usize);
        if v[i1] == v[i2] {
            let m = map1[i1].min(map1[i2]);
            map1[i1] = m;
            map1[i2] = m;
            ok[j] = false;
        }
        if v[i1] == v[i3] {
            let m = map1[i1].min(map1[i3]);
            map1[i1] = m;
            map1[i3] = m;
            ok[j] = false;
        }
        if v[i2] == v[i3] {
            let m = map1[i2].min(map1[i3]);
            map1[i2] = m;
            map1[i3] = m;
            ok[j] = false;
        }
    }
    let keep: Vec<bool> = (0..n).map(|i| map1[i] == i).collect();

    let mut map2 = vec![0i64; n];
    let mut acc = 0i64;
    for i in 0..n {
        if keep[i] {
            acc += 1;
        }
        map2[i] = acc - 1;
    }
    let kept = usize::try_from(acc).unwrap_or(0);
    let wrap = |k: i64| -> u32 {
        let idx = if k < 0 { k + i64::try_from(kept).unwrap_or(0) } else { k };
        u32::try_from(idx).unwrap_or(0)
    };
    let faces = mesh
        .faces
        .iter()
        .zip(&ok)
        .filter(|(_, o)| **o)
        .map(|(f, _)| {
            [
                wrap(map2[map1[f[0] as usize]]),
                wrap(map2[map1[f[1] as usize]]),
                wrap(map2[map1[f[2] as usize]]),
            ]
        })
        .collect();
    let pick = |i: usize| keep[i];
    McMesh {
        vertices: mesh.vertices.iter().enumerate().filter(|(i, _)| pick(*i)).map(|(_, v)| *v).collect(),
        faces,
        normals: mesh.normals.iter().enumerate().filter(|(i, _)| pick(*i)).map(|(_, v)| *v).collect(),
        values: mesh.values.iter().enumerate().filter(|(i, _)| pick(*i)).map(|(_, v)| *v).collect(),
    }
}
