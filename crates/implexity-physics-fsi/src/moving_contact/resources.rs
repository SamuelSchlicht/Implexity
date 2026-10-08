// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::CaeResult;
use crate::{interface::FluidField,moving_contact::{separate_body::{MovingFsiPairModel,PairSolidField},event_step::EventAdvancePolicy,contact_field::{ContactField,NativeContactLaw}}};
pub struct PairNativeResources<'a>{pub solid:PairSolidField<'a>,pub fluid:FluidField}
pub struct PersistentContactFields<'a,L:NativeContactLaw>{pub contact:ContactField<PairSolidField<'a>,L>,pub fluid:FluidField}
impl<'a> PairNativeResources<'a>{
 pub fn new(model:&'a MovingFsiPairModel,p:EventAdvancePolicy)->CaeResult<Self>{Ok(Self{solid:model.solid_field()?,fluid:model.event_fluid_field(p)?})}
 pub fn into_contact<L:NativeContactLaw>(self,law:L)->CaeResult<PersistentContactFields<'a,L>>{Ok(PersistentContactFields{contact:ContactField::new(self.solid,law)?,fluid:self.fluid})}
 pub fn build_contact<L:NativeContactLaw>(self,builder:impl FnOnce(&PairSolidField<'a>)->CaeResult<L>)->CaeResult<PersistentContactFields<'a,L>>{let law=builder(&self.solid)?;self.into_contact(law)}
}
