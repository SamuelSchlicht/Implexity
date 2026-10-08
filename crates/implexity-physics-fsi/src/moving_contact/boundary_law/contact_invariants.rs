// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_ad::Scalar;
pub(super) fn sub<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] { std::array::from_fn(|i| a[i] - b[i]) }
pub(super) fn dot<S: Scalar>(a: [S; 3], b: [S; 3]) -> S { a[0]*b[0] + a[1]*b[1] + a[2]*b[2] }
pub(super) fn invariants<S: Scalar>(p: [[S; 3]; 4]) -> [S; 6] {
    let u = sub(p[2],p[1]); let v = sub(p[3],p[1]); let w = sub(p[0],p[1]);
    [dot(u,u),dot(v,v),dot(w,w),dot(u,v),dot(u,w),dot(v,w)]
}
pub(super) fn invariant_gradient<S: Scalar>(old: [S;6], new: [S;6], h0:S, h1:S) -> [S;6] {
    let c=S::from_f64; let mid: [S;6]=std::array::from_fn(|i|(old[i]+new[i])*c(0.5));
    let d: [S;6]=std::array::from_fn(|i|new[i]-old[i]);
    let d0=old[0]*old[1]-old[3]*old[3]; let d1=new[0]*new[1]-new[3]*new[3];
    let dm=(d0+d1)*c(0.5); let hm=(h0+h1)*c(0.5);
    let gd=[mid[1],mid[0],c(0.),c(-2.)*mid[3],c(0.),c(0.)];
    let mut gn=gd.map(|x|x*mid[2]); gn[2]=gn[2]+dm;
    gn[1]=gn[1]-(old[4]*old[4]+new[4]*new[4])*c(0.5);
    gn[4]=gn[4]-c(2.)*mid[1]*mid[4];
    gn[0]=gn[0]-(old[5]*old[5]+new[5]*new[5])*c(0.5);
    gn[5]=gn[5]-c(2.)*mid[0]*mid[5];
    gn[3]=gn[3]+c(2.)*(mid[4]*mid[5]+d[4]*d[5]/c(12.));
    gn[4]=gn[4]+c(2.)*(mid[3]*mid[5]+d[3]*d[5]/c(12.));
    gn[5]=gn[5]+c(2.)*(mid[3]*mid[4]+d[3]*d[4]/c(12.));
    std::array::from_fn(|i|(gn[i]-hm*gd[i])/dm)
}
pub(super) fn coordinate_gradient<S:Scalar>(p:[[S;3];4],g:[S;6])->[S;12] {
    let c=S::from_f64;let u=sub(p[2],p[1]);let v=sub(p[3],p[1]);let w=sub(p[0],p[1]);
    let fu: [S;3]=std::array::from_fn(|a|c(2.)*g[0]*u[a]+g[3]*v[a]+g[4]*w[a]);
    let fv: [S;3]=std::array::from_fn(|a|c(2.)*g[1]*v[a]+g[3]*u[a]+g[5]*w[a]);
    let fw: [S;3]=std::array::from_fn(|a|c(2.)*g[2]*w[a]+g[4]*u[a]+g[5]*v[a]);
    std::array::from_fn(|i|match i/3 {0=>fw[i%3],1=>-(fu[i%3]+fv[i%3]+fw[i%3]),2=>fu[i%3],_=>fv[i%3]})
}
