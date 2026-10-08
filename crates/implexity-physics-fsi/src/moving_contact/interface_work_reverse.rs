// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
