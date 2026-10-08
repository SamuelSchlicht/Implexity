// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use super::{contact_field::{ContactField, NativeContactLaw}};
use implexity_physics_solid::soft_fsi::field::{SoftSolidField, SoftStepCore};
use implexity_solve::multirate_coupling::FluxDrivenField;
pub trait SolidView<'m>: FluxDrivenField {
    fn solid_core(&self) -> &SoftStepCore<'m>;
}
impl<'m> SolidView<'m> for SoftSolidField<'m> {
    fn solid_core(&self) -> &SoftStepCore<'m> { self.core() }
}
impl<'m,L: NativeContactLaw> SolidView<'m> for ContactField<SoftSolidField<'m>,L> {
    fn solid_core(&self) -> &SoftStepCore<'m> { self.native().core() }
}
