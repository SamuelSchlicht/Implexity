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
            minimum_barycentric: 0.,
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
    ClosedFaceProjection,
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
    closed_barycentric:[bool;3],
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
            closed_barycentric:exact_barycentric_coefficients(old,new).each_ref().map(exact_quartic_nonnegative),
        })
    }
    fn domain_bounds(&self, time: I, p: PathPolicy, check_gap: bool) -> Result<[I; 6], PathDomain> {
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
        let mut ba = alpha.sub(margin);
        let mut bb = beta.sub(margin);
        let mut bc = gamma.sub(margin);
        if p.minimum_barycentric==0.{for (bound,certified) in [&mut ba,&mut bb,&mut bc].into_iter().zip(self.closed_barycentric){if certified{if bound.hi<0.{return Err(PathDomain::Arithmetic);}bound.lo=bound.lo.max(0.);}}}
        if [area, au, av, ba, bb, bc].iter().any(|x| !x.finite()) {
            return Err(PathDomain::Arithmetic);
        }
        if !check_gap { return Ok([au, av, ba, bb, bc, I::point(0.)]); }
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
    check_linear_domains(old,new,p,true)
}

pub fn check_linear_feature_path(old: Positions,new: Positions,p: PathPolicy) -> Result<PathReport, &'static str> {
    check_linear_domains(old,new,p,false)
}

fn check_linear_domains(old: Positions,new: Positions,p: PathPolicy,check_gap:bool) -> Result<PathReport, &'static str> {
    if old.iter().chain(&new).flatten().any(|x| !x.is_finite())
        || !(p.minimum_barycentric >= 0. && p.minimum_barycentric < 1. / 3.)
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
    if check_gap && p.minimum_signed_gap >= 0. {
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
        let bounds = match motion.domain_bounds(I { lo, hi }, p, check_gap) {
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
                PathDomain::ClosedFaceProjection
            } else {
                PathDomain::SignedGap
            }
        };
        let count_domains=if check_gap {6}else{5};
        let good = |i: usize, x: I| if i == 5 || (p.minimum_barycentric==0. && (2..5).contains(&i)) { x.lo >= 0. } else { x.lo > 0. };
        let bad = |i: usize, x: I| if i == 5 || (p.minimum_barycentric==0. && (2..5).contains(&i)) { x.hi < 0. } else { x.hi <= 0. };
        if let Some(i) = (0..count_domains).find(|&i| bad(i, bounds[i])) {
            return Ok(PathReport {
                status: PathStatus::Refused,
                certified_prefix: prefix,
                blocked_interval: Some([lo, hi]),
                domain: Some(domain(i)),
                intervals_examined: count,
            });
        }
        if (0..count_domains).all(|i| good(i, bounds[i])) {
            prefix = hi;
            continue;
        }
        let unresolved = (0..count_domains).find(|&i| !good(i, bounds[i])).unwrap();
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
  let bounds=motion.domain_bounds(I{lo,hi},p,true).map_err(|_|"residual path certificate arithmetic")?;
  if bounds[..5].iter().enumerate().all(|(i,b)|if i>=2&&p.minimum_barycentric==0.{b.lo>=0.}else{b.lo>0.})&&bounds[5].lo>=0.{lower=lower.min(down(bounds[5].lo+p.minimum_signed_gap));continue;}
  let mid=lo+(hi-lo)*0.5;if depth>=p.maximum_depth||hi-lo<=p.time_resolution||mid==lo||mid==hi{return Err("residual path certificate unresolved");}
  pending.push((mid,hi,depth+1));pending.push((lo,mid,depth+1));
 }
 if !lower.is_finite(){return Err("residual path certificate finite bound");}
 Ok(ApproximatePathCertificate{previous_positions:old,current_positions:new,lower_gap_bound_m:lower,allowance_m:allowance.allowance_m,worst_allowance_ratio:(-lower).max(0.)/allowance.allowance_m,intervals_examined:count})
}

