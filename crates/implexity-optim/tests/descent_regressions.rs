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

//! Regression coverage for constrained descent, step scaling and restoration.

use implexity_core::{
    CaeResult,
    contracts::{CoordinateOptimizationSettings, ResponseSpec},
    pyobj::PyNum,
};
use implexity_optim::{
    NamedArrays,
    candidate_events::CandidateAdmission,
    descent_flow::normalise_descent_flow,
    search::{ExactPoint, SearchResult, TrialInfo, TrialValues},
    stage_search::{
        StageCoordinate, StageResponseProgram, StageSearch, StageSearchHooks,
        normalise_update_metric,
    },
};
use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

fn scalar(v: f64) -> ArrayD<f64> {
    ArrayD::from_elem(IxDyn(&[1]), v)
}
fn named(rows: &[(&str, f64)]) -> NamedArrays {
    rows.iter()
        .map(|(n, v)| (n.to_string(), scalar(*v)))
        .collect()
}
fn value(x: &NamedArrays, n: &str) -> f64 {
    x.get(n).unwrap()[[0]]
}
fn coord(name: &str, lower: f64, upper: f64) -> StageCoordinate {
    StageCoordinate {
        name: name.into(),
        lower: scalar(lower),
        upper: scalar(upper),
        designable: ArrayD::from_elem(IxDyn(&[1]), true),
    }
}

#[derive(Clone, Copy)]
enum Example {
    NonlinearEquality,
    NonlinearAdmission,
    TwoCoordinates,
    BoundRestore,
    UpperBoundRestore,
    Quadratic,
}
impl Example {
    fn point(self, design: &NamedArrays) -> ExactPoint {
        let x = value(design, "x");
        let (j, c, gj, gc) = match self {
            Self::NonlinearEquality | Self::NonlinearAdmission => {
                (0.0, x * x, named(&[("x", 0.0)]), named(&[("x", 2.0 * x)]))
            }
            Self::TwoCoordinates => (
                x,
                x + 2.0 * value(design, "y"),
                named(&[("x", 1.0), ("y", 0.0)]),
                named(&[("x", 1.0), ("y", 2.0)]),
            ),
            Self::BoundRestore => (x, x, named(&[("x", 1.0)]), named(&[("x", 1.0)])),
            Self::UpperBoundRestore => (-x, x, named(&[("x", -1.0)]), named(&[("x", 1.0)])),
            Self::Quadratic => {
                let y = value(design, "y");
                (
                    (x - 0.6).powi(2) + (y - 0.1).powi(2),
                    x + 2.0 * y,
                    named(&[("x", 2.0 * (x - 0.6)), ("y", 2.0 * (y - 0.1))]),
                    named(&[("x", 1.0), ("y", 2.0)]),
                )
            }
        };
        ExactPoint {
            design: design.clone(),
            values: BTreeMap::from([("op0:J".into(), j), ("op0:C".into(), c)]),
            gradients: BTreeMap::from([("op0:J".into(), gj), ("op0:C".into(), gc)]),
            diagnostics: Map::new(),
            descent: BTreeMap::new(),
        }
    }
}
impl StageSearchHooks for Example {
    fn values(&mut self, x: &NamedArrays) -> SearchResult<TrialValues> {
        let p = self.point(x);
        Ok((p.values.clone(), Map::new(), Some(p)))
    }
    fn sensitivities(&mut self, x: &NamedArrays) -> SearchResult<ExactPoint> {
        Ok(self.point(x))
    }
    fn project(&mut self, _: &NamedArrays, x: NamedArrays) -> CaeResult<NamedArrays> {
        Ok(x)
    }
    fn admit(
        &mut self,
        current: &NamedArrays,
        trial: &NamedArrays,
    ) -> CaeResult<Option<CandidateAdmission>> {
        if matches!(self, Self::NonlinearAdmission) {
            let change = (value(trial, "x") - value(current, "x")).abs();
            return Ok(Some(CandidateAdmission {
                max_step_scale: if change > 0.5 { 0.49 / change } else { 1.0 },
                reason: "test movement limit".into(),
                ..Default::default()
            }));
        }
        Ok(None)
    }
    fn accept(
        &mut self,
        _: &NamedArrays,
        _: &NamedArrays,
        _: Option<&CandidateAdmission>,
        p: ExactPoint,
    ) -> CaeResult<(Option<Value>, ExactPoint)> {
        Ok((None, p))
    }
    fn candidates(
        &mut self,
        _: &StageSearch,
        _: &NamedArrays,
        _: f64,
    ) -> CaeResult<Option<Vec<(f64, NamedArrays, usize)>>> {
        Ok(None)
    }
    fn evaluate_candidate(
        &mut self,
        _: &StageSearch,
        _: f64,
        x: &NamedArrays,
        _: usize,
    ) -> SearchResult<TrialValues> {
        self.values(x)
    }
    fn trial_decision(
        &mut self,
        _: i64,
        _: i64,
        _: &str,
        _: &NamedArrays,
        _: &NamedArrays,
        _: f64,
        _: &TrialInfo,
    ) -> CaeResult<()> {
        Ok(())
    }
}
fn search(example: Example, x: NamedArrays, coordinates: Vec<StageCoordinate>) -> StageSearch {
    search_with_flow(example, x, coordinates, &json!("null_space"))
}
fn search_with_flow(
    example: Example,
    x: NamedArrays,
    coordinates: Vec<StageCoordinate>,
    flow: &Value,
) -> StageSearch {
    configured_search(example, x, coordinates, flow, "equal")
}
fn configured_search(
    example: Example,
    x: NamedArrays,
    coordinates: Vec<StageCoordinate>,
    flow: &Value,
    sense: &str,
) -> StageSearch {
    let responses = [
        ResponseSpec::new("J", PyNum::Float(1.0), "minimise", None, PyNum::Float(1.0)).unwrap(),
        ResponseSpec::new(
            "C",
            PyNum::Float(1.0),
            sense,
            Some(PyNum::Float(1.0)),
            PyNum::Float(1.0),
        )
        .unwrap(),
    ];
    let program = StageResponseProgram::new(&responses, &[0], "nominal", 1.0).unwrap();
    let settings = CoordinateOptimizationSettings {
        step_policy: "backtracking".into(),
        move_limit: PyNum::Float(1.0),
        step_fraction: PyNum::Float(0.1),
        ..Default::default()
    };
    settings.validate().unwrap();
    let state = StageSearch::fresh_state(&program, &settings);
    StageSearch::new(
        program,
        BTreeMap::new(),
        normalise_update_metric(None).unwrap(),
        &normalise_descent_flow(Some(flow)).unwrap(),
        settings,
        coordinates,
        example.point(&x),
        state,
    )
    .unwrap()
}

