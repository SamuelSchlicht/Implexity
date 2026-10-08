// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
