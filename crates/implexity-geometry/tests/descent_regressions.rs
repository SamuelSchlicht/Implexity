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

//! Regression coverage for lattice metric units and morphology adjoints.

use implexity_geometry::lattice::controls::initial_controls;
use implexity_geometry::lattice::descent::{DescentMetric, control_spacing, dirichlet_riesz};
use implexity_geometry::lattice::node::LatticeSpec;
use implexity_geometry::node::Attr;
use serde_json::json;
use std::collections::BTreeMap;

#[test]
fn phase_smoothing_uses_declared_millimetres() {
    let mut attrs = BTreeMap::from([
        ("analysis_grid".into(), Attr::List(vec![Attr::Int(6); 3])),
        ("control_grid".into(), Attr::List(vec![Attr::Int(3); 3])),
        ("domain_mm".into(), Attr::List(vec![Attr::Float(3.0); 3])),
        ("period_mm".into(), Attr::Float(2.0)),
    ]);
    let spec = LatticeSpec::from_attrs(&mut attrs).unwrap();
    let controls = initial_controls(spec.control_grid);
    let fields = spec.geometry_fields(&controls).unwrap();
    let ar: Vec<f64> = (0..fields.rho.len())
        .map(|i| ((i * 17 % 31) as f64 - 15.0) / 15.0)
        .collect();
    let ap = vec![0.0; ar.len()];
    let metric = DescentMetric::from_value(&json!({"phase_length_mm": 0.25})).unwrap();
    let actual = fields.descent(&ar, &ap, &metric).unwrap();
    let gradient = fields.vjp(&ar, &ap).unwrap();
    let state = fields.state();
    let (n, weights) = state.control_nodes();
    let spacing_m = control_spacing(state.grids());
    let mut max_relative_error = 0.0_f64;
    for c in 2..8 {
        let expected = dirichlet_riesz(
            spec.control_grid,
            spacing_m,
            &weights,
            metric.phase_length_mm * 1e-3,
            &gradient[c * n..(c + 1) * n],
        )
        .unwrap();
        let norm = expected.iter().map(|v| v * v).sum::<f64>().sqrt();
        if norm > 1e-12 {
            let err = expected
                .iter()
                .zip(&actual[c * n..(c + 1) * n])
                .map(|(a, b)| (a - b) * (a - b))
                .sum::<f64>()
                .sqrt()
                / norm;
            max_relative_error = max_relative_error.max(err);
        }
    }
    println!("phase metric relative discrepancy from mm-consistent equation: {max_relative_error}");
    assert!(
        max_relative_error < 1e-8,
        "declared millimetres are used as metres: relative error {max_relative_error}"
    );
}

#[test]
fn wall_and_gap_morphology_preserve_the_directional_adjoint() {
    for (wall, gap) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut attrs = BTreeMap::from([
            ("analysis_grid".into(), Attr::List(vec![Attr::Int(9); 3])),
            ("domain_mm".into(), Attr::List(vec![Attr::Float(3.0); 3])),
            ("period_mm".into(), Attr::Float(2.0)),
        ]);
        if wall {
            attrs.insert("minimum_wall_mm".into(), Attr::Float(1.0));
        }
        if gap {
            attrs.insert("minimum_gap_mm".into(), Attr::Float(1.0));
        }
        let spec = LatticeSpec::from_attrs(&mut attrs).unwrap();
        let mut controls = initial_controls(spec.control_grid);
        for (i, x) in controls.data.iter_mut().enumerate() {
            *x += 0.13 * ((i * 23 % 47) as f64 - 23.0) / 23.0;
        }
        let fields = spec.geometry_fields(&controls).unwrap();
        let ar: Vec<f64> = (0..fields.rho.len())
            .map(|i| ((i * 17 % 31) as f64 - 15.0) / 15.0)
            .collect();
        let ap: Vec<f64> = (0..fields.rho.len())
            .map(|i| ((i * 11 % 29) as f64 - 14.0) / 14.0)
            .collect();
        let g = fields.vjp(&ar, &ap).unwrap();
        let direction: Vec<f64> = (0..controls.data.len())
            .map(|i| ((i * 7 % 37) as f64 - 18.0) / 18.0)
            .collect();
        let adjoint: f64 = g.iter().zip(&direction).map(|(g, d)| g * d).sum();
        let f = |sign: f64| {
            let mut trial = controls.clone();
            for (x, d) in trial.data.iter_mut().zip(&direction) {
                *x += sign * 1e-6 * d;
            }
            let trial = spec.geometry_fields(&trial).unwrap();
            trial.rho.iter().zip(&ar).map(|(a, b)| a * b).sum::<f64>()
                + trial
                    .phase_fraction
                    .iter()
                    .zip(&ap)
                    .map(|(a, b)| a * b)
                    .sum::<f64>()
        };
        let fd = (f(1.0) - f(-1.0)) / (2e-6);
        let error = (fd - adjoint).abs() / fd.abs().max(adjoint.abs()).max(1e-8);
        println!(
            "morphology wall={wall} gap={gap}: adjoint={adjoint}, finite_difference={fd}, relative_error={error}"
        );
        assert!(error < 1e-5);
    }
}
