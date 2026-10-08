// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use implexity_ad::Scalar;

pub trait Lattice<const Q: usize>: Copy + Send + Sync + 'static {
    const NAME: &'static str;
    const DIMENSIONS: usize;
    const C: [[i32; 3]; Q];
    const W: [f64; Q];
    const OPPOSITE: [usize; Q];
    const CF: [[f64; 3]; Q];
}

#[derive(Clone, Copy, Debug, Default)]
pub struct D2Q9;
#[derive(Clone, Copy, Debug, Default)]
pub struct D3Q19;
#[derive(Clone, Copy, Debug, Default)]
pub struct D3Q27;

const fn opposite_of<const Q: usize>(c: [[i32; 3]; Q]) -> [usize; Q] {
    let mut out = [0usize; Q];
    let mut i = 0;
    while i < Q {
        let mut j = 0;
        while j < Q {
            if c[j][0] == -c[i][0] && c[j][1] == -c[i][1] && c[j][2] == -c[i][2] {
                out[i] = j;
            }
            j += 1;
        }
        i += 1;
    }
    out
}

const fn as_f64<const Q: usize>(c: [[i32; 3]; Q]) -> [[f64; 3]; Q] {
    let mut out = [[0.0; 3]; Q];
    let mut i = 0;
    while i < Q {
        out[i] = [c[i][0] as f64, c[i][1] as f64, c[i][2] as f64];
        i += 1;
    }
    out
}

const D2Q9_C: [[i32; 3]; 9] =
    [[0, 0, 0], [-1, -1, 0], [-1, 0, 0], [-1, 1, 0], [0, -1, 0], [0, 1, 0], [1, -1, 0], [1, 0, 0], [1, 1, 0]];

const D2Q9_W: [f64; 9] = {
    let mut w = [0.0; 9];
    let mut i = 0;
    while i < 9 {
        let n = D2Q9_C[i][0].abs() + D2Q9_C[i][1].abs();
        w[i] = match n {
            0 => 4.0 / 9.0,
            1 => 1.0 / 9.0,
            _ => 1.0 / 36.0,
        };
        i += 1;
    }
    w
};

const D3Q27_C: [[i32; 3]; 27] = {
    let mut out = [[0i32; 3]; 27];
    let mut n = 1;
    let mut a = -1;
    while a <= 1 {
        let mut b = -1;
        while b <= 1 {
            let mut c = -1;
            while c <= 1 {
                if !(a == 0 && b == 0 && c == 0) {
                    out[n] = [a, b, c];
                    n += 1;
                }
                c += 1;
            }
            b += 1;
        }
        a += 1;
    }
    out
};

const D3Q27_W: [f64; 27] = {
    let mut w = [0.0; 27];
    let mut i = 0;
    while i < 27 {
        let n = D3Q27_C[i][0].abs() + D3Q27_C[i][1].abs() + D3Q27_C[i][2].abs();
        w[i] = match n {
            0 => 8.0 / 27.0,
            1 => 2.0 / 27.0,
            2 => 1.0 / 54.0,
            _ => 1.0 / 216.0,
        };
        i += 1;
    }
    w
};

impl Lattice<9> for D2Q9 {
    const NAME: &'static str = "D2Q9";
    const DIMENSIONS: usize = 2;
    const C: [[i32; 3]; 9] = D2Q9_C;
    const W: [f64; 9] = D2Q9_W;
    const OPPOSITE: [usize; 9] = opposite_of(D2Q9_C);
    const CF: [[f64; 3]; 9] = as_f64(D2Q9_C);
}

impl Lattice<19> for D3Q19 {
    const NAME: &'static str = "D3Q19";
    const DIMENSIONS: usize = 3;
    const C: [[i32; 3]; 19] = crate::d3q19::C;
    const W: [f64; 19] = crate::d3q19::W;
    const OPPOSITE: [usize; 19] = crate::d3q19::OPPOSITE;
    const CF: [[f64; 3]; 19] = as_f64(crate::d3q19::C);
}

impl Lattice<27> for D3Q27 {
    const NAME: &'static str = "D3Q27";
    const DIMENSIONS: usize = 3;
    const C: [[i32; 3]; 27] = D3Q27_C;
    const W: [f64; 27] = D3Q27_W;
    const OPPOSITE: [usize; 27] = opposite_of(D3Q27_C);
    const CF: [[f64; 3]; 27] = as_f64(D3Q27_C);
}

#[inline]
#[must_use]
pub fn dot_c<S: Scalar, const Q: usize, L: Lattice<Q>>(i: usize, v: [S; 3]) -> S {

    let c = L::CF[i];
    v[0] * c[0] + v[1] * c[1] + v[2] * c[2]
}

#[inline]
#[must_use]
pub fn moments<S: Scalar, const Q: usize, L: Lattice<Q>>(f: &[S; Q]) -> (S, [S; 3]) {
    let mut rho = f[0];
    let mut j = [S::zero(); 3];
    for i in 1..Q {
        rho += f[i];
        let c = L::CF[i];
        for d in 0..3 {
            j[d] += f[i] * c[d];
        }
    }
    (rho, j)
}

#[inline]
#[must_use]
pub fn equilibrium<S: Scalar, const Q: usize, L: Lattice<Q>>(rho: S, u: [S; 3]) -> [S; Q] {
    let usq = u[0] * u[0] + u[1] * u[1] + u[2] * u[2];
    let base = S::one() - usq * 1.5;
    std::array::from_fn(|i| {
        let cu = dot_c::<S, Q, L>(i, u);
        rho * L::W[i] * (base + cu * 3.0 + cu * cu * 4.5)
    })
}

#[inline]
pub fn equilibrium_vjp<S: Scalar, const Q: usize, L: Lattice<Q>>(
    rho: S,
    u: [S; 3],
    g: &[S; Q],
    rho_bar: &mut S,
    u_bar: &mut [S; 3],
) {
    let usq = u[0] * u[0] + u[1] * u[1] + u[2] * u[2];
    let base = S::one() - usq * 1.5;
    let mut usq_bar = S::zero();
    for i in 0..Q {
        let cu = dot_c::<S, Q, L>(i, u);
        let poly = base + cu * 3.0 + cu * cu * 4.5;
        let gw = g[i] * L::W[i];
        *rho_bar += gw * poly;
        let e = gw * rho;
        let cu_bar = e * (cu * 9.0 + 3.0);
        let c = L::CF[i];
        for d in 0..3 {
            if c[d] != 0.0 {
                u_bar[d] += cu_bar * c[d];
            }
        }
        usq_bar -= e * 1.5;
    }
    for d in 0..3 {
        u_bar[d] += usq_bar * u[d] * 2.0;
    }
}

pub const CS2: f64 = 1.0 / 3.0;

