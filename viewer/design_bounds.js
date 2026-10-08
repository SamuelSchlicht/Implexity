// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
