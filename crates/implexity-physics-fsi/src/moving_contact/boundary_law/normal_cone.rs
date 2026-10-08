// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
pub struct OrientedNormalConeCertificate{identity:String,axis:[f64;3],rows:usize}
impl OrientedNormalConeCertificate{
 pub(crate) fn from_native_star(rows:&[[f64;3]],axis:[f64;3],owner:&str,feature_positions:serde_json::Value)->CaeResult<Self>{if owner.len()!=64||!owner.bytes().all(|x|x.is_ascii_hexdigit()){return Err(CaeError::contract("native normal-cone owner identity"));}if !super::boundary_path::oriented_supporting_cone_is_unique(rows,axis).map_err(CaeError::contract)?{return Err(CaeError::contract("oriented native supporting cone is not a unique ray; manifold KKT owner required"));}let identity=implexity_core::json::canonical_sha256(&serde_json::json!({"owner":owner,"native_axis":axis,"star_halfspaces":rows,"native_feature_positions":feature_positions,"scope":"unique oriented binary64 native supporting normal ray; no ordinary shape/window gradient or global KKT uniqueness"}));Ok(Self{identity,axis,rows:rows.len()})}
 pub fn identity(&self)->&str{&self.identity}
 pub fn axis(&self)->[f64;3]{self.axis}
 pub fn halfspace_count(&self)->usize{self.rows}
}
