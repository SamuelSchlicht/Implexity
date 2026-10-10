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


use implexity_geometry::lattice::numerics::{box_blur3 as blur, box_blur3_t as blur_t};

use super::ad::{A, Shape};
use super::grid::flat;

#[must_use]
pub fn sub(a: A<'_>, lo: [usize; 3], hi: [usize; 3]) -> A<'_> {
    let s = a.shape().as3();
    let out = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let mut idx = Vec::with_capacity(out[0] * out[1] * out[2]);
    for i in lo[0]..hi[0] {
        for j in lo[1]..hi[1] {
            for k in lo[2]..hi[2] {
                idx.push(flat(s, i, j, k));
            }
        }
    }
    a.gather(idx, Shape::d3(out))
}

#[must_use]
pub fn axis_box(s: [usize; 3], axis: usize, start: usize, end: usize) -> ([usize; 3], [usize; 3]) {
    let mut lo = [0; 3];
    let mut hi = s;
    lo[axis] = start;
    hi[axis] = end;
    (lo, hi)
}

#[must_use]
pub fn slice_axis(a: A<'_>, axis: usize, start: usize, end: usize) -> A<'_> {
    let (lo, hi) = axis_box(a.shape().as3(), axis, start, end);
    sub(a, lo, hi)
}

#[must_use]
pub fn lo_part(a: A<'_>, axis: usize) -> A<'_> {
    let n = a.shape().as3()[axis];
    slice_axis(a, axis, 0, n - 1)
}

#[must_use]
pub fn hi_part(a: A<'_>, axis: usize) -> A<'_> {
    let n = a.shape().as3()[axis];
    slice_axis(a, axis, 1, n)
}

#[must_use]
pub fn plane(a: A<'_>, axis: usize, last: bool) -> A<'_> {
    let n = a.shape().as3()[axis];
    if last { slice_axis(a, axis, n - 1, n) } else { slice_axis(a, axis, 0, 1) }
}

#[must_use]
pub fn embed(x: A<'_>, lo: [usize; 3], full: [usize; 3]) -> A<'_> {
    let s = x.shape().as3();
    let mut idx = Vec::with_capacity(x.len());
    for i in 0..s[0] {
        for j in 0..s[1] {
            for k in 0..s[2] {
                idx.push(flat(full, lo[0] + i, lo[1] + j, lo[2] + k));
            }
        }
    }
    x.scatter_add(idx, Shape::d3(full))
}

#[must_use]
pub fn pad_edge(a: A<'_>, r: usize) -> A<'_> {
    let s = a.shape().as3();
    let out = [s[0] + 2 * r, s[1] + 2 * r, s[2] + 2 * r];
    let clampi = |x: usize, n: usize| x.saturating_sub(r).min(n - 1);
    let mut idx = Vec::with_capacity(out[0] * out[1] * out[2]);
    for i in 0..out[0] {
        for j in 0..out[1] {
            for k in 0..out[2] {
                idx.push(flat(s, clampi(i, s[0]), clampi(j, s[1]), clampi(k, s[2])));
            }
        }
    }
    a.gather(idx, Shape::d3(out))
}

#[must_use]
pub fn concat_axis<'g>(parts: &[A<'g>], axis: usize) -> A<'g> {
    let g = parts[0].graph();
    let s0 = parts[0].shape().as3();
    let mut out = s0;
    out[axis] = parts.iter().map(|p| p.shape().as3()[axis]).sum();
    let cat = g.concat(parts, Shape::d1(out.iter().product()));
    let mut offs = Vec::with_capacity(parts.len());
    let mut o = 0usize;
    let mut start = 0usize;
    let mut starts = Vec::with_capacity(parts.len());
    for p in parts {
        offs.push(o);
        starts.push(start);
        o += p.len();
        start += p.shape().as3()[axis];
    }
    let mut idx = Vec::with_capacity(out.iter().product());
    for i in 0..out[0] {
        for j in 0..out[1] {
            for k in 0..out[2] {
                let c = [i, j, k][axis];
                let pi = starts.iter().rposition(|&s| s <= c).unwrap_or(0);
                let ps = parts[pi].shape().as3();
                let mut loc = [i, j, k];
                loc[axis] = c - starts[pi];
                idx.push(offs[pi] + flat(ps, loc[0], loc[1], loc[2]));
            }
        }
    }
    cat.gather(idx, Shape::d3(out))
}

#[must_use]
pub fn flip(a: A<'_>, axis: usize) -> A<'_> {
    let s = a.shape().as3();
    let mut idx = Vec::with_capacity(a.len());
    for i in 0..s[0] {
        for j in 0..s[1] {
            for k in 0..s[2] {
                let mut p = [i, j, k];
                p[axis] = s[axis] - 1 - p[axis];
                idx.push(flat(s, p[0], p[1], p[2]));
            }
        }
    }
    a.gather(idx, Shape::d3(s))
}

#[must_use]
pub fn transpose(a: A<'_>, perm: [usize; 3]) -> A<'_> {
    let s = a.shape().as3();
    let out = [s[perm[0]], s[perm[1]], s[perm[2]]];
    let mut idx = Vec::with_capacity(a.len());
    for i in 0..out[0] {
        for j in 0..out[1] {
            for k in 0..out[2] {
                let o = [i, j, k];
                let mut src = [0; 3];
                for d in 0..3 {
                    src[perm[d]] = o[d];
                }
                idx.push(flat(s, src[0], src[1], src[2]));
            }
        }
    }
    a.gather(idx, Shape::d3(out))
}

fn cumsum_raw(v: &[f64], s: [usize; 3], axis: usize) -> Vec<f64> {
    let mut out = v.to_vec();
    for i in 0..s[0] {
        for j in 0..s[1] {
            for k in 0..s[2] {
                let p = [i, j, k];
                if p[axis] == 0 {
                    continue;
                }
                let mut q = p;
                q[axis] -= 1;
                let prev = out[flat(s, q[0], q[1], q[2])];
                out[flat(s, i, j, k)] += prev;
            }
        }
    }
    out
}

fn rcumsum_raw(v: &[f64], s: [usize; 3], axis: usize) -> Vec<f64> {
    let mut out = v.to_vec();
    for i in (0..s[0]).rev() {
        for j in (0..s[1]).rev() {
            for k in (0..s[2]).rev() {
                let p = [i, j, k];
                if p[axis] + 1 == s[axis] {
                    continue;
                }
                let mut q = p;
                q[axis] += 1;
                let next = out[flat(s, q[0], q[1], q[2])];
                out[flat(s, i, j, k)] += next;
            }
        }
    }
    out
}

#[must_use]
pub fn cumsum(a: A<'_>, axis: usize) -> A<'_> {
    let s = a.shape().as3();
    a.graph().linear(a, Shape::d3(s), move |v| cumsum_raw(v, s, axis), move |g| rcumsum_raw(g, s, axis))
}

#[must_use]
pub fn box_blur3(a: A<'_>, r: usize) -> A<'_> {
    if r == 0 {
        return a;
    }
    let s = a.shape().as3();
    a.graph().linear(a, Shape::d3(s), move |v| blur(v, s, r), move |g| blur_t(g, s, r))
}

#[must_use]
pub fn central_grad<'g>(f: A<'g>, h: f64) -> [A<'g>; 3] {
    let s = f.shape().as3();
    let q = pad_edge(f, 1);
    let inner = |axis: usize, shift: isize| -> A<'g> {
        let mut lo = [1usize; 3];
        let mut hi = [s[0] + 1, s[1] + 1, s[2] + 1];
        if shift > 0 {
            lo[axis] += 1;
            hi[axis] += 1;
        } else {
            lo[axis] -= 1;
            hi[axis] -= 1;
        }
        sub(q, lo, hi)
    };
    std::array::from_fn(|a| (inner(a, 1) - inner(a, -1)) / (2.0 * h))
}

#[must_use]
pub fn np_gradient_axis(f: A<'_>, axis: usize, h: f64) -> A<'_> {
    let s = f.shape().as3();
    let n = s[axis];
    let fwd = move |v: &[f64]| -> Vec<f64> {
        let mut out = vec![0.0; v.len()];
        for i in 0..s[0] {
            for j in 0..s[1] {
                for k in 0..s[2] {
                    let p = [i, j, k];
                    let c = p[axis];
                    let at = |d: isize| {
                        let mut q = p;
                        q[axis] = c.wrapping_add_signed(d);
                        v[flat(s, q[0], q[1], q[2])]
                    };
                    out[flat(s, i, j, k)] = if n < 2 {
                        0.0
                    } else if c == 0 {
                        (at(1) - at(0)) / h
                    } else if c == n - 1 {
                        (at(0) - at(-1)) / h
                    } else {
                        (at(1) - at(-1)) / (2.0 * h)
                    };
                }
            }
        }
        out
    };
    let bwd = move |g: &[f64]| -> Vec<f64> {
        let mut out = vec![0.0; g.len()];
        for i in 0..s[0] {
            for j in 0..s[1] {
                for k in 0..s[2] {
                    let p = [i, j, k];
                    let c = p[axis];
                    let gi = g[flat(s, i, j, k)];
                    let mut add = |d: isize, w: f64| {
                        let mut q = p;
                        q[axis] = c.wrapping_add_signed(d);
                        out[flat(s, q[0], q[1], q[2])] += w * gi;
                    };
                    if n < 2 {
                    } else if c == 0 {
                        add(1, 1.0 / h);
                        add(0, -1.0 / h);
                    } else if c == n - 1 {
                        add(0, 1.0 / h);
                        add(-1, -1.0 / h);
                    } else {
                        add(1, 1.0 / (2.0 * h));
                        add(-1, -1.0 / (2.0 * h));
                    }
                }
            }
        }
        out
    };
    f.graph().linear(f, Shape::d3(s), fwd, bwd)
}

#[must_use]
pub fn np_gradient(f: A<'_>, h: f64) -> [A<'_>; 3] {
    std::array::from_fn(|a| np_gradient_axis(f, a, h))
}

pub mod raw {
    use super::flat;

    #[must_use]
    pub fn sub(v: &[f64], s: [usize; 3], lo: [usize; 3], hi: [usize; 3]) -> Vec<f64> {
        let mut out = Vec::new();
        for i in lo[0]..hi[0] {
            for j in lo[1]..hi[1] {
                for k in lo[2]..hi[2] {
                    out.push(v[flat(s, i, j, k)]);
                }
            }
        }
        out
    }

    #[must_use]
    pub fn cumsum(v: &[f64], s: [usize; 3], axis: usize) -> Vec<f64> {
        super::cumsum_raw(v, s, axis)
    }
}