#[test]
fn backtracking_can_reduce_a_nonlinear_restoration_step() {
    let mut hooks = Example::NonlinearEquality;
    let x = named(&[("x", 0.1)]);
    let mut s = search(hooks, x.clone(), vec![coord("x", 0.0, 10.0)]);
    s.settings.bound_tolerance = PyNum::Float(1e-9);
    let zero = named(&[("x", 0.0)]);
    let large = s.trial_from(&mut hooks, &x, &zero, 0.1).unwrap();
    let small = s.trial_from(&mut hooks, &x, &zero, 0.0001).unwrap();
    let before = s.current.total;
    let merit = s
        .trial_merit(&hooks.point(&large).values, Some(&large))
        .unwrap();
    let row = s.iterate(&mut hooks, 0).unwrap();
    println!(
        "range trials: {} vs {}; merit {before} -> {merit}; row={row}",
        value(&large, "x"),
        value(&small, "x")
    );
    assert!(
        row["accepted"].as_bool() == Some(true),
        "all backtracking candidates keep the same overshooting Newton correction"
    );
    assert!(row["armijo_rejections"].as_i64().unwrap() > 0);
    let large_change = value(&large, "x") - 0.1;
    let small_change = value(&small, "x") - 0.1;
    assert!((small_change / large_change - 0.001).abs() < 1e-12);
    assert!((value(&s.current.point.design, "x").powi(2) - 1.0).abs() < 0.99);
    for iteration in 1..30 {
        s.iterate(&mut hooks, iteration).unwrap();
        if (value(&s.current.point.design, "x") - 1.0).abs() < 1e-7 {
            break;
        }
    }
    assert!((value(&s.current.point.design, "x") - 1.0).abs() < 1e-7);
}

