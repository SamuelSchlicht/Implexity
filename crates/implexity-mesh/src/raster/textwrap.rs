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


const WS: [char; 6] = ['\t', '\n', '\u{b}', '\u{c}', '\r', ' '];

#[must_use]
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let expanded = expand_tabs(text, 8);
    let munged: String = expanded.chars().map(|c| if WS.contains(&c) { ' ' } else { c }).collect();

    let mut chunks: Vec<Vec<char>> = Vec::new();
    for c in munged.chars() {
        let is_ws = c == ' ';
        match chunks.last_mut() {
            Some(last) if (last[0] == ' ') == is_ws => last.push(c),
            _ => chunks.push(vec![c]),
        }
    }
    chunks.reverse();
    let mut lines = Vec::new();
    let is_blank = |c: &Vec<char>| c.iter().all(|ch| ch.is_whitespace());
    while !chunks.is_empty() {
        let mut cur: Vec<Vec<char>> = Vec::new();
        let mut cur_len = 0usize;
        if chunks.last().is_some_and(is_blank) && !lines.is_empty() {
            chunks.pop();
        }
        while let Some(last) = chunks.last() {
            let l = last.len();
            if cur_len + l <= width {
                cur_len += l;
                if let Some(c) = chunks.pop() {
                    cur.push(c);
                }
            } else {
                break;
            }
        }
        if chunks.last().is_some_and(|c| c.len() > width) {
            let space_left = if width < 1 { 1 } else { width - cur_len };
            if let Some(last) = chunks.last_mut() {
                let head: Vec<char> = last[..space_left.min(last.len())].to_vec();
                let tail: Vec<char> = last[space_left.min(last.len())..].to_vec();
                cur.push(head);
                *last = tail;
            }
        }
        if cur.last().is_some_and(is_blank) {
            cur.pop();
        }
        if !cur.is_empty() {
            lines.push(cur.iter().flatten().collect());
        }
    }
    lines
}

fn expand_tabs(text: &str, tabsize: usize) -> String {
    let mut out = String::new();
    let mut col = 0usize;
    for c in text.chars() {
        match c {
            '\t' => {
                let n = tabsize - (col % tabsize);
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
            }
            '\n' | '\r' => {
                out.push(c);
                col = 0;
            }
            _ => {
                out.push(c);
                col += 1;
            }
        }
    }
    out
}

