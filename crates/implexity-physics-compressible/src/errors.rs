// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::CaeError;
use implexity_physics_base::model_errors::{PhysicsError,PhysicsModelErrorKind};
use serde_json::{Map,Value};
#[derive(Debug,Clone,PartialEq,thiserror::Error)]
#[error("{0}")]
pub struct ModelError(pub PhysicsError);
pub type PResult<T> = Result<T,ModelError>;
impl From<PhysicsError> for ModelError {fn from(e:PhysicsError)->Self{Self(e)}}
impl From<CaeError> for ModelError {fn from(e:CaeError)->Self{Self(PhysicsError::Cae(e))}}
impl From<ModelError> for CaeError {fn from(e:ModelError)->Self{e.0.into()}}
impl ModelError {
 pub fn chemistry_balance(m:impl Into<String>,p:impl Into<String>)->Self{Self(PhysicsError::Model{kind:PhysicsModelErrorKind::ChemistryBalance,message:m.into(),path:p.into(),details:Map::new()})}
 pub fn with_details(mut self,d:Map<String,Value>)->Self{if let PhysicsError::Model{details,..}=&mut self.0{*details=d;}self}

 pub fn validation(m:impl Into<String>,p:impl Into<String>)->Self{Self(PhysicsError::validation(m,p))}
 pub fn invalid(m:impl Into<String>)->Self{Self::validation(m,"")}
 pub fn contract(m:impl Into<String>)->Self{Self(PhysicsError::contract(m))}
 pub fn detail(mut self,k:&str,v:Value)->Self{if let PhysicsError::Model{details,..}=&mut self.0{details.insert(k.into(),v);}self}
 pub fn is_validation(&self)->bool{matches!(self.0,PhysicsError::Model{kind:PhysicsModelErrorKind::Validation|PhysicsModelErrorKind::ChemistryBalance,..})}
 pub fn code(&self)->Option<&'static str>{match &self.0{PhysicsError::Model{kind,..}=>Some(kind.code()),_=>None}}
}

pub type ModelIssue = implexity_physics_base::model_errors::PhysicsModelIssue;
