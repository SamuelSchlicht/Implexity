// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    pub nelx: usize,
    pub nely: usize,
    pub nelz: usize,
    pub h: f64,
    pub origin: [f64; 3],
}

impl Grid {
    #[must_use]
    pub fn new(nelx: usize, nely: usize, nelz: usize, h: f64) -> Self {
        Self { nelx, nely, nelz, h, origin: [0.0; 3] }
    }

    #[must_use]
    pub fn shape(&self) -> [usize; 3] {
        [self.nelx, self.nely, self.nelz]
    }

    #[must_use]
    pub fn node_shape(&self) -> [usize; 3] {
        [self.nelx + 1, self.nely + 1, self.nelz + 1]
    }

    #[must_use]
    pub fn nel(&self) -> usize {
        self.nelx * self.nely * self.nelz
    }

    #[must_use]
    pub fn nnode(&self) -> usize {
        (self.nelx + 1) * (self.nely + 1) * (self.nelz + 1)
    }

    #[must_use]
    pub fn extent(&self) -> [f64; 3] {
        [self.nelx as f64 * self.h, self.nely as f64 * self.h, self.nelz as f64 * self.h]
    }

    #[must_use]
    pub fn counts(&self) -> [usize; 3] {
        self.shape()
    }

    #[must_use]
    pub fn element_axis(&self, axis: usize) -> Vec<f64> {
        let n = self.counts()[axis];
        (0..n).map(|i| self.origin[axis] + (i as f64 + 0.5) * self.h).collect()
    }

    #[must_use]
    pub fn node_axis(&self, axis: usize) -> Vec<f64> {
        let n = self.counts()[axis];
        (0..=n).map(|i| self.origin[axis] + i as f64 * self.h).collect()
    }

    #[must_use]
    pub fn cell_volume(&self) -> f64 {
        self.h.powi(3)
    }

    #[must_use]
    pub fn face_area(&self) -> f64 {
        self.h.powi(2)
    }

    #[must_use]
    pub fn describe(&self) -> String {
        let e = self.extent();
        format!(
            "{}x{}x{} elements, h = {:.4} mm, domain {:.2} x {:.2} x {:.2} mm",
            self.nelx,
            self.nely,
            self.nelz,
            1e3 * self.h,
            1e3 * e[0],
            1e3 * e[1],
            1e3 * e[2]
        )
    }
}

#[must_use]
pub fn flat(s: [usize; 3], i: usize, j: usize, k: usize) -> usize {
    (i * s[1] + j) * s[2] + k
}

pub fn for_box(lo: [usize; 3], hi: [usize; 3], mut f: impl FnMut(usize, usize, usize)) {
    for i in lo[0]..hi[0] {
        for j in lo[1]..hi[1] {
            for k in lo[2]..hi[2] {
                f(i, j, k);
            }
        }
    }
}

#[must_use]
pub fn node_index(ix: usize, iy: usize, iz: usize, nely: usize, nelz: usize) -> usize {
    (ix * (nely + 1) + iy) * (nelz + 1) + iz
}

#[must_use]
pub fn element_index(ex: usize, ey: usize, ez: usize, nely: usize, nelz: usize) -> usize {
    (ex * nely + ey) * nelz + ez
}
