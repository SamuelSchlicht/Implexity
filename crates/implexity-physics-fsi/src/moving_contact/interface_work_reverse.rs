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

use implexity_core::{CaeError,CaeResult};
use implexity_linalg::sparse::CsrMatrix;
fn fail(s:&str)->CaeError{CaeError::contract(s)}
fn finite(x:&[f64],n:usize)->CaeResult<()>{if x.len()!=n||x.iter().any(|x|!x.is_finite()){return Err(fail("interface work finite vector layout"));}Ok(())}
fn dot(a:&[f64],b:&[f64])->f64{a.iter().zip(b).map(|(a,b)|a*b).sum()}
#[derive(Clone,Copy)]pub struct WorkLedger{pub body_work_j:f64,pub defect_j:f64,pub cumulative_defect_j:f64,pub cumulative_abs_work_j:f64}
pub struct WorkDirection{pub begin:Vec<f64>,pub end:Vec<f64>,pub force:Vec<f64>,pub fluid_work_j:f64,pub previous_defect_j:f64,pub previous_abs_work_j:f64}
pub struct WorkPullback{pub begin:Vec<f64>,pub end:Vec<f64>,pub force:Vec<f64>,pub fluid_work_j:f64,pub previous_defect_j:f64,pub previous_abs_work_j:f64}
pub struct WorkLinearization{trace:CsrMatrix,force:Vec<f64>,difference:Vec<f64>,body_work_j:f64}
pub fn evaluate(trace:&CsrMatrix,begin:&[f64],end:&[f64],force:&[f64],fluid_work_j:f64,previous_defect_j:f64,previous_abs_work_j:f64)->CaeResult<WorkLedger>{
 finite(begin,trace.ncols())?;finite(end,trace.ncols())?;finite(force,trace.nrows())?;
 if [fluid_work_j,previous_defect_j,previous_abs_work_j].iter().any(|x|!x.is_finite())||previous_abs_work_j<0.{return Err(fail("interface work ledger input domain"));}
 let a=trace.matvec(begin).map_err(|_|fail("interface work begin trace"))?;let b=trace.matvec(end).map_err(|_|fail("interface work end trace"))?;
 let work=force.iter().zip(b.iter().zip(&a)).map(|(f,(b,a))|f*(b-a)).sum::<f64>();let defect=fluid_work_j-work;
 let ledger=WorkLedger{body_work_j:work,defect_j:defect,cumulative_defect_j:previous_defect_j+defect,cumulative_abs_work_j:previous_abs_work_j+work.abs()};
 if [ledger.body_work_j,ledger.defect_j,ledger.cumulative_defect_j,ledger.cumulative_abs_work_j].iter().any(|x|!x.is_finite()){return Err(fail("interface work ledger overflow"));}Ok(ledger)
}
impl WorkLinearization{
 pub fn new(trace:&CsrMatrix,begin:&[f64],end:&[f64],force:&[f64],fluid_work_j:f64,previous_defect_j:f64,previous_abs_work_j:f64,recorded:WorkLedger)->CaeResult<Self>{
  let actual=evaluate(trace,begin,end,force,fluid_work_j,previous_defect_j,previous_abs_work_j)?;
  for(a,b)in [(actual.body_work_j,recorded.body_work_j),(actual.defect_j,recorded.defect_j),(actual.cumulative_defect_j,recorded.cumulative_defect_j),(actual.cumulative_abs_work_j,recorded.cumulative_abs_work_j)]{if a.to_bits()!=b.to_bits(){return Err(fail("interface work recorded primal identity mismatch"));}}
  if actual.body_work_j==0.{return Err(fail("absolute interface work kink has no ordinary derivative"));}
  let a=trace.matvec(begin).map_err(|_|fail("interface work trace"))?;let b=trace.matvec(end).map_err(|_|fail("interface work trace"))?;let difference=b.iter().zip(a).map(|(b,a)|b-a).collect();
  Ok(Self{trace:trace.clone(),force:force.to_vec(),difference,body_work_j:actual.body_work_j})
 }
 pub fn tangent(&self,d:&WorkDirection)->CaeResult<WorkLedger>{
  finite(&d.begin,self.trace.ncols())?;finite(&d.end,self.trace.ncols())?;finite(&d.force,self.trace.nrows())?;if [d.fluid_work_j,d.previous_defect_j,d.previous_abs_work_j].iter().any(|x|!x.is_finite()){return Err(fail("interface work direction finite"));}
  let a=self.trace.matvec(&d.begin).map_err(|_|fail("interface work direction trace"))?;let b=self.trace.matvec(&d.end).map_err(|_|fail("interface work direction trace"))?;
  let work=dot(&d.force,&self.difference)+self.force.iter().zip(b.iter().zip(a)).map(|(f,(b,a))|f*(b-a)).sum::<f64>();let defect=d.fluid_work_j-work;
  let ledger=WorkLedger{body_work_j:work,defect_j:defect,cumulative_defect_j:d.previous_defect_j+defect,cumulative_abs_work_j:d.previous_abs_work_j+self.body_work_j.signum()*work};
  if [ledger.body_work_j,ledger.defect_j,ledger.cumulative_defect_j,ledger.cumulative_abs_work_j].iter().any(|x|!x.is_finite()){return Err(fail("interface work tangent overflow"));}Ok(ledger)
 }
 pub fn adjoint(&self,bar:WorkLedger)->CaeResult<WorkPullback>{
  if [bar.body_work_j,bar.defect_j,bar.cumulative_defect_j,bar.cumulative_abs_work_j].iter().any(|x|!x.is_finite()){return Err(fail("interface work cotangent finite"));}
  let defect_bar=bar.defect_j+bar.cumulative_defect_j;let work_bar=bar.body_work_j-defect_bar+self.body_work_j.signum()*bar.cumulative_abs_work_j;
  let trace_bar:Vec<_>=self.force.iter().map(|x|work_bar*x).collect();let end=self.trace.matvec_transpose(&trace_bar).map_err(|_|fail("interface work trace transpose"))?;let begin=end.iter().map(|x|-x).collect();let force:Vec<f64>=self.difference.iter().map(|x|work_bar*x).collect();
  finite(&end,self.trace.ncols())?;finite(&force,self.trace.nrows())?;
  if !defect_bar.is_finite(){return Err(fail("interface work cotangent overflow"));}
  Ok(WorkPullback{begin,end,force,fluid_work_j:defect_bar,previous_defect_j:bar.cumulative_defect_j,previous_abs_work_j:bar.cumulative_abs_work_j})
 }
}