fn zsum(v:impl Iterator<Item=Z>)->Z{v.fold(Z::zero(),|a,b|a.add(&b))}
fn zmul_small(v:&Z,k:usize)->Z{zsum((0..k).map(|_|v.clone()))}
fn exact_barycentric_coefficients(old:Positions,new:Positions)->[[Z;5];3]{
 let relative=|q:Positions|->[[Z;3];3]{std::array::from_fn(|k|std::array::from_fn(|i|Z::from_f64(q[[2,3,0][k]][i]).sub(&Z::from_f64(q[1][i]))))};let a=relative(old);let b=relative(new);let motion:[[[Z;2];3];3]=std::array::from_fn(|k|std::array::from_fn(|i|[a[k][i].clone(),b[k][i].sub(&a[k][i])]));
 let cross_poly=|x:usize,y:usize|->[[Z;3];3]{std::array::from_fn(|i|{let j=(i+1)%3;let k=(i+2)%3;std::array::from_fn(|n|zsum((0..2).flat_map(|r|(0..2).filter(move|t|r+t==n).map(move|t|(r,t))).map(|(r,t)|motion[x][j][r].mul(&motion[y][k][t]).sub(&motion[x][k][r].mul(&motion[y][j][t])))))})};
 let normal=cross_poly(0,1);let beta_cross=cross_poly(2,1);let gamma_cross=cross_poly(0,2);let dot_poly=|x:&[[Z;3];3],y:&[[Z;3];3]|-> [Z;5]{std::array::from_fn(|n|zsum((0..3).flat_map(|i|(0..3).flat_map(move|r|(0..3).filter(move|t|r+t==n).map(move|t|(i,r,t)))).map(|(i,r,t)|x[i][r].mul(&y[i][t]))))};let area=dot_poly(&normal,&normal);let beta=dot_poly(&beta_cross,&normal);let gamma=dot_poly(&gamma_cross,&normal);let alpha=std::array::from_fn(|i|area[i].sub(&beta[i]).sub(&gamma[i]));[alpha,beta,gamma]
}
fn exact_quartic_nonnegative(c:&[Z;5])->bool{
 let root=[zmul_small(&c[0],12),zmul_small(&c[0],12).add(&zmul_small(&c[1],3)),zmul_small(&c[0],12).add(&zmul_small(&c[1],6)).add(&zmul_small(&c[2],2)),zmul_small(&c[0],12).add(&zmul_small(&c[1],9)).add(&zmul_small(&c[2],6)).add(&zmul_small(&c[3],3)),zmul_small(&zsum(c.iter().cloned()),12)];
 let mut pending=vec![(root,0u32)];let mut examined=0usize;while let Some((b,depth))=pending.pop(){examined+=1;if b.iter().all(|v|!v.negative){continue;}if b[0].negative||b[4].negative||depth>=32||examined>=4096{return false;}let binomial=[[1,0,0,0,0],[1,1,0,0,0],[1,2,1,0,0],[1,3,3,1,0],[1,4,6,4,1]];let left=std::array::from_fn(|k|zmul_small(&zsum((0..=k).map(|j|zmul_small(&b[j],binomial[k][j]))),1usize<<(4-k)));let right=std::array::from_fn(|k|zmul_small(&zsum((k..5).map(|j|zmul_small(&b[j],binomial[4-k][j-k]))),1usize<<k));pending.push((right,depth+1));pending.push((left,depth+1));}true
}
pub fn certify_supporting_star_path(triangle_old:[[f64;3];3],triangle_new:[[f64;3];3],anchor_old:[f64;3],anchor_new:[f64;3],point_old:[f64;3],point_new:[f64;3],positive:bool)->Result<bool,&'static str>{if triangle_old.iter().chain(&triangle_new).flatten().chain(anchor_old.iter()).chain(anchor_new.iter()).chain(point_old.iter()).chain(point_new.iter()).any(|v|!v.is_finite()){return Err("finite supporting star path");}let positions=|t:[[f64;3];3],p:[f64;3]|[p,t[0],t[1],t[2]];let a=exact_signed_volume_coefficients(positions(triangle_old,point_old),positions(triangle_new,point_new));let b=exact_signed_volume_coefficients(positions(triangle_old,anchor_old),positions(triangle_new,anchor_new));let c=std::array::from_fn(|i|if positive{a[i].sub(&b[i])}else{b[i].sub(&a[i])});Ok(exactly_nonnegative_volume(&c))}

