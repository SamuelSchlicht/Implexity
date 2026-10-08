// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use implexity_solve::nonmatching_interface::InterfaceProjection;

use super::sp::{self, Sp, SpMap};

fn err(msg: impl Into<String>) -> CaeError {
    CaeError::contract(msg.into())
}

fn vector(value: &[f64], size: usize, name: &str, nonnegative: bool) -> CaeResult<Vec<f64>> {
    let a = if value.len() == 1 && size != 1 { vec![value[0]; size] } else { value.to_vec() };
    if a.len() != size || a.iter().any(|v| !v.is_finite()) {
        return Err(err(format!("{name}: finite scalar or vector of size {size} required")));
    }
    if nonnegative && a.iter().any(|v| *v < 0.0) {
        return Err(err(format!("{name}: invalid sign")));
    }
    Ok(a)
}

#[derive(Clone, Debug)]
pub struct TraceLoads<S> {
    pub solid_load_n: Vec<[S; 3]>,
    pub fluid_reaction_n: Vec<[S; 3]>,
    pub traction_on_solid_pa: Vec<[S; 3]>,
    pub fluid_absolute_pressure_pa: Vec<S>,
}

#[derive(Clone, Debug)]
pub struct PressureTrace {
    pub left: Sp,
    pub right: Sp,
    lmap: SpMap,
    rmap: SpMap,
    pub measure: Vec<f64>,
    pub normals: Vec<[f64; 3]>,
    pub reference: Vec<f64>,
    pub exterior: Vec<f64>,
    pub shear: Vec<[f64; 3]>,
}

impl PressureTrace {

    pub fn new(
        left: Sp,
        right: Sp,
        measure: Vec<f64>,
        solid_coordinates: &[[f64; 3]],
        fluid_coordinates: &[[f64; 3]],
        interface_coordinates: &[[f64; 3]],
        normals: Vec<[f64; 3]>,
        reference: f64,
        exterior: f64,
        shear: Vec<[f64; 3]>,
        tolerance: f64,
        provenance: &str,
    ) -> CaeResult<Self> {
        let projection = InterfaceProjection::new(&left, &right, &measure)?;
        let _ = projection;
        if provenance.trim().is_empty() {
            return Err(err("actual geometry provenance required"));
        }
        if !tolerance.is_finite() || tolerance <= 0.0 {
            return Err(err("explicit positive coordinate tolerance required"));
        }
        let ni = measure.len();
        let coords = |a: &[[f64; 3]], n: usize| {
            if a.len() != n || a.iter().flatten().any(|v| !v.is_finite()) {
                return Err(err("finite xyz coordinates required"));
            }
            Ok(())
        };
        coords(solid_coordinates, left.ncols())?;
        coords(fluid_coordinates, right.ncols())?;
        coords(interface_coordinates, ni)?;
        let this = Self {
            lmap: SpMap::new(&left),
            rmap: SpMap::new(&right),
            left,
            right,
            measure,
            normals,
            reference: vec![reference; ni],
            exterior: vec![exterior; ni],
            shear,
        };
        for a in 0..3 {
            let xs: Vec<f64> = solid_coordinates.iter().map(|p| p[a]).collect();
            let xf: Vec<f64> = fluid_coordinates.iter().map(|p| p[a]).collect();
            let li = sp::mv(&this.left, &xs)?;
            let ri = sp::mv(&this.right, &xf)?;
            for (k, target) in interface_coordinates.iter().enumerate() {
                if (li[k] - target[a]).abs() > tolerance || (ri[k] - target[a]).abs() > tolerance {
                    return Err(err("both traces must reproduce actual interface coordinates"));
                }
            }
        }

        this.admit(&vec![1.0; this.right.ncols()])?;
        Ok(this)
    }