#[test]
fn null_space_direction_remains_tangent_after_default_coordinate_scaling() {
    let mut hooks = Example::TwoCoordinates;
    let x = named(&[("x", 0.2), ("y", 0.4)]);
    let mut s = search(
        hooks,
        x.clone(),
        vec![coord("x", 0.0, 1.0), coord("y", 0.0, 1.0)],
    );
    let pg = s.projected_gradient().0;
    let raw_tangent = value(&pg, "x") + 2.0 * value(&pg, "y");
    assert!(raw_tangent.abs() < 1e-10);
    let direction = s.direction(&pg).unwrap();
    let after = s.trial_from(&mut hooks, &x, &direction, 0.1).unwrap();
    let residual = value(&after, "x") + 2.0 * value(&after, "y") - 1.0;
    println!(
        "tangent before scaling={raw_tangent}; trial=({},{}); equality residual={residual}",
        value(&after, "x"),
        value(&after, "y")
    );
    assert!(
        residual.abs() < 1e-10,
        "per-coordinate update scaling destroys the null-space projection"
    );
}

#[test]
fn equality_restoration_can_leave_a_box_bound_inward() {
    for (mut hooks, start) in [
        (Example::BoundRestore, 0.0),
        (Example::UpperBoundRestore, 2.0),
    ] {
        let x = named(&[("x", start)]);
        let mut s = search(hooks, x, vec![coord("x", 0.0, 2.0)]);
        let row = s.iterate(&mut hooks, 0).unwrap();
        assert_eq!(row["accepted"].as_bool(), Some(true));
        assert!((value(&s.current.point.design, "x") - 1.0).abs() < 1e-8);
    }
}

#[test]
fn restoration_keeps_coordinates_blocked_by_objective_descent() {
    let mut hooks = Example::TwoCoordinates;
    let x = named(&[("x", 0.0), ("y", 0.0)]);
    let mut s = search(hooks, x, vec![coord("x", 0.0, 2.0), coord("y", 0.0, 2.0)]);
    assert!(s.projected_gradient().1 < 1e-10);
    let row = s.iterate(&mut hooks, 0).unwrap();
    assert_eq!(row["accepted"].as_bool(), Some(true));
    assert!(value(&s.current.point.design, "x") > 0.0);
    assert!(value(&s.current.point.design, "y") > 0.0);
    assert!((s.current.point.values["op0:C"] - 1.0).abs() < 1e-10);
}

#[test]
fn feasible_bound_optimum_has_a_zero_tangent() {
    let mut hooks = Example::TwoCoordinates;
    let x = named(&[("x", 0.0), ("y", 0.5)]);
    let mut s = search(
        hooks,
        x.clone(),
        vec![coord("x", 0.0, 2.0), coord("y", 0.0, 2.0)],
    );
    assert_eq!(s.projected_gradient().1, 0.0);
    let row = s.iterate(&mut hooks, 0).unwrap();
    assert_eq!(row["accepted"].as_bool(), Some(false));
    assert!(s.current.point.design.arrays_equal(&x));
}

#[test]
fn coordinate_scales_move_limits_and_box_bounds_preserve_linear_constraints() {
    for metric in ["global_max", "gradient_adaptive", "family_balanced"] {
        for move_limit in [0.02, 1.0] {
            let mut hooks = Example::TwoCoordinates;
            let x = named(&[("x", 0.02), ("y", 0.49)]);
            let mut s = search(
                hooks,
                x.clone(),
                vec![coord("x", 0.0, 2.0), coord("y", 0.0, 4.0)],
            );
            s.step_scales = BTreeMap::from([("x".into(), 0.25), ("y".into(), 2.0)]);
            s.settings.move_limit = PyNum::Float(move_limit);
            s.update_metric = normalise_update_metric(Some(&json!(metric))).unwrap();
            let pg = s.projected_gradient().0;
            let direction = s.direction(&pg).unwrap();
            let step = 0.1_f64.min(move_limit);
            let large = s.trial_from(&mut hooks, &x, &direction, step).unwrap();
            let small = s
                .trial_from(&mut hooks, &x, &direction, step * 0.25)
                .unwrap();
            assert!((value(&large, "x") + 2.0 * value(&large, "y") - 1.0).abs() < 1e-10);
            assert!((value(&small, "x") + 2.0 * value(&small, "y") - 1.0).abs() < 1e-10);
            assert!(value(&large, "x") >= -1e-14 && value(&large, "x") < 0.02);
            assert!((value(&large, "x") - 0.02).abs() <= 0.25 * move_limit * 2.0 + 1e-14);
            assert!((value(&small, "x") - 0.02 - 0.25 * (value(&large, "x") - 0.02)).abs() < 1e-14);
        }
    }
}