pub fn exact_fixed_axis_separation(a:[[f64;3];3],b:[[f64;3];3],axis:[f64;3])->Result<bool,&'static str>{if a.iter().chain(&b).flatten().chain(&axis).any(|x|!x.is_finite())||axis.iter().all(|x|*x==0.){return Err("finite nonzero separation axis");}for p in a{for q in b{let value=zsum((0..3).map(|i|Z::from_f64(axis[i]).mul(&Z::from_f64(q[i]).sub(&Z::from_f64(p[i])))));if value.negative||value.words.is_empty(){return Ok(false);}}}Ok(true)}

fn exact_edge_star_coefficients(old:Positions,new:Positions,anchor_old:[f64;3],anchor_new:[f64;3],point_old:[f64;3],point_new:[f64;3])->[Z;4]{
 let relative=|q:Positions,anchor:[f64;3],point:[f64;3]|->[[Z;3];3]{std::array::from_fn(|k|std::array::from_fn(|i|match k{0=>Z::from_f64(q[1][i]).sub(&Z::from_f64(q[0][i])),1=>Z::from_f64(q[3][i]).sub(&Z::from_f64(q[2][i])),_=>Z::from_f64(point[i]).sub(&Z::from_f64(anchor[i]))}))};
 let a=relative(old,anchor_old,point_old);let b=relative(new,anchor_new,point_new);let motion:[[[Z;2];3];3]=std::array::from_fn(|k|std::array::from_fn(|i|[a[k][i].clone(),b[k][i].sub(&a[k][i])]));let mut out:[Z;4]=std::array::from_fn(|_|Z::zero());
 for(i,j,k,positive)in[(0,1,2,true),(1,2,0,true),(2,0,1,true),(0,2,1,false),(1,0,2,false),(2,1,0,false)]{for x in 0..2{for y in 0..2{for z in 0..2{let v=motion[2][i][x].mul(&motion[0][j][y]).mul(&motion[1][k][z]);out[x+y+z]=out[x+y+z].add(&if positive{v}else{v.neg()});}}}}out
}
pub fn certify_supporting_edge_star_path(old:Positions,new:Positions,anchor_old:[f64;3],anchor_new:[f64;3],point_old:[f64;3],point_new:[f64;3],orientation:f64,positive:bool)->Result<bool,&'static str>{
 if old.iter().chain(&new).flatten().chain(anchor_old.iter()).chain(anchor_new.iter()).chain(point_old.iter()).chain(point_new.iter()).any(|x|!x.is_finite())||(orientation!=1.&&orientation!= -1.){return Err("finite edge-star path and explicit normal orientation");}
 let mut c=exact_edge_star_coefficients(old,new,anchor_old,anchor_new,point_old,point_new);if (orientation<0.)==positive{c=c.map(|v|v.neg());}Ok(exactly_nonnegative_volume(&c))
}
pub fn certify_edge_gap_path(old:Positions,new:Positions,orientation:f64)->Result<bool,&'static str>{certify_supporting_edge_star_path(old,new,old[2],new[2],old[0],new[0],orientation,true)}

