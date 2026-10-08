// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

(()=>{
"use strict";


const exact=new Map(Object.entries({
 "profile.rectangle":"Rectangle profile","profile.circle":"Circle profile","profile.polygon":"Polygon profile","extrude":"Extrude profile","revolve":"Revolve profile","reflect":"Reflect geometry","loft":"Loft profiles","sweep.tube":"Sweep circular tube",
 "model:control":"Topology",
 "x_min":"Minimum X face","x_max":"Maximum X face","y_min":"Minimum Y face","y_max":"Maximum Y face","z_min":"Minimum Z face","z_max":"Maximum Z face",
 "model:phase:fuel":"Fuel passage","model:phase:oxidizer":"Oxidizer passage","model:phase:hot_gas":"Hot-gas passage","model:phase:coolant":"Coolant passage","model:phase:ambient":"External-flow region","model:phase:propellant":"Propellant passage",
 "catalytic_decomposition":"Catalytic decomposition",
 "global_finite_rate":"Global finite-rate chemistry","finite_rate_multispecies":"Finite-rate multispecies chemistry","flamelet_progress_variable":"Flamelet/progress-variable model","finite_rate_eddy_dissipation":"Finite-rate/eddy-dissipation model","thickened_flame_les":"Thickened-flame LES",
 "screening_quasi_1d":"Differentiable screening model","compressible_rans":"Compressible RANS","compressible_urans":"Compressible URANS","compressible_les":"Compressible LES","compressible_dns":"Compressible DNS",
 "fixed_geometry":"Fixed geometry","parametric_geometry":"Parametric geometry","topology_free":"Topology free","shape_free":"Shape free","phase_free":"Phase allocation free","material_free":"Material distribution free","cooling_only":"Cooling geometry only","preserve_interface":"Preserve interface","manufacturing_protected":"Manufacturing-protected","manual_locked":"Locked by user",
 "smooth_worst_case":"Smooth worst case","reinitialize_physics":"Reinitialise physical state","fixed_solid":"Keep solid","fixed_void":"Keep void","preserve_current":"Preserve current design","control_proposal":"Propose design update","favourable_add":"Favourable for material addition","favourable_remove":"Favourable for material removal",
 "no_slip":"No-slip wall","moving_wall":"Moving wall","volume_flow":"Volumetric flow","mass_flow":"Mass flow","traction_outlet":"Traction outlet","mean_zero":"Mean-zero pressure gauge","stokes_brinkman":"Stokes–Brinkman","steady_laminar_navier_stokes_brinkman":"Steady laminar Navier–Stokes–Brinkman","darcy_forchheimer_reduced":"Reduced Darcy–Forchheimer model",
 "minimize":"Minimise","minimise":"Minimise","maximize":"Maximise","maximise":"Maximise","target":"Match target","upper":"Upper bound","lower":"Lower bound","equal":"Equality target",
 "pressure_drop":"Pressure drop","outlet_uniformity":"Outlet-flow uniformity","brinkman_force":"Brinkman force","material_volume_fraction":"Material volume fraction","pumping_power":"Pumping power",
 "coolant_pressure_drop_Pa":"Coolant pressure drop","max_wall_temperature_K":"Maximum wall temperature","cycle_damage":"Accumulated cycle damage","specific_impulse_s":"Specific impulse","feed_pressure_margin_Pa":"Feed pressure margin","feed_pump_power_W":"Feed pump power","injection_vaporization_efficiency":"Injection vaporization efficiency","minimum_weber_number":"Minimum Weber number","maximum_ohnesorge_number":"Maximum Ohnesorge number","first_longitudinal_mode_Hz":"First longitudinal acoustic mode","thermoacoustic_stability_margin":"Thermoacoustic stability margin","radiative_heat_flux_W_m2":"Radiative heat flux","combustion_efficiency":"Combustion efficiency","mixture_uniformity":"Mixture uniformity",
 "compressed_liquid":"Compressed-liquid screening closure","peng_robinson":"Peng–Robinson screening closure",
 "native_unified_history":"Coupled thermo-fluid-solid history","intent_orchestrated":"Automatic multiphysics","resolved_stokes_brinkman":"Resolved Stokes–Brinkman flow","legacy_multiphysics_implicit":"Structural and thermal analysis",
 "native_field_solver":"Native field solver","field_component":"Field component","algebraic_adapter":"Algebraic adapter","unknown":"Status unavailable",
 "gray_radiation":"Gray radiation","native_solid_history":"Native solid history","thermal_exchanges":"Thermal exchanges"
}));

const ACRONYMS=new Map(Object.entries({
 cfd:"CFD",cae:"CAE",rans:"RANS",urans:"URANS",les:"LES",dns:"DNS",si:"SI",api:"API",mcp:"MCP",jax:"JAX",jvp:"JVP",vjp:"VJP",chf:"CHF",tmf:"TMF",gpu:"GPU",cpu:"CPU",rss:"RSS",re:"Re",pe:"Pe",cucrzr:"CuCrZr",legacy_multiphysics:"LEGACY_MULTIPHYSICS",if97:"IF97"
}));
const UNIT_SUFFIXES=[
 ["_W_m3","W/m³"],["_kg_m3","kg/m³"],["_m_s2","m/s²"],["_Pa_s","Pa·s"],
 ["_W_m2","W/m²"],["_W_mK","W/(m·K)"],["_J_m3","J/m³"],["_kg_s","kg/s"],["_m3_s","m³/s"],["_m_s","m/s"],
 ["_m3","m³"],["_m2","m²"],["_Pa","Pa"],["_Hz","Hz"],["_K","K"],["_J","J"],["_W","W"],["_kg","kg"],["_s","s"]
];
const UNIT_DISPLAY=new Map(Object.entries({
 "m^3":"m³","m^2":"m²","m^3/s":"m³/s","m^2/s":"m²/s","W/m^2":"W/m²","J/m^3":"J/m³","W/(m K)":"W/(m·K)","W/mK":"W/(m·K)"
}));

function scalar(value){return value==null?"":String(value).trim();}
function isMachineIdentifier(value){return /^[A-Za-z][A-Za-z0-9_.:-]*$/.test(scalar(value));}
function inferredUnit(identifier){
 const raw=scalar(identifier);
 for(const [suffix,unit] of UNIT_SUFFIXES)if(raw.endsWith(suffix))return unit;
 return "";
}
function displayUnit(value){const unit=scalar(value);return UNIT_DISPLAY.get(unit)||unit;}
function stripUnitSuffix(identifier){
 const raw=scalar(identifier);
 for(const [suffix] of UNIT_SUFFIXES)if(raw.endsWith(suffix))return raw.slice(0,-suffix.length);
 return raw;
}
function wordLabel(identifier){
 const raw=scalar(identifier);if(!raw)return "";
 if(!isMachineIdentifier(raw))return raw;
 let source=stripUnitSuffix(raw).split(".").pop().replace(/([a-z0-9])([A-Z])/g,"$1 $2").replace(/[_:-]+/g," ").trim();
 const words=source.split(/\s+/).filter(Boolean).map(word=>ACRONYMS.get(word.toLowerCase())||word.toLowerCase());
 if(!words.length)return raw;
 if(!ACRONYMS.has(words[0].toLowerCase()))words[0]=words[0].charAt(0).toUpperCase()+words[0].slice(1);
 return words.join(" ");
}
function normaliseMetadata(metadata){
 if(!metadata||typeof metadata!=="object")return {};
 return metadata;
}
function present(identifier,metadata={}){
 const id=scalar(identifier),meta=normaliseMetadata(metadata);
 const suppliedLabel=scalar(meta.label||meta.display_name||meta.displayName||meta.title);
 const known=exact.get(id)||"";
 const label=suppliedLabel||known||wordLabel(id);
 const suppliedUnit=scalar(meta.unit||meta.units);
 const description=scalar(meta.description||meta.help||meta.scope);
 return Object.freeze({
   id,
   label:(label||id).replaceAll(String.fromCharCode(8212),": "),
   unit:displayUnit(suppliedUnit||inferredUnit(id)),
   description:description.replaceAll(String.fromCharCode(8212),": "),
   explicit:Boolean(suppliedLabel||known)
 });
}
function humanize(value,metadata){return present(value,metadata).label;}


function humanizeElement(root){
 if(!root||typeof root.querySelectorAll!=="function")return root;
 const nodes=[];
 if(root.matches?.("[data-implexity-display-id]"))nodes.push(root);
 nodes.push(...root.querySelectorAll("[data-implexity-display-id]"));
 for(const node of nodes){
   const descriptor=present(node.dataset.implexityDisplayId,{
     label:node.dataset.implexityDisplayLabel,
     unit:node.dataset.implexityDisplayUnit,
     description:node.dataset.implexityDisplayDescription
   });
   node.textContent=descriptor.label;
 }
 return root;
}
function setText(node,identifier,metadata){
 if(!node)return null;
 const descriptor=present(identifier,metadata);node.textContent=descriptor.label;return descriptor;
}

const api=Object.freeze({present,humanize,humanizeElement,setText,isMachineIdentifier,inferredUnit,displayUnit,exact});
window.ImplexityText=api;
})();
