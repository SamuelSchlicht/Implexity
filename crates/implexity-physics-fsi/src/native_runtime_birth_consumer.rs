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

use crate::{gas_geometry_time::{GeometryTime,GeometryTimeBase},native_continuous_coupled_field::NativeContinuousCoupledField,native_cut_cell_partition::CutBoundary,native_gas_geometry_adapter::NativeGasGeometryPath,native_normal_birth::NativeNormalBirthStage};
use implexity_core::{CaeError,CaeResult};
use std::{collections::{BTreeMap,BTreeSet},sync::Arc};
fn fail(s:&str)->CaeError{CaeError::contract(s)}
pub(crate) fn source_region_slot<S:GeometryTimeBase>(endpoint:&crate::native_gas_geometry_adapter::NativeGasGeometryEndpoint<S>,region:usize)->CaeResult<usize>{
 if endpoint.refined.regions.len()!=endpoint.aggregated.component_slots.len(){return Err(fail("birth region component metadata shape"));}
 let slots=endpoint.refined.regions.iter().zip(&endpoint.aggregated.component_slots).filter_map(|(r,s)|(*r==Some(region)).then_some(*s)).collect::<BTreeSet<_>>();
 if slots.len()!=1{return Err(fail("birth source region lacks one actual aggregated gas slot"));}Ok(*slots.first().unwrap())
}
impl NativeContinuousCoupledField {
    pub(crate) fn source_region_birth<S:GeometryTimeBase>(&self,old:&crate::native_gas_geometry_adapter::NativeGasGeometryEndpoint<S>,current:&crate::native_gas_geometry_adapter::NativeGasGeometryEndpoint<S>,slot:usize)->CaeResult<bool>{
        if slot>=self.config.layout.slots()||old.aggregated.volumes_m3.len()!=self.config.layout.slots()||current.aggregated.volumes_m3.len()!=self.config.layout.slots()||old.aggregated.active_slots.len()!=self.config.layout.slots()||current.aggregated.active_slots.len()!=self.config.layout.slots(){return Err(fail("birth source region endpoint shape differs"));}
        if !current.aggregated.active_slots[slot]||current.aggregated.volumes_m3[slot].value()<=0.{return Err(fail("birth source region is absent from actual positive gas geometry"));}
        if old.aggregated.volumes_m3[slot].value()==0.{if self.config.layout.representation(slot)?!=crate::gas_continuous_layout::ContinuousGasRepresentation::Kinetic||old.aggregated.active_slots[slot]||old.aggregated.volumes_m3[slot].has_direction(){return Err(fail("zero-volume birth region is not a genuine kinetic slot"));}Ok(true)}else if !old.aggregated.active_slots[slot]||old.aggregated.volumes_m3[slot].value()<0.{Err(fail("birth source region has inconsistent prior geometry"))}else{Ok(false)}
    }
    pub(crate) fn admit_birth_rows<S:GeometryTimeBase>(&self,slots:&[usize],gas:&[S])->CaeResult<()>{for &slot in slots{let range=self.config.layout.range(slot)?;if gas.get(range).ok_or_else(||fail("birth inventory checkpoint shape"))?.iter().any(|v|v.value()!=0.||v.has_direction()){return Err(fail("a genuinely born gas region retains inventory or a nonzero direction"));}}Ok(())}
    pub(crate) fn authentic_birth_facets<S:GeometryTimeBase>(&self,charts:&[usize],design:&[S])->CaeResult<BTreeMap<usize,[CutBoundary;2]>>{
        let adapter=self.config.geometry.as_ref();let owner=adapter.owner.capacity_owner();adapter.kinematics.validate_native_owner_bindings(owner)?;
        let mut density=[Vec::new(),Vec::new()];
        for body in 0..2{for node in 0..owner.nodes[body].len(){let row=node+if body==0{0}else{owner.nodes[0].len()};let(c,w)=owner.phase.row(row);let mut value=0.;for(&j,&w)in c.iter().zip(w){value+=design.get(j).ok_or_else(||fail("birth design is outside native phase map"))?.value()*w;}density[body].push(value);}}
        let mut result=BTreeMap::new();
        for &chart in charts{let features=adapter.kinematics.chart_features(chart)?;let mut facets=[CutBoundary::Cell{axis:0,side:0},CutBoundary::Cell{axis:0,side:0}];
            for body in 0..2{let mut nodes=BTreeSet::new();for feature in &features[body]{for &(node,w)in feature.map(&density[body])?.weights(){if !w.is_finite(){return Err(fail("birth feature weights are invalid"));}if w!=0.{nodes.insert(node);}}}
                let matches=owner.boundary_faces[body].iter().filter(|face|nodes.iter().all(|node|face.contains(node))).collect::<Vec<_>>();if matches.len()!=1{return Err(fail("birth chart lacks one authentic exterior native facet"));}let face=**matches.first().unwrap();let elements=owner.elements[body].iter().enumerate().filter_map(|(i,tet)|face.iter().all(|node|tet.contains(node)).then_some(i)).collect::<Vec<_>>();if elements.len()!=1{return Err(fail("birth facet lacks one actual native element owner"));}facets[body]=CutBoundary::Native{body,element:elements[0],face:Some(face),physical:true};
            }if result.insert(chart,facets).is_some(){return Err(fail("birth chart is repeated"));}
        }Ok(result)
    }
    pub(crate) fn derivative_birth_stages<'a,S:GeometryTimeBase>(&'a self,n:usize,tick:usize,receipt:crate::native_gas_geometry_adapter::NativeProjectedChildReceipt<'a,S>)->CaeResult<Vec<NativeNormalBirthStage<'a,S>>>{
        let charts=receipt.sample().closed_start_charts().iter().copied().filter(|chart|!receipt.sample().closed_end_charts().contains(chart)).collect::<Vec<_>>();let facets=self.authentic_birth_facets(&charts,receipt.design())?;self.birth_stages_for_runtime_receipt(n,tick,receipt,&facets)
    }
    pub(crate) fn trial_birth_stages<'a,S:GeometryTimeBase>(&'a self,n:usize,tick:usize,receipt:crate::native_gas_geometry_adapter::NativeProjectedTrialChildReceipt<'a,S>)->CaeResult<Vec<NativeNormalBirthStage<'a,S>>>{
        let sample=self.trial_birth_sample_for_receipt(n,tick,receipt)?;let charts=sample.closed_start_charts().iter().copied().filter(|chart|!sample.closed_end_charts().contains(chart)).collect::<Vec<_>>();let facets=self.authentic_birth_facets(&charts,sample.design())?;charts.iter().map(|&chart|NativeNormalBirthStage::from_runtime_sample_authority(sample.clone(),chart,facets[&chart].clone())).collect()
    }
    pub(crate) fn primal_birth_stages_from_sample<'a>(&'a self,sample:Arc<crate::native_runtime_birth_sample::NativeRuntimeBirthSample<'a,f64>>)->CaeResult<Vec<NativeNormalBirthStage<'a,f64>>>{
        let charts=sample.closed_start_charts().iter().copied().filter(|chart|!sample.closed_end_charts().contains(chart)).collect::<Vec<_>>();let facets=self.authentic_birth_facets(&charts,sample.design())?;charts.iter().map(|&chart|NativeNormalBirthStage::from_runtime_sample_authority(sample.clone(),chart,facets[&chart].clone())).collect()
    }
    pub(crate) fn consume_birth_stages<'a,S:GeometryTimeBase>(&'a self,path:&mut NativeGasGeometryPath<S>,stages:&[NativeNormalBirthStage<'a,S>])->CaeResult<()>{
        let adapter=self.config.geometry.as_ref();let mut regions=BTreeSet::new();let mut labels=Vec::new();
        let mut record=|stage:&NativeNormalBirthStage<'a,S>,chart:usize,corner:usize,layer:usize,layers:usize,node:usize,paired:usize,origin:(usize,usize,usize),point:[GeometryTime<S>;3],tets:&[(usize,[usize;4])]|->CaeResult<()>{
            let mut incidence=Vec::new();for side in 0..2{let lower=stage.facet_incidence(corner,side)?.residual();let upper=stage.facet_incidence(corner+3,side)?.residual();let fraction=layer as f64/layers as f64;let residual=lower*(1.-fraction)+upper*fraction;let sign=stage.right_time_sign(residual)?;incidence.push(format!("{sign:?}"));}
            for &(region,_)in tets{let slot=source_region_slot(&path.current_geometry,region)?;
                if self.source_region_birth(&path.old_geometry,&path.current_geometry,slot)?{regions.insert(region);}
            }
            labels.push(serde_json::json!([chart,corner,layer,layers,node,paired,origin,tets,point.map(|v|[v.value.value().to_bits(),v.rate.value().to_bits()]),incidence]));Ok(())
        };
        for stage in stages{if let Ok(context)=stage.context(){if !std::ptr::eq(context.projected().adapter(),adapter){return Err(fail("birth stage belongs to another actual runtime adapter"));}adapter.validate_birth_release_owner(path,context.sample())?;adapter.validate_birth_path_inputs(path,context.child_start(),context.child_end(),context.design(),context.time_scale()*context.nominal_tick_step_s())?;let sources=crate::native_layered_birth_source::NativeLayeredBirthSources::from_stage(adapter,stage)?;for source in sources.sources(){source.validate_owner(adapter,stage)?;record(stage,source.chart(),source.corner(),source.layer(),source.layers(),source.node(),source.paired_vertex(),source.original_source(),source.point(),source.tetrahedra())?;}}
            else{let context=stage.runtime_sample()?;context.validate_path_owner(path)?;adapter.validate_birth_path_inputs(path,context.child_start(),context.child_end(),context.design(),context.time_scale()*context.nominal_tick_step_s())?;let sources=crate::native_layered_birth_multicontext::NativeLayeredBirthSources::from_stage(adapter,stage)?;for source in sources.sources(){source.validate_owner(adapter,stage)?;record(stage,source.chart(),source.corner(),source.layer(),source.layers(),source.node(),source.paired_vertex(),source.original_source(),source.point(),source.tetrahedra())?;}}
        }
        path.birth_regions=regions.into_iter().collect();path.birth_route_identity=if labels.is_empty(){String::new()}else{implexity_core::json::canonical_sha256(&serde_json::json!([adapter.kinematics.binding_identity(),labels]))};Ok(())
    }
    pub(crate) fn admit_birth_inventory<S:GeometryTimeBase>(&self,path:&NativeGasGeometryPath<S>,gas:&[S])->CaeResult<()>{
        for region in &path.birth_regions{let slot=source_region_slot(&path.current_geometry,*region)?;self.admit_birth_rows(&[slot],gas)?;}
        Ok(())
    }
}
