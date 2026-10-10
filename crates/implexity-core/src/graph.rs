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


use std::collections::{BTreeMap, BTreeSet};

#[must_use]
pub fn strongly_connected(
    nodes: &BTreeSet<String>,
    adj: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<BTreeSet<String>> {
    struct T<'a> {
        adj: &'a BTreeMap<String, BTreeSet<String>>,
        index: usize,
        stack: Vec<String>,
        indices: BTreeMap<String, usize>,
        low: BTreeMap<String, usize>,
        on: BTreeSet<String>,
        sccs: Vec<BTreeSet<String>>,
    }
    impl T<'_> {
        fn strong(&mut self, v: &str) {
            self.indices.insert(v.to_string(), self.index);
            self.low.insert(v.to_string(), self.index);
            self.index += 1;
            self.stack.push(v.to_string());
            self.on.insert(v.to_string());
            let next: Vec<String> = self.adj.get(v).map(|s| s.iter().cloned().collect()).unwrap_or_default();
            for w in next {
                if !self.indices.contains_key(&w) {
                    self.strong(&w);
                    let lw = self.low.get(&w).copied().unwrap_or(usize::MAX);
                    if let Some(lv) = self.low.get_mut(v) {
                        *lv = (*lv).min(lw);
                    }
                } else if self.on.contains(&w) {
                    let iw = self.indices.get(&w).copied().unwrap_or(usize::MAX);
                    if let Some(lv) = self.low.get_mut(v) {
                        *lv = (*lv).min(iw);
                    }
                }
            }
            if self.low.get(v) == self.indices.get(v) {
                let mut comp = BTreeSet::new();
                while let Some(w) = self.stack.pop() {
                    self.on.remove(&w);
                    let done = w == v;
                    comp.insert(w);
                    if done {
                        break;
                    }
                }
                if comp.len() > 1 {
                    self.sccs.push(comp);
                }
            }
        }
    }
    let mut t = T {
        adj,
        index: 0,
        stack: Vec::new(),
        indices: BTreeMap::new(),
        low: BTreeMap::new(),
        on: BTreeSet::new(),
        sccs: Vec::new(),
    };
    for a in nodes {
        if !t.indices.contains_key(a) {
            t.strong(a);
        }
    }
    t.sccs
}


