// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
// METAPLEXIS-DISCLAIMER-END





use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};

use super::boundary::Topology;

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wale {
    pub constant: f64,
    pub denominator_floor: f64,
}

impl Wale {

    pub fn validate(&self) -> CaeResult<()> {
        if !(self.constant.is_finite()
            && self.constant > 0.0
            && self.denominator_floor.is_finite()
            && self.denominator_floor > 0.0)
        {
            return Err(CaeError::contract(
                "WALE constant and denominator_floor must be finite and positive",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Difference {
    Zero,
    Central(usize, usize),
    Forward(usize),
    Backward(usize),
}

#[derive(Clone, Debug)]
pub struct Stencil {
    pub diff: Vec<[Difference; 3]>,
}

impl Stencil {
    #[must_use]
    pub fn new<const Q: usize>(topology: &Topology<Q>) -> Self {
        let grid = topology.grid;
        let n = grid.cells();
        let diff = (0..n)
            .map(|x| {
                std::array::from_fn(|a| {
                    if topology.wall[x] || grid.shape[a] == 1 {
                        return Difference::Zero;
                    }
                    let mut o = [0i64; 3];
                    o[a] = 1;
                    let plus = grid
                        .inside(x, o, topology.periodic)
                        .then(|| grid.wrap(x, o))
                        .filter(|&y| !topology.wall[y]);
                    o[a] = -1;
                    let minus = grid
                        .inside(x, o, topology.periodic)
                        .then(|| grid.wrap(x, o))
                        .filter(|&y| !topology.wall[y]);
                    match (plus, minus) {
                        (Some(p), Some(m)) => Difference::Central(p, m),
                        (Some(p), None) => Difference::Forward(p),
                        (None, Some(m)) => Difference::Backward(m),
                        (None, None) => Difference::Zero,
                    }
                })
            })
            .collect();
        Self { diff }
    }

    #[must_use]
    pub fn gradient<S: Scalar>(&self, u: &[[S; 3]], x: usize) -> [[S; 3]; 3] {
        let mut g = [[S::zero(); 3]; 3];
        for b in 0..3 {
            for a in 0..3 {
                g[a][b] = match self.diff[x][b] {
                    Difference::Zero => S::zero(),
                    Difference::Central(p, m) => (u[p][a] - u[m][a]) * 0.5,
                    Difference::Forward(p) => u[p][a] - u[x][a],
                    Difference::Backward(m) => u[x][a] - u[m][a],
                };
            }
        }
        g
    }

    #[must_use]
    pub fn gradient_transpose<S: Scalar>(
        &self,
        g_bar: &[[[S; 3]; 3]],
        y: usize,
        neighbours: &[usize],
    ) -> [S; 3] {
        let mut out = [S::zero(); 3];
        let mut add = |x: usize, b: usize, coef: f64| {
            for a in 0..3 {
                out[a] += g_bar[x][a][b] * coef;
            }
        };
        for &x in neighbours.iter().chain(std::iter::once(&y)) {
            for b in 0..3 {
                match self.diff[x][b] {
                    Difference::Zero => {}
                    Difference::Central(p, m) => {
                        if p == y {
                            add(x, b, 0.5);
                        }
                        if m == y {
                            add(x, b, -0.5);
                        }
                    }
                    Difference::Forward(p) => {
                        if p == y {
                            add(x, b, 1.0);
                        }
                        if x == y {
                            add(x, b, -1.0);
                        }
                    }
                    Difference::Backward(m) => {
                        if x == y {
                            add(x, b, 1.0);
                        }
                        if m == y {
                            add(x, b, -1.0);
                        }
                    }
                }
            }
        }
        out
    }
}

fn invariants<S: Scalar>(g: &[[S; 3]; 3]) -> ([[S; 3]; 3], [[S; 3]; 3], S, S) {
    let mut g2 = [[S::zero(); 3]; 3];
    for a in 0..3 {
        for b in 0..3 {
            for c in 0..3 {
                g2[a][b] += g[a][c] * g[c][b];
            }
        }
    }
    let tr = (g2[0][0] + g2[1][1] + g2[2][2]) / 3.0;
    let mut sd = [[S::zero(); 3]; 3];
    let mut s = [[S::zero(); 3]; 3];
    let mut a_inv = S::zero();
    let mut b_inv = S::zero();
    for a in 0..3 {
        for b in 0..3 {
            sd[a][b] = (g2[a][b] + g2[b][a]) * 0.5 - if a == b { tr } else { S::zero() };
            s[a][b] = (g[a][b] + g[b][a]) * 0.5;
            a_inv += sd[a][b] * sd[a][b];
            b_inv += s[a][b] * s[a][b];
        }
    }
    (sd, s, a_inv, b_inv)
}

#[must_use]
pub fn lattice_eddy_viscosity<S: Scalar>(model: &Wale, g: &[[S; 3]; 3]) -> S {
    let (_, _, a, b) = invariants(g);
    let num = a.powf(1.5);
    let den = b.powf(2.5) + a.powf(1.25) + model.denominator_floor;
    num / den * (model.constant * model.constant)
}

pub fn physical_eddy_viscosity<S: Scalar>(
    model: &Wale,
    velocity_gradient_per_s: &[[S; 3]; 3],
    spacing_m: S,
    step_s: S,
) -> CaeResult<S> {
    model.validate()?;
    if !spacing_m.value().is_finite()
        || spacing_m.value() <= 0.0
        || !step_s.value().is_finite()
        || step_s.value() <= 0.0
        || velocity_gradient_per_s.iter().flatten().any(|v| !v.value().is_finite())
    {
        return Err(CaeError::contract("WALE requires finite gradients and positive spacing and step"));
    }
    let lattice_gradient = velocity_gradient_per_s.map(|row| row.map(|v| v * step_s));
    let viscosity = lattice_eddy_viscosity(model, &lattice_gradient) * spacing_m * spacing_m / step_s;
    if !viscosity.value().is_finite() || viscosity.value() < 0.0 {
        return Err(CaeError::contract("WALE eddy viscosity is not finite and nonnegative"));
    }
    Ok(viscosity)
}

#[must_use]
pub fn omega<S: Scalar>(model: &Wale, g: &[[S; 3]; 3], tau0: S) -> S {
    S::one() / (tau0 + lattice_eddy_viscosity(model, g) * 3.0)
}

#[must_use]
pub fn omega_vjp<S: Scalar>(model: &Wale, g: &[[S; 3]; 3], tau0: S, w: S) -> ([[S; 3]; 3], S) {
    let (sd, s, a, b) = invariants(g);
    let c2 = model.constant * model.constant;
    let num = a.powf(1.5);
    let den = b.powf(2.5) + a.powf(1.25) + model.denominator_floor;
    let nu = num / den * c2;
    let tau = tau0 + nu * 3.0;
    let tau_bar = -w / (tau * tau);
    let nu_bar = tau_bar * 3.0;
    let a_bar = nu_bar * c2 * (a.powf(0.5) * 1.5 / den - num * a.powf(0.25) * 1.25 / (den * den));
    let b_bar = -nu_bar * c2 * num * b.powf(1.5) * 2.5 / (den * den);
    let mut g_bar = [[S::zero(); 3]; 3];
    for x in 0..3 {
        for y in 0..3 {
            let mut da = S::zero();
            for k in 0..3 {
                da += sd[x][k] * g[y][k] + g[k][x] * sd[k][y];
            }
            g_bar[x][y] = a_bar * da * 2.0 + b_bar * s[x][y] * 2.0;
        }
    }
    (g_bar, tau_bar)
}

