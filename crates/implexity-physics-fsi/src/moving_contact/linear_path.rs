// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


pub type Positions = [[f64; 3]; 4];

#[derive(Clone, Copy, Debug)]
pub struct PathPolicy {
    pub minimum_barycentric: f64,
    pub minimum_area_ratio: f64,
    pub minimum_signed_gap: f64,
    pub time_resolution: f64,
    pub maximum_intervals: usize,
    pub maximum_depth: u32,
}
impl Default for PathPolicy {
    fn default() -> Self {
        Self {
            minimum_barycentric: 1e-6,
            minimum_area_ratio: 1e-8,
            minimum_signed_gap: 0.,
            time_resolution: 1e-10,
            maximum_intervals: 65536,
            maximum_depth: 60,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathDomain {
    SignedGap,
    StrictInterior,
    TriangleArea,
    Arithmetic,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathStatus {
    Admitted,
    Refused,
    Unresolved,
}
#[derive(Clone, Copy, Debug)]
pub struct PathReport {
    pub status: PathStatus,
    pub certified_prefix: f64,
    pub blocked_interval: Option<[f64; 2]>,
    pub domain: Option<PathDomain>,
    pub intervals_examined: usize,
}

#[derive(Clone, Copy, Debug)]
struct I {
    lo: f64,
    hi: f64,
}
fn down(x: f64) -> f64 {
    if x.is_nan() || x == f64::NEG_INFINITY {
        return x;
    }
    if x == 0. {
        return -f64::from_bits(1);
    }
    f64::from_bits(if x > 0. {
        x.to_bits() - 1
    } else {
        x.to_bits() + 1
    })
}
fn up(x: f64) -> f64 {
    -down(-x)
}
impl I {
    fn point(x: f64) -> Self {
        Self { lo: x, hi: x }
    }
    fn zero(self) -> bool {
        self.lo == 0. && self.hi == 0.
    }
    fn finite(self) -> bool {
        self.lo.is_finite() && self.hi.is_finite() && self.lo <= self.hi
    }
    fn add(self, b: Self) -> Self {
        if self.zero() {
            return b;
        }
        if b.zero() {
            return self;
        }
        Self {
            lo: down(self.lo + b.lo),
            hi: up(self.hi + b.hi),
        }
    }
    fn neg(self) -> Self {
        Self {
            lo: -self.hi,
            hi: -self.lo,
        }
    }
    fn sub(self, b: Self) -> Self {
        if self.lo == self.hi && b.lo == b.hi && self.lo == b.lo {
            return Self::point(0.);
        }
        self.add(b.neg())
    }
    fn mul(self, b: Self) -> Self {
        if self.zero() || b.zero() {
            return Self::point(0.);
        }
        let a = [
            self.lo * b.lo,
            self.lo * b.hi,
            self.hi * b.lo,
            self.hi * b.hi,
        ];
        Self {
            lo: down(a.into_iter().fold(f64::INFINITY, f64::min)),
            hi: up(a.into_iter().fold(f64::NEG_INFINITY, f64::max)),
        }
    }
    fn square(self) -> Self {
        if self.zero() {
            return self;
        }
        let a = self.lo * self.lo;
        let b = self.hi * self.hi;
        Self {
            lo: if self.lo <= 0. && self.hi >= 0. {
                0.
            } else {
                down(a.min(b)).max(0.)
            },
            hi: up(a.max(b)),
        }
    }
    fn div_positive(self, b: Self) -> Self {
        self.mul(Self {
            lo: down(1. / b.hi),
            hi: up(1. / b.lo),
        })
    }
    fn sqrt_positive(self) -> Self {
        Self {
            lo: down(self.lo.sqrt()).max(0.),
            hi: up(self.hi.sqrt()),
        }
    }
}
type V = [I; 3];
fn sub(a: V, b: V) -> V {
    std::array::from_fn(|i| a[i].sub(b[i]))
}
fn dot(a: V, b: V) -> I {
    (0..3).fold(I::point(0.), |s, i| s.add(a[i].mul(b[i])))
}
fn norm2(a: V) -> I {
    a.into_iter().fold(I::point(0.), |s, x| s.add(x.square()))
}
fn cross(a: V, b: V) -> V {
    [
        a[1].mul(b[2]).sub(a[2].mul(b[1])),
        a[2].mul(b[0]).sub(a[0].mul(b[2])),
        a[0].mul(b[1]).sub(a[1].mul(b[0])),
    ]
}

#[derive(Clone)]
struct Z {
    negative: bool,
    words: Vec<u64>,
}
impl Z {
    fn zero() -> Self {
        Self {
            negative: false,
            words: vec![],
        }
    }
    fn normalize(mut self) -> Self {
        while self.words.last() == Some(&0) {
            self.words.pop();
        }
        if self.words.is_empty() {
            self.negative = false;
        }
        self
    }
    fn from_f64(x: f64) -> Self {
        let bits = x.to_bits();
        let exponent = ((bits >> 52) & 2047) as usize;
        let mantissa = (bits & ((1_u64 << 52) - 1)) | if exponent == 0 { 0 } else { 1_u64 << 52 };
        if mantissa == 0 {
            return Self::zero();
        }
        let shift = exponent.saturating_sub(1);
        let limb = shift / 64;
        let rem = shift % 64;
        let mut words = vec![0; limb + 2];
        words[limb] = mantissa << rem;
        if rem > 0 {
            words[limb + 1] = mantissa >> (64 - rem);
        }
        Self {
            negative: bits >> 63 != 0,
            words,
        }
        .normalize()
    }
    fn neg(mut self) -> Self {
        if !self.words.is_empty() {
            self.negative = !self.negative;
        }
        self
    }
    fn magnitude_cmp(&self, b: &Self) -> std::cmp::Ordering {
        self.words
            .len()
            .cmp(&b.words.len())
            .then_with(|| self.words.iter().rev().cmp(b.words.iter().rev()))
    }
    fn add(&self, b: &Self) -> Self {
        if self.negative == b.negative {
            let n = self.words.len().max(b.words.len());
            let mut words = Vec::with_capacity(n + 1);
            let mut carry = 0_u128;
            for i in 0..n {
                let sum = self.words.get(i).copied().unwrap_or(0) as u128
                    + b.words.get(i).copied().unwrap_or(0) as u128
                    + carry;
                words.push(sum as u64);
                carry = sum >> 64;
            }
            if carry != 0 {
                words.push(carry as u64);
            }
            Self {
                negative: self.negative,
                words,
            }
            .normalize()
        } else {
            let (a, b) = if self.magnitude_cmp(b).is_lt() {
                (b, self)
            } else {
                (self, b)
            };
            let mut words = Vec::with_capacity(a.words.len());
            let mut borrow = false;
            for i in 0..a.words.len() {
                let (v, b1) = a.words[i].overflowing_sub(b.words.get(i).copied().unwrap_or(0));
                let (v, b2) = v.overflowing_sub(u64::from(borrow));
                words.push(v);
                borrow = b1 || b2;
            }
            debug_assert!(!borrow);
            Self {
                negative: a.negative,
                words,
            }
            .normalize()
        }
    }
    fn sub(&self, b: &Self) -> Self {
        self.add(&b.clone().neg())
    }
    fn mul(&self, b: &Self) -> Self {
        if self.words.is_empty() || b.words.is_empty() {
            return Self::zero();
        }
        let mut words = vec![0; self.words.len() + b.words.len()];
        for (i, &a) in self.words.iter().enumerate() {
            let mut carry = 0_u128;
            for (j, &b) in b.words.iter().enumerate() {
                let v = a as u128 * b as u128 + words[i + j] as u128 + carry;
                words[i + j] = v as u64;
                carry = v >> 64;
            }
            words[i + b.words.len()] = carry as u64;
        }
        Self {
            negative: self.negative != b.negative,
            words,
        }
        .normalize()
    }
}
fn exact_signed_volume_coefficients(old: Positions, new: Positions) -> [Z;4] {
    let relative = |q: Positions| -> [[Z; 3]; 3] {
        std::array::from_fn(|k| {
            std::array::from_fn(|i| Z::from_f64(q[[2, 3, 0][k]][i]).sub(&Z::from_f64(q[1][i])))
        })
    };
    let a = relative(old);
    let b = relative(new);
    let motion: [[[Z; 2]; 3]; 3] =
        std::array::from_fn(|k| std::array::from_fn(|i| [a[k][i].clone(), b[k][i].sub(&a[k][i])]));
    let mut coefficients: [Z; 4] = std::array::from_fn(|_| Z::zero());
    for (i, j, k, positive) in [
        (0, 1, 2, true),
        (1, 2, 0, true),
        (2, 0, 1, true),
        (0, 2, 1, false),
        (1, 0, 2, false),
        (2, 1, 0, false),
    ] {
        for x in 0..2 {
            for y in 0..2 {
                for z in 0..2 {
                    let value = motion[2][i][x].mul(&motion[0][j][y]).mul(&motion[1][k][z]);
                    coefficients[x + y + z] =
                        coefficients[x + y + z].add(&if positive { value } else { value.neg() });
                }
            }
        }
    }
    coefficients
}
fn exact_volume_bernstein(c:&[Z;4])->[Z;4]{
 let twice=|x:&Z|x.add(x);let triple=|x:&Z|twice(x).add(x);
 [triple(&c[0]),triple(&c[0]).add(&c[1]),triple(&c[0]).add(&twice(&c[1])).add(&c[2]),triple(&c[0].add(&c[1]).add(&c[2]).add(&c[3]))]
}
fn exactly_nonnegative_volume(c:&[Z;4])->bool{
    let twice=|x:&Z|x.add(x);
    let triple=|x:&Z|twice(x).add(x);
    let root=[triple(&c[0]),triple(&c[0]).add(&c[1]),triple(&c[0]).add(&twice(&c[1])).add(&c[2]),triple(&c[0].add(&c[1]).add(&c[2]).add(&c[3]))];
    let mut pending=vec![(root,0u32)];
    let mut examined=0usize;
    while let Some((b,depth))=pending.pop(){
        examined+=1;
        if b.iter().all(|v|!v.negative){continue;}
        if b[0].negative||b[3].negative||depth>=32||examined>=4096{return false;}
        let eight=|x:&Z|twice(&twice(&twice(x)));
        let four=|x:&Z|twice(&twice(x));
        let center=b[0].add(&triple(&b[1])).add(&triple(&b[2])).add(&b[3]);
        let left=[eight(&b[0]),four(&b[0].add(&b[1])),twice(&b[0].add(&twice(&b[1])).add(&b[2])),center.clone()];
        let right=[center,twice(&b[1].add(&twice(&b[2])).add(&b[3])),four(&b[2].add(&b[3])),eight(&b[3])];
        pending.push((right,depth+1));
        pending.push((left,depth+1));
    }
    true
}
fn exactly_above_volume_threshold(root:&[Z;4],threshold:&Z)->bool{
    let twice=|x:&Z|x.add(x);
    let triple=|x:&Z|twice(x).add(x);
    let root=std::array::from_fn(|k|root[k].sub(threshold));
    let mut pending=vec![(root,0u32)];
    let mut examined=0usize;
    while let Some((b,depth))=pending.pop(){
        examined+=1;
        if b.iter().all(|v|!v.negative){continue;}
        if b[0].negative||b[3].negative||depth>=32||examined>=4096{return false;}
        let eight=|x:&Z|twice(&twice(&twice(x)));
        let four=|x:&Z|twice(&twice(x));
        let center=b[0].add(&triple(&b[1])).add(&triple(&b[2])).add(&b[3]);
        let left=[eight(&b[0]),four(&b[0].add(&b[1])),twice(&b[0].add(&twice(&b[1])).add(&b[2])),center.clone()];
        let right=[center,twice(&b[1].add(&twice(&b[2])).add(&b[3])),four(&b[2].add(&b[3])),eight(&b[3])];
        pending.push((right,depth+1));
        pending.push((left,depth+1));
    }
    true
}
struct Motion {
    old: [V; 3],
    delta: [V; 3],
    scale: f64,
    coplanar: bool,
    nonnegative_volume: bool,
    volume_bernstein:[Z;4],
}
impl Motion {
    fn new(old: Positions, new: Positions) -> Result<Self, &'static str> {

        let relative = |q: Positions| -> [V; 3] {
            let q = q.map(|p| p.map(I::point));
            [sub(q[2], q[1]), sub(q[3], q[1]), sub(q[0], q[1])]
        };
        let a = relative(old);
        let b = relative(new);
        let scale = a[..2]
            .iter()
            .chain(&b[..2])
            .flatten()
            .fold(0_f64, |s, x| s.max(x.lo.abs()).max(x.hi.abs()));
        if !scale.is_finite() || scale <= 0. {
            return Err("unrepresentable triangle edge scale");
        }
        let normalize = |v: [V; 3]| v.map(|w| w.map(|x| x.div_positive(I::point(scale))));
        let a = normalize(a);
        let b = normalize(b);
        let delta = std::array::from_fn(|k| sub(b[k], a[k]));
        if a.iter()
            .chain(&b)
            .chain(&delta)
            .flatten()
            .any(|x| !x.finite())
        {
            return Err("unrepresentable normalized motion");
        }
        let signed_volume=exact_signed_volume_coefficients(old,new);
        Ok(Self {
            old: a,
            delta,
            scale,
            coplanar: signed_volume.iter().all(|x|x.words.is_empty()),
            nonnegative_volume: exactly_nonnegative_volume(&signed_volume),
            volume_bernstein:exact_volume_bernstein(&signed_volume),
        })
    }
    fn domain_bounds(&self, time: I, p: PathPolicy) -> Result<[I; 6], PathDomain> {
        let [u, v, w]: [V; 3] = std::array::from_fn(|k| {
            std::array::from_fn(|i| self.old[k][i].add(self.delta[k][i].mul(time)))
        });
        let n = cross(u, v);
        let area = norm2(n);
        let ratio = I::point(p.minimum_area_ratio).square();

        let au = area.sub(ratio.mul(norm2(u).square()));
        let av = area.sub(ratio.mul(norm2(v).square()));
        let beta = dot(cross(w, v), n);
        let gamma = dot(cross(u, w), n);
        let alpha = area.sub(beta).sub(gamma);
        let margin = I::point(p.minimum_barycentric).mul(area);
        let ba = alpha.sub(margin);
        let bb = beta.sub(margin);
        let bc = gamma.sub(margin);
        if [area, au, av, ba, bb, bc].iter().any(|x| !x.finite()) {
            return Err(PathDomain::Arithmetic);
        }

        let numerator = dot(w, n);
        let mut gap = if self.coplanar {
            I::point(0.).sub(I::point(p.minimum_signed_gap))
        } else if area.lo > 0. {
            numerator
                .div_positive(area.sqrt_positive())
                .mul(I::point(self.scale))
                .sub(I::point(p.minimum_signed_gap))
        } else {
            I {
                lo: f64::NEG_INFINITY,
                hi: f64::INFINITY,
            }
        };
        if self.nonnegative_volume && p.minimum_signed_gap==0. && gap.finite(){
            if gap.hi<0.{return Err(PathDomain::Arithmetic);}
            gap.lo=gap.lo.max(0.);
        }
        if p.minimum_signed_gap<0.&&area.lo>0.{
            let normal=down(down(area.sqrt_positive().lo*self.scale)*self.scale);
            if normal>0.&&normal.is_finite(){
                let threshold=Z::from_f64(p.minimum_signed_gap).mul(&Z::from_f64(normal)).mul(&Z::from_f64(1.));
                let threshold=threshold.add(&threshold).add(&threshold);
                if exactly_above_volume_threshold(&self.volume_bernstein,&threshold){
                    if gap.hi<0.{return Err(PathDomain::Arithmetic);}gap.lo=gap.lo.max(0.);
                }
            }
        }
        if area.lo > 0. && !gap.finite() {
            return Err(PathDomain::Arithmetic);
        }
        Ok([au, av, ba, bb, bc, gap])
    }
}

pub fn check_linear_path(
    old: Positions,
    new: Positions,
    p: PathPolicy,
) -> Result<PathReport, &'static str> {
    if old.iter().chain(&new).flatten().any(|x| !x.is_finite())
        || !(p.minimum_barycentric > 0. && p.minimum_barycentric < 1. / 3.)
        || !(p.minimum_area_ratio > 0. && p.minimum_area_ratio < 1.)
        || !p.minimum_signed_gap.is_finite()
        || !(p.time_resolution > 0. && p.time_resolution < 1.)
        || p.maximum_intervals == 0
        || p.maximum_depth == 0
        || p.maximum_depth > 64
    {
        return Err("invalid finite linear path or admission policy");
    }
    let motion = Motion::new(old, new)?;
    if p.minimum_signed_gap >= 0. {
        let c = exact_signed_volume_coefficients(old, new);
        let end = c[0].add(&c[1]).add(&c[2]).add(&c[3]);
        if c[0].negative || end.negative {
            return Ok(PathReport {
                status: PathStatus::Refused,
                certified_prefix: 0.,
                blocked_interval: Some(if c[0].negative { [0., 0.] } else { [1., 1.] }),
                domain: Some(PathDomain::SignedGap),
                intervals_examined: 0,
            });
        }
    }
    let mut pending = vec![(0., 1., 0_u32)];
    let mut prefix = 0.;
    let mut count = 0;
    while let Some((lo, hi, depth)) = pending.pop() {
        if count >= p.maximum_intervals {
            return Ok(PathReport {
                status: PathStatus::Unresolved,
                certified_prefix: prefix,
                blocked_interval: Some([lo, hi]),
                domain: None,
                intervals_examined: count,
            });
        }
        count += 1;
        let bounds = match motion.domain_bounds(I { lo, hi }, p) {
            Ok(b) => b,
            Err(d) => {
                return Ok(PathReport {
                    status: PathStatus::Unresolved,
                    certified_prefix: prefix,
                    blocked_interval: Some([lo, hi]),
                    domain: Some(d),
                    intervals_examined: count,
                })
            }
        };
        let domain = |i| {
            if i < 2 {
                PathDomain::TriangleArea
            } else if i < 5 {
                PathDomain::StrictInterior
            } else {
                PathDomain::SignedGap
            }
        };

        let good = |i: usize, x: I| if i == 5 { x.lo >= 0. } else { x.lo > 0. };
        let bad = |i: usize, x: I| if i == 5 { x.hi < 0. } else { x.hi <= 0. };
        if let Some(i) = (0..6).find(|&i| bad(i, bounds[i])) {
            return Ok(PathReport {
                status: PathStatus::Refused,
                certified_prefix: prefix,
                blocked_interval: Some([lo, hi]),
                domain: Some(domain(i)),
                intervals_examined: count,
            });
        }
        if (0..6).all(|i| good(i, bounds[i])) {
            prefix = hi;
            continue;
        }
        let unresolved = (0..6).find(|&i| !good(i, bounds[i])).unwrap();
        let mid = lo + (hi - lo) * 0.5;
        if count >= p.maximum_intervals
            || depth >= p.maximum_depth
            || hi - lo <= p.time_resolution
            || mid == lo
            || mid == hi
        {
            return Ok(PathReport {
                status: PathStatus::Unresolved,
                certified_prefix: prefix,
                blocked_interval: Some([lo, hi]),
                domain: Some(domain(unresolved)),
                intervals_examined: count,
            });
        }

        pending.push((mid, hi, depth + 1));
        pending.push((lo, mid, depth + 1));
    }
    Ok(PathReport {
        status: PathStatus::Admitted,
        certified_prefix: 1.,
        blocked_interval: None,
        domain: None,
        intervals_examined: count,
    })
}

#[derive(Clone,Copy,Debug)]
pub struct ResidualPathAllowance{pub residual_tolerance:f64,pub gap_scale_m:f64,pub allowance_m:f64}
impl ResidualPathAllowance{
 pub fn new(residual_tolerance:f64,gap_scale_m:f64)->Result<Self,&'static str>{
  let allowance_m=residual_tolerance*gap_scale_m;
  if !residual_tolerance.is_finite()||residual_tolerance<=0.||!gap_scale_m.is_finite()||gap_scale_m<=0.||!allowance_m.is_finite()||allowance_m<=0.{return Err("finite positive residual-derived path allowance");}
  Ok(Self{residual_tolerance,gap_scale_m,allowance_m})
 }
 pub fn policy(self,mut strict:PathPolicy)->Result<PathPolicy,&'static str>{
  if Self::new(self.residual_tolerance,self.gap_scale_m)?.allowance_m.to_bits()!=self.allowance_m.to_bits(){return Err("residual path allowance product identity");}
  if strict.minimum_signed_gap!=0.{return Err("residual allowance requires original zero-gap policy");}
  strict.minimum_signed_gap=-self.allowance_m;Ok(strict)
 }
}
#[derive(Clone,Copy,Debug)]
pub struct ApproximatePathCertificate{pub previous_positions:Positions,pub current_positions:Positions,pub lower_gap_bound_m:f64,pub allowance_m:f64,pub worst_allowance_ratio:f64,pub intervals_examined:usize}
pub fn certify_residual_path(old:Positions,new:Positions,strict:PathPolicy,allowance:ResidualPathAllowance)->Result<ApproximatePathCertificate,&'static str>{
 let p=allowance.policy(strict)?;
 let report=check_linear_path(old,new,p)?;
 if report.status!=PathStatus::Admitted{return Err("residual-derived whole contact path not admitted");}
 let motion=Motion::new(old,new)?;let mut pending=vec![(0.,1.,0u32)];let mut count=0;let mut lower=f64::INFINITY;
 while let Some((lo,hi,depth))=pending.pop(){
  count+=1;if count>p.maximum_intervals{return Err("residual path certificate budget");}
  let bounds=motion.domain_bounds(I{lo,hi},p).map_err(|_|"residual path certificate arithmetic")?;
  if bounds[..5].iter().all(|b|b.lo>0.)&&bounds[5].lo>=0.{lower=lower.min(down(bounds[5].lo+p.minimum_signed_gap));continue;}
  let mid=lo+(hi-lo)*0.5;if depth>=p.maximum_depth||hi-lo<=p.time_resolution||mid==lo||mid==hi{return Err("residual path certificate unresolved");}
  pending.push((mid,hi,depth+1));pending.push((lo,mid,depth+1));
 }
 if !lower.is_finite(){return Err("residual path certificate finite bound");}
 Ok(ApproximatePathCertificate{previous_positions:old,current_positions:new,lower_gap_bound_m:lower,allowance_m:allowance.allowance_m,worst_allowance_ratio:(-lower).max(0.)/allowance.allowance_m,intervals_examined:count})
}
