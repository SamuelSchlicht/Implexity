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

use crate::native_gap_column_owner::NativeGapColumnOwner;
use crate::native_union_boundary::PhysicalFacet;
use implexity_ad::Scalar;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
struct Vertex<S: Scalar> { feature: Vec<usize>, point: [S; 3], phase: S }

fn iso_vertex<S: Scalar>(a: &Vertex<S>, b: &Vertex<S>, iso: f64) -> Vertex<S> {
    let (a,b)=if a.feature <= b.feature {(a,b)}else{(b,a)};
    let fraction = (S::from_f64(iso) - a.phase) / (b.phase - a.phase);
    let body = a.feature[1];
    let nodes = [a.feature[2].min(b.feature[2]), a.feature[2].max(b.feature[2])];
    Vertex { feature: vec![1, body, nodes[0], nodes[1]], point: std::array::from_fn(|k| a.point[k] + (b.point[k] - a.point[k]) * fraction), phase: S::from_f64(iso) }
}

fn triangulate<S: Scalar>(owner: Vec<usize>, polygon: &[Vertex<S>], out: &mut Vec<PhysicalFacet<S>>) {
    for i in 1..polygon.len().saturating_sub(1) {
        let ids = [0, i, i + 1];
        out.push(PhysicalFacet { owner: owner.clone(), vertices: ids.map(|j| polygon[j].point), vertex_features: ids.map(|j| polygon[j].feature.clone()) });
    }
}

fn normal<S: Scalar>(p: [[S; 3]; 3]) -> [S; 3] {
    let a: [S; 3] = std::array::from_fn(|k| p[1][k] - p[0][k]);
    let b: [S; 3] = std::array::from_fn(|k| p[2][k] - p[0][k]);
    [a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0]]
}

pub fn authentic_native_facets<S: Scalar>(
    owner: &NativeGapColumnOwner,
    trace: &[S],
    design: &[S],
    same_motion: impl Fn(S, S) -> bool,
) -> Result<Vec<PhysicalFacet<S>>, String> {
    owner.evaluate(trace,design).map_err(|error| error.to_string())?;
    let mut phase = vec![S::zero();owner.phase.nrows()];
    for (i,value) in phase.iter_mut().enumerate() {
        let (columns,weights)=owner.phase.row(i);
        *value=columns.iter().zip(weights).map(|(&j,&w)|design[j]*w).fold(S::zero(),|a,b|a+b);
    }
    let mut facets=Vec::new();
    for body in 0..2 {
        let offset=if body==0{0}else{owner.nodes[0].len()};
        let exterior:BTreeSet<_>=owner.boundary_faces[body].iter().copied().collect();
        for (element,indices) in owner.elements[body].iter().enumerate() {
            let vertices=indices.map(|i|Vertex{feature:vec![0,body,i],point:std::array::from_fn(|k|trace[owner.nodes[body][i].force_flux[k]]+owner.nodes[body][i].reference_m[k]),phase:phase[offset+i]});
            if vertices.iter().all(|v|v.phase.value()<owner.iso){continue;}
            for v in &vertices {
                if v.phase.value()==owner.iso && !same_motion(v.phase,S::from_f64(owner.iso)) {
                    return Err("native phase vertex crosses a physical iso stratum".into());
                }
            }
            let mut iso_points=BTreeMap::<Vec<usize>,Vertex<S>>::new();
            let mut iso_adjacency=BTreeMap::<Vec<usize>,BTreeSet<Vec<usize>>>::new();
            for face in [[0,1,2,3],[0,1,3,2],[0,2,3,1],[1,2,3,0]] {
                let mut ids=[face[0],face[1],face[2]];
                let outward=normal(ids.map(|i|vertices[i].point));
                let toward=(0..3).map(|k|outward[k]*(vertices[face[3]].point[k]-vertices[ids[0]].point[k])).fold(S::zero(),|a,b|a+b);
                if toward.value()==0. || !toward.value().is_finite(){return Err("native physical facet loses tetrahedral rank".into());}
                if toward.value()>0.{ids.swap(1,2);}
                let mut polygon=Vec::new();let mut section=BTreeMap::<Vec<usize>,Vertex<S>>::new();
                for i in 0..3 {
                    let a=&vertices[ids[i]];let b=&vertices[ids[(i+1)%3]];
                    if a.phase.value()>=owner.iso {polygon.push(a.clone());}
                    if a.phase.value()==owner.iso {section.insert(a.feature.clone(),a.clone());}
                    if (a.phase.value()-owner.iso)*(b.phase.value()-owner.iso)<0. {
                        let p=iso_vertex(a,b,owner.iso);polygon.push(p.clone());section.insert(p.feature.clone(),p);
                    }
                }
                let mut face_nodes=ids.map(|i|indices[i]);face_nodes.sort();
                if (exterior.contains(&face_nodes) || ids.iter().all(|&i|vertices[i].phase.value()==owner.iso)) && polygon.len()>=3 {
                    triangulate([vec![0,body],face_nodes.to_vec()].concat(),&polygon,&mut facets);
                }
                if section.len()==2 {
                    let pair:Vec<_>=section.keys().cloned().collect();
                    iso_adjacency.entry(pair[0].clone()).or_default().insert(pair[1].clone());
                    iso_adjacency.entry(pair[1].clone()).or_default().insert(pair[0].clone());
                    iso_points.extend(section);
                } else if section.len()>2 && vertices.iter().any(|v|v.phase.value()<owner.iso) {
                    return Err("coincident native phase facet requires an explicit event owner".into());
                }
            }
            if vertices.iter().any(|v|v.phase.value()<owner.iso) && vertices.iter().any(|v|v.phase.value()>owner.iso) {
                if iso_adjacency.len()<3 || iso_adjacency.values().any(|neighbors|neighbors.len()!=2) {
                    return Err("native phase facet semantic edge graph is not a unique loop".into());
                }
                let start=iso_adjacency.keys().next().unwrap().clone();let mut current=start.clone();let mut previous=None;let mut loop_keys=Vec::new();
                loop {
                    loop_keys.push(current.clone());let next=iso_adjacency[&current].iter().find(|key|Some(*key)!=previous.as_ref()).unwrap().clone();previous=Some(current);current=next;
                    if current==start{break;}if loop_keys.len()>=iso_adjacency.len(){return Err("native phase facet edge graph does not close".into());}
                }
                if loop_keys.len()!=iso_adjacency.len(){return Err("native phase facet has disconnected edge graph".into());}
                let mut polygon:Vec<_>=loop_keys.iter().map(|key|iso_points[key].clone()).collect();
                let n=normal([polygon[0].point,polygon[1].point,polygon[2].point]);
                let solid=vertices.iter().find(|v|v.phase.value()>owner.iso).unwrap();
                let toward=(0..3).map(|k|n[k]*(solid.point[k]-polygon[0].point[k])).fold(S::zero(),|a,b|a+b);
                if toward.value()>0.{polygon.reverse();}
                triangulate(vec![1,body,element],&polygon,&mut facets);
            }
        }
    }
    Ok(facets)
}