pub fn oriented_supporting_cone_is_unique(rows:&[[f64;3]],axis:[f64;3])->Result<bool,&'static str>{
 if rows.is_empty()||rows.iter().flatten().chain(axis.iter()).any(|x|!x.is_finite())||axis.iter().all(|x|*x==0.){return Err("finite supporting cone/native normal");}
 let mut a:Vec<[Z;3]>=rows.iter().map(|r|r.map(Z::from_f64)).collect();let normal=axis.map(Z::from_f64);a.push(normal.clone());let dot=|u:&[Z;3],v:&[Z;3]|zsum((0..3).map(|i|u[i].mul(&v[i])));let cross=|u:&[Z;3],v:&[Z;3]|[u[1].mul(&v[2]).sub(&u[2].mul(&v[1])),u[2].mul(&v[0]).sub(&u[0].mul(&v[2])),u[0].mul(&v[1]).sub(&u[1].mul(&v[0]))];let zero=|u:&[Z;3]|u.iter().all(|x|x.words.is_empty());
 if a.iter().any(|r|dot(r,&normal).negative){return Ok(false);}let mut spans=false;let mut found=false;
 for i in 0..a.len(){for j in i+1..a.len(){let ray=cross(&a[i],&a[j]);if zero(&ray){continue;}if !spans{spans=a.iter().any(|r|!dot(r,&ray).words.is_empty());}for positive in [true,false]{let ray=if positive{ray.clone()}else{ray.clone().map(|v|v.neg())};if a.iter().any(|r|dot(r,&ray).negative){continue;}found=true;if !zero(&cross(&ray,&normal)){return Ok(false);}}}}
 Ok(spans&&found)
}

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum RootDomain{Interior,Outside,Unresolved}
#[derive(Clone,Copy,Debug)]
pub struct CoplanarityBracket{pub interval:[f64;2],pub transverse:bool,pub exact_fraction:Option<f64>}
pub fn isolate_coplanarity_roots(old:Positions,new:Positions,p:PathPolicy)->Result<Vec<CoplanarityBracket>,&'static str>{
 if old.iter().chain(&new).flatten().any(|x|!x.is_finite())||!p.time_resolution.is_finite()||p.time_resolution<=0.||p.time_resolution>=1.||p.maximum_intervals==0||p.maximum_depth==0||p.maximum_depth>60{return Err("coplanarity root input/policy");}
 let root=exact_volume_bernstein(&exact_signed_volume_coefficients(old,new));
 if root.iter().all(|x|x.words.is_empty()){return Ok(vec![CoplanarityBracket{interval:[0.,1.],transverse:false,exact_fraction:None}]);}
 let mut pending=vec![(root,0.,1.,0u32)];let mut output=vec![];let mut count=0;
 while let Some((b,lo,hi,depth))=pending.pop(){count+=1;if count>p.maximum_intervals{return Err("coplanarity root isolation budget exhausted");}
  for (index,t,other) in [(0,lo,1),(3,hi,2)]{if b[index].words.is_empty(){output.push(CoplanarityBracket{interval:[t,t],transverse:!b[other].words.is_empty(),exact_fraction:Some(t)});}}
  let signs:Vec<bool>=b.iter().filter(|v|!v.words.is_empty()).map(|v|v.negative).collect();let variations=signs.windows(2).filter(|w|w[0]!=w[1]).count();if variations==0{continue;}
  let derivative=[b[1].sub(&b[0]),b[2].sub(&b[1]),b[3].sub(&b[2])];let transverse=derivative.iter().all(|v|!v.words.is_empty()&&!v.negative)||derivative.iter().all(|v|!v.words.is_empty()&&v.negative);
  if hi-lo<=p.time_resolution||depth>=p.maximum_depth{output.push(CoplanarityBracket{interval:[lo,hi],transverse:variations==1&&transverse,exact_fraction:None});continue;}
  let twice=|x:&Z|x.add(x);let triple=|x:&Z|twice(x).add(x);let four=|x:&Z|twice(&twice(x));let eight=|x:&Z|twice(&four(x));let center=b[0].add(&triple(&b[1])).add(&triple(&b[2])).add(&b[3]);let left=[eight(&b[0]),four(&b[0].add(&b[1])),twice(&b[0].add(&twice(&b[1])).add(&b[2])),center.clone()];let right=[center,twice(&b[1].add(&twice(&b[2])).add(&b[3])),four(&b[2].add(&b[3])),eight(&b[3])];let mid=lo+(hi-lo)*0.5;if mid==lo||mid==hi{return Err("coplanarity root temporal resolution");}pending.push((right,mid,hi,depth+1));pending.push((left,lo,mid,depth+1));
 }
 output.sort_by(|a,b|a.interval[0].total_cmp(&b.interval[0]).then(a.interval[1].total_cmp(&b.interval[1])));output.dedup_by(|a,b|a.interval==b.interval&&a.exact_fraction==b.exact_fraction);Ok(output)
}
pub fn vertex_face_root_domain(old:Positions,new:Positions,interval:[f64;2],ratio:f64)->Result<RootDomain,&'static str>{
 if !interval.iter().all(|v|v.is_finite())||interval[0]<0.||interval[1]>1.||interval[0]>interval[1]||!ratio.is_finite()||ratio<=0.||ratio>=1.{return Err("vertex face root domain inputs");}
 let p=PathPolicy{minimum_area_ratio:ratio,..PathPolicy::default()};let m=Motion::new(old,new)?;let b=match m.domain_bounds(I{lo:interval[0],hi:interval[1]},p,true){Ok(b)=>b,Err(_)=>return Ok(RootDomain::Unresolved)};
 if b[2..5].iter().any(|v|v.hi<0.){return Ok(RootDomain::Outside);}if b[..5].iter().all(|v|v.lo>0.){Ok(RootDomain::Interior)}else{Ok(RootDomain::Unresolved)}
}

