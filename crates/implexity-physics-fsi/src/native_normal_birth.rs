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

use crate::{
    gas_gap_mass::GasMassScalar,
    gas_geometry_time::{GeometryTime, GeometryTimeBase},
    moving_contact::surface_map::SurfaceFeature,
    native_closed_release::NativeClosedReleaseSample,
    native_cut_cell_partition::CutBoundary,
    native_gap_column_owner::NativeGapColumnOwner,
    surface_gap::SurfaceGapKinematics,
};
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use std::sync::Arc;
fn fail(s: &str) -> CaeError {
    CaeError::contract(s)
}
fn sub<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    std::array::from_fn(|k| a[k] - b[k])
}
fn dot<S: Scalar>(a: [S; 3], b: [S; 3]) -> S {
    (0..3).fold(S::zero(), |s, k| s + a[k] * b[k])
}
fn cross<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn zero_outer<S: GeometryTimeBase>(v: S) -> CaeResult<()> {
    if !v.finite_first_order() || v.value() != 0. {
        return Err(fail("normal birth does not retain its exact closed primal"));
    }
    match v.geometry_first_order_directions() {
        Some(d) if d.iter().all(|x| x.is_finite() && *x == 0.) => Ok(()),
        None if !v.has_direction() => Ok(()),
        _ => Err(fail("normal birth leaves the fixed outer closed stratum")),
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RightTimeSign {
    Negative,
    Zero,
    Positive,
}
#[derive(Clone)]
pub(crate) struct NativeBirthFacetIncidence<S: GeometryTimeBase> {
    source_corner: usize,
    target_side: usize,
    residual: GeometryTime<S>,
    sign: RightTimeSign,
}
impl<S: GeometryTimeBase> NativeBirthFacetIncidence<S> {
    pub(crate) fn source_corner(&self) -> usize {
        self.source_corner
    }
    pub(crate) fn target_side(&self) -> usize {
        self.target_side
    }
    pub(crate) fn residual(&self) -> GeometryTime<S> {
        self.residual
    }
    pub(crate) fn sign(&self) -> RightTimeSign {
        self.sign
    }
}
pub(crate) struct NativeNormalBirthContext<'a,S: GeometryTimeBase> {
    projected: crate::native_gas_geometry_adapter::NativeProjectedChildReceipt<'a,S>,
    runtime_clock: Option<crate::native_runtime_birth_clock::NativeRuntimeBirthClock<'a,S>>,
    sample: Arc<NativeClosedReleaseSample>,
    binding_identity: String,
    macro_start: Vec<f64>,
    macro_end: Vec<f64>,
    child_start: Vec<S>,
    child_end: Vec<S>,
    design: Vec<S>,
    time_scale: S,
    nominal_tick_step_s: f64,
}
impl<'a,S: GeometryTimeBase> NativeNormalBirthContext<'a,S> {
    pub(crate) fn projected(&self) -> &crate::native_gas_geometry_adapter::NativeProjectedChildReceipt<'a,S> { &self.projected }
    pub(crate) fn sample(&self) -> &NativeClosedReleaseSample { &self.sample }
    pub(crate) fn binding_identity(&self) -> &str { &self.binding_identity }
    pub(crate) fn macro_start(&self) -> &[f64] { &self.macro_start }
    pub(crate) fn macro_end(&self) -> &[f64] { &self.macro_end }
    pub(crate) fn child_start(&self) -> &[S] { &self.child_start }
    pub(crate) fn child_end(&self) -> &[S] { &self.child_end }
    pub(crate) fn design(&self) -> &[S] { &self.design }
    pub(crate) fn time_scale(&self) -> S { self.time_scale }
    pub(crate) fn nominal_tick_step_s(&self) -> f64 { self.nominal_tick_step_s }
}
#[derive(Clone)]
pub(crate) struct NativeNormalBirthStage<'a,S: GeometryTimeBase> {
    chart: usize,
    bindings: [CutBoundary; 2],
    vertices: [[GeometryTime<S>; 3]; 6],
    normal: [S; 3],
    normal_rates: [S; 3],
    right_volume_rate_m3_s: S,
    context: Option<Arc<NativeNormalBirthContext<'a,S>>>,
    runtime_sample: Option<Arc<crate::native_runtime_birth_sample::NativeRuntimeBirthSample<'a,S>>>,
}
impl<'a,S: GeometryTimeBase> NativeNormalBirthStage<'a,S> {
    pub(crate) fn from_strict_sample(
        kinematics: &SurfaceGapKinematics,
        owner: &NativeGapColumnOwner,
        sample: &NativeClosedReleaseSample,
        macro_start: &[f64],
        macro_end: &[f64],
        child_start: &[S],
        child_end: &[S],
        design: &[S],
        time_scale: S,
        nominal_tick_step_s: f64,
        chart: usize,
        bindings: [CutBoundary; 2],
    ) -> CaeResult<Self> {
        if !nominal_tick_step_s.is_finite()
            || nominal_tick_step_s <= 0.
            || !time_scale.finite_first_order()
            || time_scale.value() <= 0.
            || child_start.len() != owner.trace_size
            || child_end.len() != owner.trace_size
            || child_start
                .iter()
                .chain(child_end)
                .chain(design)
                .any(|v| !v.finite_first_order())
        {
            return Err(fail("invalid normal birth sample inputs"));
        }
        sample.parent().validate_time_scale(time_scale.value())?;
        sample.validate_path(
            kinematics,
            macro_start,
            macro_end,
            &child_start.iter().map(|v| v.value()).collect::<Vec<_>>(),
            &child_end.iter().map(|v| v.value()).collect::<Vec<_>>(),
            &design.iter().map(|v| v.value()).collect::<Vec<_>>(),
        )?;
        if sample.fractions().0 != 0.
            || !sample.closed_start_charts().contains(&chart)
            || sample.closed_end_charts().contains(&chart)
            || !sample.parent().released_charts().contains(&chart)
        {
            return Err(fail(
                "normal birth requires an authentic released closed-start chart",
            ));
        }
        Self::from_authenticated_chart(kinematics,owner,child_start,child_end,design,time_scale,nominal_tick_step_s,chart,bindings,sample.parent().closed_endpoint_corners(),sample.parent().released_corners())
    }
    fn from_authenticated_chart(
        kinematics:&SurfaceGapKinematics,owner:&NativeGapColumnOwner,child_start:&[S],child_end:&[S],design:&[S],time_scale:S,nominal_tick_step_s:f64,chart:usize,bindings:[CutBoundary;2],maintained:&[(usize,usize)],released_corners:&[(usize,usize)],
    )->CaeResult<Self>{
        kinematics.validate_native_owner_bindings(owner)?;
        let features = kinematics.chart_features(chart)?;
        let mut density = [Vec::new(), Vec::new()];
        for body in 0..2 {
            for node in 0..owner.nodes[body].len() {
                let row = node + if body == 0 { 0 } else { owner.nodes[0].len() };
                let (c, w) = owner.phase.row(row);
                density[body].push(
                    c.iter()
                        .zip(w)
                        .fold(0., |s, (&j, &w)| s + design[j].value() * w),
                );
            }
        }
        for body in 0..2 {
            let (b, element, face) = match bindings[body] {
                CutBoundary::Native {
                    body,
                    element,
                    face: Some(face),
                    physical: true,
                } => (body, element, face),
                _ => {
                    return Err(fail(
                        "normal birth currently requires authentic exterior native facets",
                    ))
                }
            };
            let mut sorted = face;
            sorted.sort_unstable();
            let tet = owner.elements[body]
                .get(element)
                .ok_or_else(|| fail("normal birth native element is absent"))?;
            if b != body
                || !owner.boundary_faces[body].contains(&sorted)
                || face.iter().any(|i| !tet.contains(i))
                || sorted.windows(2).any(|w| w[0] == w[1])
            {
                return Err(fail(
                    "normal birth facet does not own its native element boundary",
                ));
            }
            for feature in &features[body] {
                if let SurfaceFeature::DensityEdge { iso, .. } = feature {
                    if iso.to_bits() != owner.iso.to_bits() {
                        return Err(fail(
                            "normal birth density feature uses another physical iso",
                        ));
                    }
                }
                let map = feature.map(&density[body])?;
                if map
                    .weights()
                    .iter()
                    .any(|&(node, w)| !w.is_finite() || w < 0. || w != 0. && !face.contains(&node))
                {
                    return Err(fail(
                        "normal birth feature is outside its authentic native facet",
                    ));
                }
                let phase = map
                    .weights()
                    .iter()
                    .map(|&(node, w)| w * density[body][node])
                    .sum::<f64>();
                if !phase.is_finite()
                    || !matches!(feature, SurfaceFeature::DensityEdge { .. }) && phase < owner.iso
                {
                    return Err(fail(
                        "normal birth feature is outside actual solid material",
                    ));
                }
            }
        }
        let step = S::from_f64(nominal_tick_step_s) * time_scale;
        let trace = child_start
            .iter()
            .zip(child_end)
            .map(|(&a, &b)| GeometryTime::new(a, (b - a) / step))
            .collect::<Vec<_>>();
        let x = design
            .iter()
            .map(|&v| GeometryTime::new(v, S::zero()))
            .collect::<Vec<_>>();
        let all = kinematics.mapped_geometry_from_trace(&trace, &x, |v, _| Ok(v))?;
        let vertices = *all
            .get(chart)
            .ok_or_else(|| fail("normal birth chart is absent"))?;
        let triangle = [
            vertices[0].map(|v| v.value),
            vertices[1].map(|v| v.value),
            vertices[2].map(|v| v.value),
        ];
        let mut area_vector = cross(sub(triangle[1], triangle[0]), sub(triangle[2], triangle[0]));
        let mut orientation = 1.;
        let native_point = |body: usize, node: usize| -> CaeResult<[S; 3]> {
            let n = owner.nodes[body]
                .get(node)
                .ok_or_else(|| fail("normal birth native node is absent"))?;
            Ok(std::array::from_fn(|k| {
                S::from_f64(n.reference_m[k]) + child_start[n.force_flux[k]]
            }))
        };
        for body in 0..2 {
            let (element, face) = match bindings[body] {
                CutBoundary::Native {
                    element,
                    face: Some(face),
                    ..
                } => (element, face),
                _ => unreachable!(),
            };
            let tet = owner.elements[body][element];
            let opposite = *tet
                .iter()
                .find(|i| !face.contains(i))
                .ok_or_else(|| fail("normal birth facet has no interior native vertex"))?;
            let toward = dot(area_vector, sub(native_point(body, opposite)?, triangle[0]));
            if !toward.finite_first_order() || toward.value() == 0. {
                return Err(fail("normal birth native orientation loses rank"));
            }
            if body == 0 {
                if toward.value() > 0. {
                    area_vector = area_vector.map(|v| -v);
                    orientation = -1.;
                }
            } else if toward.value() <= 0. {
                return Err(fail("normal birth native facets do not oppose the opening"));
            }
        }
        let norm = dot(area_vector, area_vector).sqrt();
        if !norm.finite_first_order() || norm.value() <= 0. {
            return Err(fail("normal birth closed triangle loses area"));
        }
        let normal = area_vector.map(|v| v / norm);
        let raw = cross(sub(vertices[1], vertices[0]), sub(vertices[2], vertices[0]));
        let length = dot(raw, raw).sqrt();
        let time_normal = raw.map(|v| v * orientation / length);
        let gaps = std::array::from_fn::<_, 3, _>(|corner| {
            dot(time_normal, sub(vertices[corner + 3], vertices[corner]))
        });
        for gap in gaps {
            zero_outer(gap.value)?;
        }
        let normal_rates = gaps.map(|v| v.rate);
        let mut released = false;
        for (corner, rate) in normal_rates.iter().enumerate() {
            if !rate.finite_first_order() || rate.value() < 0. {
                return Err(fail("normal birth opens into an inadmissible solid side"));
            }
            if rate.value() == 0. {
                zero_outer(*rate)?;
                if !maintained.contains(&(chart,corner))
                {
                    return Err(fail(
                        "zero-rate birth corner is not an authenticated maintained corner",
                    ));
                }
            } else {
                released = true;
                if !released_corners.contains(&(chart,corner))
                {
                    return Err(fail(
                        "positive normal birth corner is not authenticated released",
                    ));
                }
            }
        }
        if !released {
            return Err(fail(
                "normal birth does not have a strict positive right-time opening",
            ));
        }
        let mut total = GeometryTime::<S>::zero();
        for ids in [[0, 1, 2, 5], [0, 1, 5, 4], [0, 3, 4, 5]] {
            let t = ids.map(|i| vertices[i]);
            total +=
                dot(sub(t[1], t[0]), cross(sub(t[2], t[0]), sub(t[3], t[0]))) * (orientation / 6.);
        }
        zero_outer(total.value)?;
        let right_volume_rate_m3_s = total.rate;
        if !right_volume_rate_m3_s.finite_first_order() || right_volume_rate_m3_s.value() <= 0. {
            return Err(fail("normal birth right-time volume rate is inadmissible"));
        }
        Ok(Self {
            chart,
            bindings,
            vertices,
            normal,
            normal_rates,
            right_volume_rate_m3_s,
            context: None,
            runtime_sample: None,
        })
    }
    pub(crate) fn from_runtime_sample_authority(
        sample:Arc<crate::native_runtime_birth_sample::NativeRuntimeBirthSample<'a,S>>,chart:usize,bindings:[CutBoundary;2],
    )->CaeResult<Self>{
        sample.validate_birth_chart(chart)?;
        let adapter=sample.adapter();
        let mut stage=Self::from_authenticated_chart(&adapter.kinematics,adapter.owner.capacity_owner(),sample.child_start(),sample.child_end(),sample.design(),sample.time_scale(),sample.nominal_tick_step_s(),chart,bindings,sample.maintained_corners(),sample.released_corners())?;
        stage.runtime_sample=Some(sample);Ok(stage)
    }
    pub(crate) fn runtime_sample(&self)->CaeResult<&Arc<crate::native_runtime_birth_sample::NativeRuntimeBirthSample<'a,S>>>{self.runtime_sample.as_ref().ok_or_else(||fail("birth stage has no genuine runtime sample authority"))}
    pub(crate) fn from_projected_child(
        adapter:&'a crate::native_gas_geometry_adapter::NativeGasGeometryAdapter,
        projected:crate::native_gas_geometry_adapter::NativeProjectedChildReceipt<'a,S>,
        nominal_tick_step_s:f64, chart:usize, bindings:[CutBoundary;2],
    ) -> CaeResult<Self> {
        if !std::ptr::eq(adapter,projected.adapter()) { return Err(fail("projected child belongs to another actual adapter")); }
        let kinematics=&adapter.kinematics; let owner=adapter.owner.capacity_owner();
        let sample=projected.sample().clone();
        let macro_start=projected.macro_start().iter().map(|v|v.value()).collect::<Vec<_>>();
        let macro_end=projected.macro_end().iter().map(|v|v.value()).collect::<Vec<_>>();
        let child_start=projected.child_start().to_vec();let child_end=projected.child_end().to_vec();
        let design=projected.design().to_vec(); let time_scale=projected.time_scale();
        let mut stage=Self::from_strict_sample(kinematics,owner,&sample,&macro_start,&macro_end,&child_start,&child_end,&design,time_scale,nominal_tick_step_s,chart,bindings)?;
        stage.context=Some(Arc::new(NativeNormalBirthContext{projected,runtime_clock:None,sample,binding_identity:kinematics.binding_identity(),macro_start,macro_end,child_start,child_end,design,time_scale,nominal_tick_step_s}));
        Ok(stage)
    }
    pub(crate) fn from_runtime_projected_child(
        clock:crate::native_runtime_birth_clock::NativeRuntimeBirthClock<'a,S>,
        projected:crate::native_gas_geometry_adapter::NativeProjectedChildReceipt<'a,S>,
        chart:usize,bindings:[CutBoundary;2],
    )->CaeResult<Self> {
        clock.validate_receipt(&projected)?;
        let mut stage=Self::from_projected_child(clock.adapter(),projected,clock.nominal_step_s(),chart,bindings)?;
        let context=stage.context.as_mut().ok_or_else(||fail("runtime birth context is absent"))?;
        Arc::get_mut(context).ok_or_else(||fail("new runtime birth context is already shared"))?.runtime_clock=Some(clock);
        Ok(stage)
    }
    pub(crate) fn from_shared_context(
        kinematics: &SurfaceGapKinematics, owner: &NativeGapColumnOwner,
        context: Arc<NativeNormalBirthContext<'a,S>>, chart: usize, bindings: [CutBoundary;2],
    ) -> CaeResult<Self> {
        if !std::ptr::eq(kinematics,&context.projected.adapter().kinematics)
            || !std::ptr::eq(owner,context.projected.adapter().owner.capacity_owner())
            || context.binding_identity!=kinematics.binding_identity() {
            return Err(fail("normal birth context belongs to another feature binding"));
        }
        if let Some(clock)=&context.runtime_clock { clock.validate_receipt(&context.projected)?; }
        let mut stage=Self::from_strict_sample(kinematics,owner,context.sample(),context.macro_start(),context.macro_end(),context.child_start(),context.child_end(),context.design(),context.time_scale(),context.nominal_tick_step_s(),chart,bindings)?;
        stage.context=Some(context);Ok(stage)
    }
    pub(crate) fn context(&self) -> CaeResult<&Arc<NativeNormalBirthContext<'a,S>>> {
        self.context.as_ref().ok_or_else(||fail("normal birth stage has no owned source context"))
    }
    pub(crate) fn facet_incidence(
        &self,
        source_corner: usize,
        target_side: usize,
    ) -> CaeResult<NativeBirthFacetIncidence<S>> {
        if source_corner >= 6 || target_side >= 2 {
            return Err(fail(
                "normal birth facet incidence token is outside the sealed chart",
            ));
        }
        let source_side = source_corner / 3;
        let residual = if source_side == target_side {
            GeometryTime::<S>::zero()
        } else {
            let base = target_side * 3;
            let raw = cross(
                sub(self.vertices[base + 1], self.vertices[base]),
                sub(self.vertices[base + 2], self.vertices[base]),
            );
            let alignment = dot(raw.map(|v| v.value), self.normal).value();
            if !alignment.is_finite() || alignment == 0. {
                return Err(fail("normal birth authentic facet orientation loses rank"));
            }
            let orientation = if (target_side == 0) == (alignment > 0.) {
                1.
            } else {
                -1.
            };
            let length = dot(raw, raw).sqrt();
            if !length.finite_first_order() || length.value.value() <= 0. {
                return Err(fail("normal birth authentic facet loses area"));
            }
            let normal = raw.map(|v| v * orientation / length);
            dot(
                normal,
                sub(self.vertices[source_corner], self.vertices[base]),
            )
        };
        let sign = self.right_time_sign(residual)?;
        Ok(NativeBirthFacetIncidence {
            source_corner,
            target_side,
            residual,
            sign,
        })
    }
    pub(crate) fn chart(&self) -> usize {
        self.chart
    }
    pub(crate) fn bindings(&self) -> &[CutBoundary; 2] {
        &self.bindings
    }
    pub(crate) fn vertices(&self) -> &[[GeometryTime<S>; 3]; 6] {
        &self.vertices
    }
    pub(crate) fn normal(&self) -> [S; 3] {
        self.normal
    }
    pub(crate) fn normal_rates(&self) -> [S; 3] {
        self.normal_rates
    }
    pub(crate) fn right_volume_rate_m3_s(&self) -> S {
        self.right_volume_rate_m3_s
    }
    pub(crate) fn right_time_sign(&self, residual: GeometryTime<S>) -> CaeResult<RightTimeSign> {
        if !residual.finite_first_order() {
            return Err(fail("normal birth incidence has nonfinite mixed jets"));
        }
        if residual.value.value() < 0. {
            return Ok(RightTimeSign::Negative);
        }
        if residual.value.value() > 0. {
            return Ok(RightTimeSign::Positive);
        }
        zero_outer(residual.value)?;
        if residual.rate.value() < 0. {
            return Ok(RightTimeSign::Negative);
        }
        if residual.rate.value() > 0. {
            return Ok(RightTimeSign::Positive);
        }
        zero_outer(residual.rate)?;
        Ok(RightTimeSign::Zero)
    }
    pub(crate) fn admit_zero_inventory(&self, inventory: &[S]) -> CaeResult<()> {
        for &q in inventory {
            zero_outer(q)?;
        }
        Ok(())
    }
}