    pub fn admit(&self, pressure: &[f64]) -> CaeResult<()> {
        if pressure.len() != self.right.ncols() || pressure.iter().any(|v| !v.is_finite()) {
            return Err(err("actual finite fluid gauge pressure required"));
        }
        let n = self.measure.len();
        let gauge = vector(&sp::mv(&self.right, pressure)?, n, "gauge pressure", false)?;
        let reference = vector(&self.reference, n, "absolute pressure reference", true)?;
        vector(&self.exterior, n, "absolute external pressure", true)?;
        if gauge.iter().zip(&reference).any(|(g, r)| g + r <= 0.0) {
            return Err(err("fluid absolute pressure must be positive"));
        }
        if self.normals.len() != n
            || self.shear.len() != n
            || self.normals.iter().chain(&self.shear).flatten().any(|v| !v.is_finite())
        {
            return Err(err("finite normals and viscous traction with shape (interface samples,3) required"));
        }
        if self.normals.iter().any(|v| ((v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt() - 1.0).abs() > 1e-10)
        {
            return Err(err("solid-outward normals must have unit length; do not include area in normals"));
        }
        Ok(())
    }

    #[must_use]
    pub fn loads<S: Scalar>(&self, pressure: &[S]) -> TraceLoads<S> {
        let pg = self.rmap.apply(pressure);
        let absolute: Vec<S> = pg.iter().zip(&self.reference).map(|(p, r)| *p + *r).collect();
        let traction: Vec<[S; 3]> = absolute
            .iter()
            .zip(&self.normals)
            .zip(&self.shear)
            .map(|((p, n), s)| std::array::from_fn(|a| -(*p) * n[a] + s[a]))
            .collect();
        let q: Vec<[S; 3]> = traction.iter().zip(&self.measure).map(|(t, m)| t.map(|v| v * *m)).collect();
        let reaction = self.rmap.apply_t_vec(&q).into_iter().map(|v| v.map(|x| -x)).collect();
        TraceLoads {
            solid_load_n: self.lmap.apply_t_vec(&q),
            fluid_reaction_n: reaction,
            traction_on_solid_pa: traction,
            fluid_absolute_pressure_pa: absolute,
        }
    }

    #[must_use]
    pub fn viscous_loads<S: Scalar>(&self, stress: &[[S; 9]]) -> (Vec<[S; 3]>, Vec<[S; 3]>) {
        let interface: Vec<[S; 9]> = self.rmap.apply_vec(stress);
        let forces: Vec<[S; 3]> = interface
            .iter()
            .zip(&self.normals)
            .zip(&self.measure)
            .map(|((s, n), m)| {
                std::array::from_fn(|a| (s[3 * a] * n[0] + s[3 * a + 1] * n[1] + s[3 * a + 2] * n[2]) * *m)
            })
            .collect();
        let reaction = self.rmap.apply_t_vec(&forces).into_iter().map(|v| v.map(|x| -x)).collect();
        (self.lmap.apply_t_vec(&forces), reaction)
    }


    pub fn viscous_force_matrix(&self) -> CaeResult<Sp> {
        let count = self.measure.len();
        let (mut r, mut c, mut v) = (Vec::new(), Vec::new(), Vec::new());
        for i in 0..count {
            for a in 0..3 {
                for b in 0..3 {
                    r.push(3 * i + a);
                    c.push(9 * i + 3 * a + b);
                    v.push(self.measure[i] * self.normals[i][b]);
                }
            }
        }
        let contraction = sp::triplets(3 * count, 9 * count, &r, &c, &v)?;
        sp::mm3(&sp::kron_eye(&sp::t(&self.left), 3)?, &contraction, &sp::kron_eye(&self.right, 9)?)
    }


    pub fn pressure_force_matrix(&self) -> CaeResult<Sp> {
        let count = self.measure.len();
        let (mut r, mut c, mut v) = (Vec::new(), Vec::new(), Vec::new());
        for i in 0..count {
            for a in 0..3 {
                r.push(3 * i + a);
                c.push(i);
                v.push(-self.measure[i] * self.normals[i][a]);
            }
        }
        let scatter = sp::triplets(3 * count, count, &r, &c, &v)?;
        sp::mm3(&sp::kron_eye(&sp::t(&self.left), 3)?, &scatter, &self.right)
    }
}