#[test]
fn capped_restoration_still_shrinks_during_backtracking() {
    let mut hooks = Example::NonlinearEquality;
    let x = named(&[("x", 0.1)]);
    let mut s = search(hooks, x.clone(), vec![coord("x", 0.0, 2.0)]);
    s.settings.move_limit = PyNum::Float(0.2);
    let zero = named(&[("x", 0.0)]);
    let large = s.trial_from(&mut hooks, &x, &zero, 0.1).unwrap();
    let small = s.trial_from(&mut hooks, &x, &zero, 0.05).unwrap();
    assert!((value(&large, "x") - 0.5).abs() < 1e-12);
    assert!((value(&small, "x") - 0.3).abs() < 1e-12);
}

#[test]
fn restoration_preserves_frozen_coordinates() {
    let mut hooks = Example::TwoCoordinates;
    let x = named(&[("x", 0.0), ("y", 0.4)]);
    let mut y = coord("y", 0.0, 2.0);
    y.designable.fill(false);
    let mut s = search(hooks, x, vec![coord("x", 0.0, 2.0), y]);
    let row = s.iterate(&mut hooks, 0).unwrap();
    assert_eq!(row["accepted"].as_bool(), Some(true));
    assert_eq!(value(&s.current.point.design, "y"), 0.4);
    assert!((value(&s.current.point.design, "x") - 0.2).abs() < 1e-10);
}

#[test]
fn accelerated_directions_stay_on_a_linear_constraint() {
    for directions in ["steepest", "conjugate", "quasi_newton"] {
        let mut hooks = Example::Quadratic;
        let x = named(&[("x", 0.2), ("y", 0.4)]);
        let mut s = search_with_flow(
            hooks,
            x,
            vec![coord("x", 0.0, 1.0), coord("y", 0.0, 1.0)],
            &json!({"mode":"null_space","directions":directions}),
        );
        for iteration in 0..30 {
            s.iterate(&mut hooks, iteration).unwrap();
            assert!(
                (s.current.point.values["op0:C"] - 1.0).abs() < 1e-9,
                "{directions}"
            );
            if s.current.point.values["op0:J"] < 0.008001 {
                break;
            }
        }
        assert!(s.current.point.values["op0:J"] < 0.008001, "{directions}");
    }
}

#[test]
fn augmented_lagrangian_keeps_its_original_coordinate_update() {
    let mut hooks = Example::BoundRestore;
    let x = named(&[("x", 0.75)]);
    let mut s = search_with_flow(
        hooks,
        x.clone(),
        vec![coord("x", 0.0, 2.0)],
        &json!("augmented_lagrangian"),
    );
    assert!(s.manifold.is_none());
    let pg = s.projected_gradient().0;
    let direction = s.direction(&pg).unwrap();
    let trial = s.trial_from(&mut hooks, &x, &direction, 0.1).unwrap();
    assert!((value(&trial, "x") - 0.55).abs() < 1e-12);
}

#[test]
fn admission_rescaling_reduces_constraint_restoration() {
    let mut hooks = Example::NonlinearAdmission;
    let x = named(&[("x", 0.1)]);
    let mut s = search(hooks, x, vec![coord("x", 0.0, 10.0)]);
    let row = s.iterate(&mut hooks, 0).unwrap();
    assert_eq!(row["accepted"].as_bool(), Some(true));
    assert!(row["step_fraction"].as_f64().unwrap() < 0.1);
    assert!((value(&s.current.point.design, "x") - 0.59).abs() < 1e-10);
}

#[test]
fn inequality_restoration_can_leave_either_box_bound() {
    for (mut hooks, start, sense) in [
        (Example::BoundRestore, 0.0, "lower"),
        (Example::UpperBoundRestore, 2.0, "upper"),
    ] {
        let x = named(&[("x", start)]);
        let mut s = configured_search(
            hooks,
            x,
            vec![coord("x", 0.0, 2.0)],
            &json!("null_space"),
            sense,
        );
        let row = s.iterate(&mut hooks, 0).unwrap();
        assert_eq!(row["accepted"].as_bool(), Some(true));
        assert!((value(&s.current.point.design, "x") - 1.0).abs() < 1e-8);
    }
}

#[test]
fn fixed_step_retains_the_configured_restoration_fraction() {
    let mut hooks = Example::BoundRestore;
    let x = named(&[("x", 0.0)]);
    let mut s = search_with_flow(
        hooks,
        x,
        vec![coord("x", 0.0, 2.0)],
        &json!({"mode":"null_space","range_fraction":0.25}),
    );
    s.settings.step_policy = "fixed_step".into();
    let row = s.iterate(&mut hooks, 0).unwrap();
    assert_eq!(row["accepted"].as_bool(), Some(true));
    assert!((value(&s.current.point.design, "x") - 0.25).abs() < 1e-10);
}
