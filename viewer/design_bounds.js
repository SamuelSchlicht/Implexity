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

(() => {
  'use strict';
  const scalar = value => typeof value === 'number' && Number.isFinite(value);
  function shape(value) {
    if(scalar(value))return [];
    if(!Array.isArray(value)||!value.length)throw new Error('Bounds require finite numbers or nonempty rectangular numeric arrays.');
    const child=shape(value[0]);
    for(const item of value)if(JSON.stringify(shape(item))!==JSON.stringify(child))throw new Error('Bound arrays must be rectangular.');
    return [value.length,...child];
  }
  function validate(lower,upper,expectedShape=null){
    const lo=shape(lower),hi=shape(upper),actual=lo.length?lo:hi;
    if(lo.length&&hi.length&&JSON.stringify(lo)!==JSON.stringify(hi))throw new Error('Lower and upper tensor bounds must have the same shape.');
    if(actual.length&&expectedShape&&JSON.stringify(actual)!==JSON.stringify(expectedShape))throw new Error('Tensor bounds must match the exact design-coordinate shape.');
    const check=(a,b)=>{if(!Array.isArray(a)&&!Array.isArray(b)){if(!(a<b))throw new Error('Every lower bound must be strictly below its upper bound.');return;}
      const count=Array.isArray(a)?a.length:b.length;for(let i=0;i<count;i++)check(Array.isArray(a)?a[i]:a,Array.isArray(b)?b[i]:b);};
    check(lower,upper);return {lower:structuredClone(lower),upper:structuredClone(upper),shape:actual};
  }
  function valid(lower,upper,expectedShape=null){try{validate(lower,upper,expectedShape);return true;}catch{return false;}}
  function mappedControl(problem){const map=problem?.context?.geometry_design_map;return ['implexity-geometry-design-map/1','implexity-geometry-design-map/2'].includes(map?.schema);}
  window.ImplexityDesignBounds=Object.freeze({validate,valid,shape,mappedControl});
})();