pub fn exact_projection_sign(point:[f64;3],anchor:[f64;3],axis:[f64;3])->Result<i8,&'static str>{
 if point.iter().chain(&anchor).chain(&axis).any(|v|!v.is_finite())||axis.iter().all(|v|*v==0.){return Err("finite nonzero exact projection inputs");}
 let z=zsum((0..3).map(|k|Z::from_f64(point[k]).sub(&Z::from_f64(anchor[k])).mul(&Z::from_f64(axis[k]))));Ok(if z.words.is_empty(){0}else if z.negative{-1}else{1})
}

pub fn exact_affine_projection_root(old:[f64;3],new:[f64;3],anchor_old:[f64;3],anchor_new:[f64;3],axis:[f64;3],p:PathPolicy)->Result<Option<CoplanarityBracket>,&'static str>{
 for (point,anchor)in[(old,anchor_old),(new,anchor_new)]{exact_projection_sign(point,anchor,axis)?;}
 if !p.time_resolution.is_finite()||p.time_resolution<=0.||p.time_resolution>=1.||p.maximum_depth==0||p.maximum_depth>60||p.maximum_intervals==0{return Err("affine projection root policy");}
 let projection=|point:[f64;3],anchor:[f64;3]|zsum((0..3).map(|k|Z::from_f64(point[k]).sub(&Z::from_f64(anchor[k])).mul(&Z::from_f64(axis[k]))));let a=projection(old,anchor_old);let b=projection(new,anchor_new);
 if a.negative||a.words.is_empty(){return Err("affine projection root lacks strict initial separation");}if !b.negative&&!b.words.is_empty(){return Ok(None);}if b.words.is_empty(){return Ok(Some(CoplanarityBracket{interval:[1.,1.],transverse:true,exact_fraction:Some(1.)}));}
 let mut lo=0.;let mut hi=1.;for depth in 0..=p.maximum_depth{if depth as usize>=p.maximum_intervals{return Err("affine projection root budget");}if hi-lo<=p.time_resolution||depth==p.maximum_depth{return Ok(Some(CoplanarityBracket{interval:[lo,hi],transverse:true,exact_fraction:None}));}let t=lo+(hi-lo)*0.5;if t==lo||t==hi{return Err("affine projection temporal resolution");}let z=a.mul(&Z::from_f64(1.).sub(&Z::from_f64(t))).add(&b.mul(&Z::from_f64(t)));if z.words.is_empty(){return Ok(Some(CoplanarityBracket{interval:[t,t],transverse:true,exact_fraction:Some(t)}));}if z.negative{hi=t;}else{lo=t;}}
 Err("affine projection root internal exhaustion")
}

pub fn exact_affine_clock_is_represented(clock:f64,interval:[f64;2],fraction:f64)->Result<bool,&'static str>{
 if interval.iter().chain([clock,fraction].iter()).any(|v|!v.is_finite())||fraction<=0.||fraction>1.||interval[1]<=interval[0]{return Err("finite affine clock domain");}
 let one=Z::from_f64(1.);let t=Z::from_f64(interval[0]).mul(&one).add(&Z::from_f64(fraction).mul(&Z::from_f64(interval[1]).sub(&Z::from_f64(interval[0]))));Ok(t.sub(&Z::from_f64(clock).mul(&one)).words.is_empty())
}
