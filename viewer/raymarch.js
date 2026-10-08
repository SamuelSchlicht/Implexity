// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

const PALETTE = {
  bgTop:   [0.9301, 0.9560, 0.9823],
  bgBot:   [0.7084, 0.7758, 0.8632],
  albedo:  [0.3231, 0.3813, 0.4564],
  cap:     [0.4851, 0.5776, 0.7157],
  accent:  [0.0232, 0.1878, 0.6724],
  key:     [1.0000, 0.9823, 0.9216],
  fill:    [0.5457, 0.6445, 0.7758],
};

const VERT = `#version 300 es
in vec2 a_pos;
out vec2 v_uv;
void main(){ v_uv = a_pos * 0.5 + 0.5; gl_Position = vec4(a_pos, 0.0, 1.0); }`;


const FRAG_HEAD = `#version 300 es
precision highp float;
precision highp int;
precision highp sampler3D;

uniform vec3  u_eye;
uniform vec3  u_fwd;
uniform vec3  u_right;
uniform vec3  u_up;
uniform vec2  u_res;
uniform float u_tanHalfFov;
uniform int u_orthographic;
uniform float u_orthoSpan;
uniform int u_backgroundWhite;

uniform float u_stepFactor;                                         
uniform float u_fixedStep;                                         
uniform int   u_traceFixed;                                
uniform float u_eps;                                                        
uniform float u_epsGrowth;                                         
uniform float u_t0;
uniform float u_tmax;
uniform int   u_maxSteps;
uniform float u_scale;                             

uniform vec3  u_clipO;
uniform vec3  u_clipN;
uniform int   u_clipOn;

                                                                        
                                                                             
                                                                             
                                                                          
                                                                          
uniform int   u_sectionFillOn;
uniform vec3  u_sectionN;
uniform float u_sectionD;
uniform vec3  u_sectionLo;
uniform vec3  u_sectionHi;
uniform vec3  u_sectionFill;

uniform int   u_shadows;
uniform int   u_ao;
uniform int   u_colorOn;
uniform int   u_baseColourOn;
uniform vec3  u_baseColour;
uniform highp sampler3D u_field;
uniform vec3  u_fieldLo;
uniform vec3  u_fieldHi;
uniform vec3  u_fieldDim;
uniform vec2  u_fieldRange;
uniform int   u_fieldPalette;
uniform vec3 u_fieldColorLow;
uniform vec3 u_fieldColorHigh;
uniform float u_fieldThreshold;
uniform int   u_fieldRegistered;
uniform mat3  u_fieldWorldToIndex;
uniform vec3  u_fieldIndexOffset;
uniform vec3  u_fieldIndexDim;

uniform vec3  u_lightDir;
uniform float u_groundZ;                            
uniform int   u_groundOn;
uniform vec3  u_centre;                                           

in  vec2 v_uv;
layout(location = 0) out vec4 o_color;
layout(location = 1) out float o_steps;
`;

const FRAG_TAIL = `
                                                                              
                                                                        
  
                                                                             
                                                                             
                                                                 
                                                                              
float mapc(vec3 p) {
    float d = sdf(p);
    if (u_clipOn == 1) d = max(d, dot(p - u_clipO, u_clipN));
    return d;
}

                                                                         
                                                                         
                                                              
vec3 normalAt(vec3 p, float h) {
    const vec2 k = vec2(1.0, -1.0);
    return normalize(k.xyy * sdf(p + k.xyy * h) +
                     k.yyx * sdf(p + k.yyx * h) +
                     k.yxy * sdf(p + k.yxy * h) +
                     k.xxx * sdf(p + k.xxx * h));
}

                                                                             
                                                              
                                                                             
                                                                      
                                                                        
                                                                        
float ambient(vec3 p, vec3 n) {
    float R = 0.075 * u_scale;
    float occ = 0.0, w = 1.0, norm = 0.0;
    for (int i = 1; i <= 5; i++) {
        float h = R * float(i) / 5.0;
        occ  += max(h - mapc(p + n * h), 0.0) * w;
        norm += h * w;
        w *= 0.78;
    }
    return clamp(1.0 - 1.30 * occ / norm, 0.0, 1.0);
}

                                                                               
                                                                             
                                                     
float shadow(vec3 p, vec3 l) {
    float res = 1.0;
    float t = 0.02 * u_scale;
    float k = 8.0;                                                      
                                                                            
                                                                          
                                                                          
                                                                         
                                                                           
                                                                 
    for (int i = 0; i < 64; i++) {
        float d = mapc(p + l * t);
        if (d < 0.0015 * u_scale) return 0.0;
        res = min(res, k * d / t);
                                                                      
                                                                            
                                                                           
                                                                           
                                                                         
                                                                      
        t += clamp(d * max(u_stepFactor, 0.25), 0.0008 * u_scale, 0.30 * u_scale);
        if (t > 2.0 * u_scale) break;
    }
    return clamp(res, 0.0, 1.0);
}

                                                                           
                                                                           
                                                                  
vec3 sequentialRamp(float t) {
    t = clamp(t, 0.0, 1.0);
    const vec3 c0 = vec3(0.180, 0.263, 0.451);
    const vec3 c1 = vec3(0.161, 0.494, 0.588);
    const vec3 c2 = vec3(0.396, 0.706, 0.588);
    const vec3 c3 = vec3(0.898, 0.796, 0.470);
    const vec3 c4 = vec3(0.816, 0.408, 0.259);
    if (t < 0.25) return mix(c0, c1, t / 0.25);
    if (t < 0.50) return mix(c1, c2, (t - 0.25) / 0.25);
    if (t < 0.75) return mix(c2, c3, (t - 0.50) / 0.25);
    return mix(c3, c4, (t - 0.75) / 0.25);
}

                                                                             
                                                                             
vec3 divergingRamp(float t) {
    t = clamp(t, 0.0, 1.0);
    const vec3 c0 = vec3(0.000, 0.447, 0.698);
    const vec3 c1 = vec3(0.518, 0.718, 0.827);
    const vec3 c2 = vec3(0.957, 0.957, 0.945);
    const vec3 c3 = vec3(0.925, 0.655, 0.435);
    const vec3 c4 = vec3(0.835, 0.369, 0.000);
    if (t < 0.25) return mix(c0, c1, t / 0.25);
    if (t < 0.50) return mix(c1, c2, (t - 0.25) / 0.25);
    if (t < 0.75) return mix(c2, c3, (t - 0.50) / 0.25);
    return mix(c3, c4, (t - 0.75) / 0.25);
}

                                                                           
                                                                            
                                                                              
                           
float dither(vec2 fc) {
    return fract(52.9829189 * fract(dot(fc, vec2(0.06711056, 0.00583715))));
}

                                                                      
                                                              
float cam0Depth(float t) { return t - 0.55 * u_scale; }

vec3 background(vec2 uv) {
    return mix(vec3(${PALETTE.bgBot.join(", ")}),
               vec3(${PALETTE.bgTop.join(", ")}),
               smoothstep(0.0, 1.0, uv.y));
}

void main() {
    vec2 ndc = (gl_FragCoord.xy / u_res) * 2.0 - 1.0;
    float aspect = u_res.x / u_res.y;
    vec3 rd = normalize(u_fwd
                      + u_right * (ndc.x * aspect * u_tanHalfFov)
                      + u_up    * (ndc.y * u_tanHalfFov));
    vec3 ro = u_eye;
    if (u_orthographic == 1) {
        ro += u_right * (ndc.x * aspect * u_orthoSpan * 0.5)
            + u_up * (ndc.y * u_orthoSpan * 0.5);
        rd = normalize(u_fwd);
    }

    float t = u_t0;
    int steps = 0;
    bool hit = false;
    float d = 0.0;

    float traceEnd = u_tmax;
    bool hasTraceInterval = true;
#ifdef IMPLEXITY_FINITE_TRACE_BOUNDS
    vec2 traceInterval;
    hasTraceInterval = finiteTraceInterval(ro, rd, traceInterval);
    if (hasTraceInterval) {
        t = max(0.0, traceInterval.x);
        traceEnd = min(traceEnd, traceInterval.y);
        hasTraceInterval = traceEnd >= t;
    }
#endif

    if (hasTraceInterval) {
    if (u_traceFixed == 1) {
                                                                                   
                                                                            
                                                                            
                                                                            
                                                                            
                                                                          
                                                                      
                                                                
        float prev = mapc(ro + rd * t);
#ifdef IMPLEXITY_FINITE_TRACE_BOUNDS
                                                                              
                                                                           
        if (traceInterval.x >= 0.0 && prev <= 0.0) { hit = true; d = 0.0; }
#endif
        for (int i = 0; i < 1024; i++) {
            if (hit) break;
            if (i >= u_maxSteps) break;
            steps = i + 1;
                                                                           
                                                                               
                                                                           
            float tn = min(t + u_fixedStep, t + 0.5 * (traceEnd - t));
            if (tn <= t) break;
            float dn = mapc(ro + rd * tn);
            if (dn <= 0.0 && prev > 0.0) {
                float a = t, b = tn;
                for (int k = 0; k < 12; k++) {
                    float m = 0.5 * (a + b);
                    if (mapc(ro + rd * m) <= 0.0) b = m; else a = m;
                }
                t = b; d = 0.0; hit = true; break;
            }
            prev = dn; t = tn;
        }
    } else {
        for (int i = 0; i < 1024; i++) {
            if (i >= u_maxSteps) break;
            steps = i + 1;
            vec3 p = ro + rd * t;
            d = mapc(p);
            float eps = u_eps * (1.0 + u_epsGrowth * t / u_scale);
            if (d < eps) { hit = true; break; }
                                                                          
                                                                              
                                                                          
                                                                            
            t += max(d * u_stepFactor, 1.0e-4 * u_scale);
            if (t > traceEnd) break;
        }
    }
    }

                                                                            
                                                                             
    bool sectionFill = false;
    if (u_sectionFillOn == 1) {
        float slope = dot(rd, u_sectionN);
        float excess = dot(ro, u_sectionN) - u_sectionD;
        if (slope < -1.0e-12 && excess > 0.0) {
            float tp = -excess / slope;
            vec3 pp = ro + rd * tp;
            float tol = 1.0e-6 * u_scale;
            bool inBox = all(greaterThanEqual(pp, u_sectionLo - tol))
                      && all(lessThanEqual(pp, u_sectionHi + tol));
            float fillEps = u_eps * (1.0 + u_epsGrowth * tp / u_scale);
            if (inBox && (!hit || tp < t - fillEps) && sdf(pp) > fillEps) {
                sectionFill = true; hit = true; t = tp;
            }
        }
    }

    o_steps = float(steps);
    vec3 col;
    if (!hit) {
        col = u_backgroundWhite == 1 ? vec3(1.0) : background(v_uv);
                                                                             
                                                                               
                                                                              
                                                                             
                                                                          
                                                                       
                                                  
        if (u_groundOn == 1 && u_shadows == 1 && rd.z < -1.0e-4) {
            float tg = (u_groundZ - ro.z) / rd.z;
            if (tg > 0.0 && tg < u_tmax) {
                vec3 g = ro + rd * tg;
                float fall = 1.0 - smoothstep(0.42 * u_scale, 1.10 * u_scale,
                                              length(g.xy - u_centre.xy));
                if (fall > 0.001) {
                    float gs = shadow(g + vec3(0.0, 0.0, 2.0 * u_eps),
                                      normalize(u_lightDir));
                    col *= 1.0 - 0.30 * (1.0 - gs) * fall;
                }
            }
        }
    } else {
        vec3 p = ro + rd * t;
        float onCap = 0.0;
        if (u_clipOn == 1) {
            float pl = dot(p - u_clipO, u_clipN);
            onCap = (pl > -2.0 * u_eps) ? 1.0 : 0.0;
        }
                                                                            
                                                                       
        float onSection = 0.0;
        if (u_sectionFillOn == 1
            && (sectionFill || abs(dot(p, u_sectionN) - u_sectionD) <= 2.0 * u_eps))
            onSection = 1.0;
                                                                            
                                                                               
                                                                              
                                                                              
                                                                               
        vec3 n = mix(normalAt(p, max(0.35 * u_eps, 3.0e-4 * u_scale)),
                     u_clipN, onCap);
        if (onSection > 0.5) n = u_sectionN;

                                                                             
                                                                               
                                                                           
                               
        vec3 base = mix(vec3(${PALETTE.albedo.join(", ")}),
                        vec3(${PALETTE.cap.join(", ")}), onCap);
        float edge = 0.0;
        if (u_clipOn == 1 && onCap > 0.5) {
                                                                             
                                                                         
            float e = abs(sdf(p));
            edge = 1.0 - smoothstep(0.0, 2.2 * max(u_eps, 3.0e-4 * u_scale), e);
        }
        if (u_baseColourOn == 1) {
                                                                              
                                                                           
            base = pow(clamp(u_baseColour, 0.0, 1.0), vec3(2.2));
        }
        if (u_colorOn == 1) {
            vec3 uvw;
            if (u_fieldRegistered == 1) {
                vec3 idx = u_fieldWorldToIndex * p + u_fieldIndexOffset;
                idx = clamp(idx, vec3(0.0), max(u_fieldIndexDim - 1.0, vec3(0.0)));
                                                                              
                                                                            
                uvw = (vec3(idx.z, idx.y, idx.x) + 0.5)
                    / vec3(u_fieldIndexDim.z, u_fieldIndexDim.y, u_fieldIndexDim.x);
            } else {
                vec3 q = clamp((p - u_fieldLo) / max(u_fieldHi - u_fieldLo,
                                                     vec3(1e-9)), 0.0, 1.0);
                uvw = (q * (u_fieldDim - 1.0) + 0.5) / u_fieldDim;
            }
            float s = texture(u_field, uvw).r;
            float g = (s - u_fieldRange.x)
                    / max(u_fieldRange.y - u_fieldRange.x, 1e-9);
                                                                            
                                                                            
                                                                          
                                                                          
                                                     
            vec3 fieldColour = u_fieldPalette == 1 ? divergingRamp(g)
                                                   : sequentialRamp(g);
                                                                             
                                                                             
                                                                 
            vec3 low = pow(clamp(u_fieldColorLow, 0.0, 1.0), vec3(2.2));
            vec3 high = pow(clamp(u_fieldColorHigh, 0.0, 1.0), vec3(2.2));
            if (u_fieldPalette == 2) fieldColour = mix(low, high, clamp(g, 0.0, 1.0));
            if (u_fieldPalette == 3) fieldColour = s < u_fieldThreshold ? low : high;
            base = mix(base, fieldColour, 0.96 - 0.40 * onCap);
        }
        if (sectionFill) {
            base = pow(clamp(u_sectionFill, 0.0, 1.0), vec3(2.2));
            edge = 0.0;
        }
        onCap = max(onCap, onSection);

                                                                            
                                                                          
                                                                              
                                                                               
                                                                            
                                                                              
        vec3 L = normalize(mix(u_lightDir, -rd, onCap * 0.85));
        float sh = (u_shadows == 1 && onCap < 0.5)
                 ? mix(0.22, 1.0, shadow(p + n * 2.0 * u_eps, L)) : 1.0;
        float ao = (u_ao == 1) ? ambient(p, n) : 1.0;

                                                                             
                                                                            
                                                                             
                                                                                
        float key  = max(dot(n, L), 0.0);
        float wrap = max((dot(n, L) + 0.45) / 1.45, 0.0);                        
        vec3  F    = normalize(vec3(-0.45, -0.30, 0.55));
        float fill = max(dot(n, F), 0.0);
        float rim  = pow(1.0 - max(dot(n, -rd), 0.0), 4.5);

                                                                              
                                                                            
                                                              
        float sky = 0.5 + 0.5 * n.z;
        vec3 amb = mix(vec3(${PALETTE.bgBot.join(", ")}),
                       vec3(${PALETTE.bgTop.join(", ")}), sky) * (0.26 * ao);
        vec3 lit = base * (amb
                         + vec3(${PALETTE.key.join(", ")})
                           * (0.90 * mix(key, wrap, 0.12) * sh)
                         + vec3(${PALETTE.fill.join(", ")})
                           * (0.22 * fill * mix(0.45, 1.0, ao)));
        vec3 H = normalize(L - rd);
        float spec = pow(max(dot(n, H), 0.0), 48.0) * 0.11 * sh * ao;
        lit += vec3(${PALETTE.key.join(", ")}) * spec * (1.0 - 0.75 * onCap);
                                                                         
                                                                         
        lit += vec3(${PALETTE.accent.join(", ")}) * rim * 0.17 * (1.0 - onSection);
        lit = mix(lit, vec3(${PALETTE.accent.join(", ")}) * 1.25, edge * 0.50);

                                                                           
                                                                            
        float fog = clamp((t - cam0Depth(t)) / (3.0 * u_scale), 0.0, 0.22);
                                                                        
                                                            
        fog *= 1.0 - onSection;
        col = mix(lit, background(v_uv), fog);
    }
    vec3 srgb = pow(clamp(col, 0.0, 1.0), vec3(1.0 / 2.2));
    srgb += (dither(gl_FragCoord.xy) - 0.5) / 255.0;
    o_color = vec4(srgb, 1.0);
}
`;

 
 
 
 
const BLIT_FRAG = `#version 300 es
precision highp float;
uniform sampler2D u_tex;
uniform int u_ss;
in vec2 v_uv;
out vec4 o;
void main(){
    if (u_ss <= 1) { o = texture(u_tex, v_uv); return; }
    ivec2 base = ivec2(gl_FragCoord.xy) * u_ss;
    vec3 sum = vec3(0.0);
    for (int j = 0; j < 3; j++) {
        if (j >= u_ss) break;
        for (int i = 0; i < 3; i++) {
            if (i >= u_ss) break;
            sum += pow(texelFetch(u_tex, base + ivec2(i, j), 0).rgb, vec3(2.2));
        }
    }
    o = vec4(pow(sum / float(u_ss * u_ss), vec3(1.0 / 2.2)), 1.0);
}`;


const REDUCE_FRAG = `#version 300 es
precision highp float;
uniform sampler2D u_tex;
uniform vec2 u_srcRes;
in vec2 v_uv;
layout(location = 0) out vec2 o;
void main(){
    ivec2 cell = ivec2(gl_FragCoord.xy);
    vec2 base = (vec2(cell) / 32.0) * u_srcRes;
    vec2 span = u_srcRes / 32.0;
    float s = 0.0, m = 0.0;
    for (int j = 0; j < 8; j++) {
        for (int i = 0; i < 8; i++) {
            ivec2 c = ivec2(base + span * (vec2(float(i), float(j)) + 0.5) / 8.0);
            c = clamp(c, ivec2(0), ivec2(u_srcRes) - 1);
            float v = texelFetch(u_tex, c, 0).r;
            s += v; m = max(m, v);
        }
    }
    o = vec2(s / 64.0, m);
}`;


const LEVELS = [
  { scale: 0.42, steps: 56,  shadows: 0, ao: 0 },
  { scale: 0.70, steps: 110, shadows: 0, ao: 1 },
  { scale: 1.00, steps: 220, shadows: 1, ao: 1 },
];

function reducedMotion() {
  try {
    return typeof matchMedia === "function"
      && matchMedia("(prefers-reduced-motion: reduce)").matches;
  } catch (e) { return false; }
}

function fallbackNotice(canvas, message) {


  const host = canvas.parentNode;
  if (host) {
    const d = document.createElement("div");
    d.className = "raymarch-fallback";
    d.setAttribute("role", "status");
    d.style.cssText = "position:absolute;inset:0;display:flex;align-items:center;"
      + "justify-content:center;padding:24px;text-align:center;"
      + "background:#eef3f9;color:#5a6675;font:13px/1.5 Arial,Helvetica,sans-serif";
    d.innerHTML = '<div style="max-width:34em"><div style="font-weight:700;'
      + 'color:#10151c;margin-bottom:6px">The traced viewport needs WebGL2</div>'
      + '<div>' + message + '</div><div style="margin-top:8px;font-size:11px">'
      + 'The grid preview beside it does not, and is unaffected.</div></div>';
    if (getComputedStyle(host).position === "static") host.style.position = "relative";
    host.appendChild(d);
    canvas.style.display = "none";
  }
}


export function buildShaderSources(modelSource) {
  return {vertex:VERT, fragment:FRAG_HEAD + "\n" +
    (modelSource || "float sdf(vec3 p){return 1.0;}") + "\n" + FRAG_TAIL};
}

export function createViewport(canvas, opts) {
  opts = opts || {};
  const listeners = { stats: [], error: [] };
  const emitError = (msg, detail) => {
    listeners.error.forEach((f) => { try { f(msg, detail); } catch (e) {} });
  };

  const gl = canvas.getContext("webgl2", {
    antialias: false, alpha: false, depth: false, stencil: false,
    powerPreference: "high-performance", preserveDrawingBuffer: true,
  });
  if (!gl) {
    fallbackNotice(canvas, opts.fallbackMessage
      || "This browser or machine did not provide a WebGL2 context, so the "
       + "field cannot be sphere-traced here.");
    return stubViewport(listeners, emitError);
  }
  const extCBF = gl.getExtension("EXT_color_buffer_float");
  const extLIN = gl.getExtension("OES_texture_float_linear");
  if (!extCBF) {
    emitError("EXT_color_buffer_float is absent: step statistics are "
            + "unavailable and 3-D field textures may not filter.");
  }


  const st = {
    prog: null, blit: null, reduce: null,
    fbo: null, colorTex: null, stepTex: null, fboW: 0, fboH: 0,
    redFbo: null, redTex: null,
    uniforms: {},
    locs: {},
    schema: {},
    textures: {},
    colorBy: null, backgroundWhite: false, renderedFrames: 0,
    stepFactor: 1.0, traceFixed: 0, eps: 0.01, tmax: 400, scale: 100,
    groundZ: -50, centre: [0, 0, 0],
    epsGrowth: opts.epsGrowth == null ? 1.0 : +opts.epsGrowth,
    clip: { origin: [0, 0, 0], normal: [0, 0, 1], enabled: false },
    sectionFill: null,
    baseColour: null,
    level: 0, maxLevel: LEVELS.length - 1, dirty: true, running: true,
    lastInput: 0, frames: 0, fpsAt: 0, fps: 0, lastFrame: 0,
    stepsAvg: null, stepsMax: null, statsAt: 0,
    reduced: reducedMotion(), disposed: false, ready: false,
    quality: null, supersample: 1, supersampleEffective: 1,
    maxRasterSide: Math.min(gl.getParameter(gl.MAX_TEXTURE_SIZE) || 4096,
                            (gl.getParameter(gl.MAX_VIEWPORT_DIMS) || [4096, 4096])[0],
                            (gl.getParameter(gl.MAX_VIEWPORT_DIMS) || [4096, 4096])[1]),
    textureQueue: new Map(), textureFlushRaf: 0, textureDrainWaiters: [],
    textureUploadBytes: 0,
    textureUpdates: 0, textureSkippedUpdates: 0, textureAllocations: 0,
  };

  const cam = {
    target: [0, 0, 0], dist: 100, yaw: 0.85, pitch: 0.48, fov: 32,
    vYaw: 0, vPitch: 0, projection: "perspective", verticalSpan: 100,
  };


  const vbo = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, vbo);
  gl.bufferData(gl.ARRAY_BUFFER,
                new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);

  function compile(type, src) {
    const s = gl.createShader(type);
    gl.shaderSource(s, src);
    gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) {
      const log = gl.getShaderInfoLog(s) || "(no log)";
      gl.deleteShader(s);
      return { error: log };
    }
    return { shader: s };
  }

  function link(fragSrc) {
    const v = compile(gl.VERTEX_SHADER, VERT);
    if (v.error) return { error: "vertex shader: " + v.error };
    const f = compile(gl.FRAGMENT_SHADER, fragSrc);
    if (f.error) return { error: f.error, source: fragSrc };
    const p = gl.createProgram();
    gl.attachShader(p, v.shader);
    gl.attachShader(p, f.shader);
    gl.bindAttribLocation(p, 0, "a_pos");
    gl.linkProgram(p);
    gl.deleteShader(v.shader);
    gl.deleteShader(f.shader);
    if (!gl.getProgramParameter(p, gl.LINK_STATUS)) {
      return { error: "link: " + (gl.getProgramInfoLog(p) || "(no log)") };
    }
    return { program: p };
  }

  const blitR = link(BLIT_FRAG);
  if (blitR.error) emitError("blit program: " + blitR.error);
  st.blit = blitR.program || null;
  const redR = link(REDUCE_FRAG);
  st.reduce = redR.program || null;

  function drawQuad() {
    gl.bindBuffer(gl.ARRAY_BUFFER, vbo);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  }


  function ensureFbo(w, h) {
    if (st.fbo && st.fboW === w && st.fboH === h) return;
    if (st.fbo) {
      gl.deleteFramebuffer(st.fbo);
      gl.deleteTexture(st.colorTex);
      if (st.stepTex) gl.deleteTexture(st.stepTex);
    }
    st.fboW = w; st.fboH = h;
    st.colorTex = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, st.colorTex);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, w, h, 0, gl.RGBA,
                  gl.UNSIGNED_BYTE, null);
    st.fbo = gl.createFramebuffer();
    gl.bindFramebuffer(gl.FRAMEBUFFER, st.fbo);
    gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0,
                            gl.TEXTURE_2D, st.colorTex, 0);
    if (extCBF) {
      st.stepTex = gl.createTexture();
      gl.bindTexture(gl.TEXTURE_2D, st.stepTex);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.R32F, w, h, 0, gl.RED, gl.FLOAT, null);
      gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT1,
                              gl.TEXTURE_2D, st.stepTex, 0);
      gl.drawBuffers([gl.COLOR_ATTACHMENT0, gl.COLOR_ATTACHMENT1]);
    } else {
      st.stepTex = null;
      gl.drawBuffers([gl.COLOR_ATTACHMENT0]);
    }
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
  }

  function ensureReduceTarget() {
    if (st.redFbo || !extCBF || !st.reduce) return;
    st.redTex = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, st.redTex);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RG32F, 32, 32, 0, gl.RG, gl.FLOAT, null);
    st.redFbo = gl.createFramebuffer();
    gl.bindFramebuffer(gl.FRAMEBUFFER, st.redFbo);
    gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0,
                            gl.TEXTURE_2D, st.redTex, 0);
    gl.drawBuffers([gl.COLOR_ATTACHMENT0]);
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
  }


  function basis() {
    const cp = Math.cos(cam.pitch), sp = Math.sin(cam.pitch);
    const cy = Math.cos(cam.yaw), sy = Math.sin(cam.yaw);
    const dir = [cp * cy, cp * sy, sp];
    const eye = [cam.target[0] + dir[0] * cam.dist,
                 cam.target[1] + dir[1] * cam.dist,
                 cam.target[2] + dir[2] * cam.dist];
    const fwd = [-dir[0], -dir[1], -dir[2]];
    let right = [-sy, cy, 0];
    const up = [-sp * cy, -sp * sy, cp];
    return { eye, fwd, right, up };
  }

  function markDirty(fromInput) {
    st.dirty = true;
    st.level = 0;
    if (fromInput) st.lastInput = performance.now();
  }


  function setShader(glslSource, uniformSchema) {
    if (st.disposed) return { ok: false, log: "viewport disposed" };
    const src = buildShaderSources(glslSource).fragment;
    const r = link(src);
    if (r.error) {
      emitError("the model shader did not compile", r.error);
      return { ok: false, log: r.error, source: src };
    }
    if (st.prog) gl.deleteProgram(st.prog);
    st.prog = r.program;
    st.locs = {};
    st.schema = {};
    st.uniforms = {};


    const schema = (uniformSchema && uniformSchema.uniforms) || uniformSchema || {};
    if (uniformSchema && !uniformSchema.uniforms
        && uniformSchema.scale_mm == null) {
      emitError("setShader was given a bare uniform map, so the scene "
              + "metadata (step_factor, scale_mm, eps_mm, bbox) is missing and "
              + "defaults for a 100 mm scene are in use.  Pass the whole reply "
              + "from POST /v1/implicit/shader instead -- it contains the "
              + "uniform map as .uniforms.");
    }
    for (const name in schema) {
      const rec = schema[name] || {};
      st.schema[name] = rec;
      st.uniforms[name] = rec.value == null ? 0 : rec.value;
    }
    if (uniformSchema) {
      if (uniformSchema.step_factor != null) {
        st.stepFactor = +uniformSchema.step_factor;
        st.traceFixed = 0;
      } else if (uniformSchema.trace_mode === "fixed"
                 || uniformSchema.step_factor === null) {
        st.stepFactor = 1.0;
        st.traceFixed = 1;
      }
      if (uniformSchema.trace_mode) {
        st.traceFixed = uniformSchema.trace_mode === "fixed" ? 1 : 0;
      }
      if (uniformSchema.eps_mm != null) st.eps = +uniformSchema.eps_mm;
      if (uniformSchema.tmax_mm != null) st.tmax = +uniformSchema.tmax_mm;
      if (uniformSchema.scale_mm != null) st.scale = +uniformSchema.scale_mm;
      if (uniformSchema.bbox) {
        const bb = uniformSchema.bbox;
        st.groundZ = bb[0][2] - 0.012 * st.scale;
        st.centre = [(bb[0][0] + bb[1][0]) / 2, (bb[0][1] + bb[1][1]) / 2,
                     (bb[0][2] + bb[1][2]) / 2];
      }
      st.fieldClass = uniformSchema.field_class_text
                   || (uniformSchema.field_class || {}).kind || null;
      st.stepNote = uniformSchema.step_note || null;
    }
    st.ready = true;
    markDirty(false);
    return { ok: true, log: gl.getProgramInfoLog(st.prog) || "" };
  }

  function loc(name) {
    if (!(name in st.locs)) st.locs[name] = gl.getUniformLocation(st.prog, name);
    return st.locs[name];
  }

  function setUniforms(values) {
    if (!values) return;
    for (const k in values) st.uniforms[k] = values[k];
    markDirty(false);
  }

  function applyUniforms() {
    for (const name in st.uniforms) {
      const l = loc(name);
      if (l === null) continue;
      const v = st.uniforms[name];
      const type = (st.schema[name] && st.schema[name].type) || null;
      if (Array.isArray(v)) {
        if (v.length === 2) gl.uniform2f(l, v[0], v[1]);
        else if (v.length === 3) gl.uniform3f(l, v[0], v[1], v[2]);
        else if (v.length === 4) gl.uniform4f(l, v[0], v[1], v[2], v[3]);
        else if (v.length === 9) gl.uniformMatrix3fv(l, false, v);
        else if (v.length === 16) gl.uniformMatrix4fv(l, false, v);
      } else if (type === "int" || type === "bool") {
        gl.uniform1i(l, v | 0);
      } else {
        gl.uniform1f(l, +v);
      }
    }
  }


  function invert3(m) {
    const a=m[0],b=m[1],c=m[2], d=m[3],e=m[4],f=m[5], g=m[6],h=m[7],i=m[8];
    const A=e*i-f*h, B=c*h-b*i, C=b*f-c*e;
    const D=f*g-d*i, E=a*i-c*g, F=c*d-a*f;
    const G=d*h-e*g, H=b*g-a*h, I=a*e-b*d;
    const det=a*A+b*D+c*G;
    if (!Number.isFinite(det) || Math.abs(det) < 1e-18) return null;
    const s=1/det;
    return [A*s,B*s,C*s,D*s,E*s,F*s,G*s,H*s,I*s];
  }

  function mul3(m, v) {
    return [m[0]*v[0]+m[1]*v[1]+m[2]*v[2],
            m[3]*v[0]+m[4]*v[1]+m[5]*v[2],
            m[6]*v[0]+m[7]*v[1]+m[8]*v[2]];
  }

  function registrationRecord(registration, fallbackShape) {
    if (!registration || !Array.isArray(registration.basis)
        || !Array.isArray(registration.origin)) return null;
    const shape=(registration.shape || fallbackShape || []).slice(0,3).map(Number);
    if (shape.length !== 3 || shape.some(v => !(v > 0))) return null;
    const q=registration.basis;
    if (q.length !== 3 || q.some(v => !Array.isArray(v) || v.length !== 3)) return null;

    const B=[+q[0][0],+q[1][0],+q[2][0],
             +q[0][1],+q[1][1],+q[2][1],
             +q[0][2],+q[1][2],+q[2][2]];
    const inv=invert3(B);
    if (!inv) return null;
    const origin=registration.origin.slice(0,3).map(Number);
    const io=mul3(inv, origin);
    const shift=registration.centering === "cell" ? 0.5 : 0.0;
    const indexOffset=[-io[0]-shift,-io[1]-shift,-io[2]-shift];

    const worldToIndexGL=[inv[0],inv[3],inv[6],inv[1],inv[4],inv[7],inv[2],inv[5],inv[8]];
    return {wire:registration, shape, worldToIndexGL, indexOffset};
  }

  function textureParameters(rec) {
    gl.bindTexture(gl.TEXTURE_3D, rec.tex);
    const filt = extLIN ? gl.LINEAR : gl.NEAREST;
    gl.texParameteri(gl.TEXTURE_3D, gl.TEXTURE_MIN_FILTER, filt);
    gl.texParameteri(gl.TEXTURE_3D, gl.TEXTURE_MAG_FILTER, filt);
    gl.texParameteri(gl.TEXTURE_3D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_3D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_3D, gl.TEXTURE_WRAP_R, gl.CLAMP_TO_EDGE);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
  }

  function allocateTexture(name, spec) {
    if (st.disposed || !spec) return null;
    const registered=registrationRecord(spec.registration, spec.shape);
    const logicalShape=(registered ? registered.shape : (spec.shape || [2,2,2])).map(v=>v|0);
    if (logicalShape.length !== 3 || logicalShape.some(v=>v<=0)) {
      emitError("texture " + name + " has an invalid shape", logicalShape);
      return null;
    }
    const glShape=registered
      ? [logicalShape[2],logicalShape[1],logicalShape[0]]
      : logicalShape.slice();
    const same=st.textures[name]
      && st.textures[name].glShape.join(",") === glShape.join(",")
      && Boolean(st.textures[name].registration) === Boolean(registered);
    let rec=st.textures[name];
    if (!rec) rec=st.textures[name]={tex:gl.createTexture()};
    if (!same || spec.reset) {
      textureParameters(rec);
      gl.texImage3D(gl.TEXTURE_3D,0,gl.R32F,glShape[0],glShape[1],glShape[2],0,
                    gl.RED,gl.FLOAT,null);
      rec.tileRevisions=new Map();
      st.textureAllocations++;
    }
    rec.shape=logicalShape;
    rec.glShape=glShape;
    rec.registration=registered;
    rec.bbox=spec.bbox || rec.bbox || [[0,0,0],[1,1,1]];
    rec.range=spec.range || rec.range || [0,1];
    rec.revision=spec.revision || rec.revision || null;
    return rec;
  }

  function setTexture(name, spec) {
    if (st.disposed || !spec) return;
    const rec=allocateTexture(name, spec);
    if (!rec) return;
    const data=spec.data instanceof Float32Array ? spec.data : new Float32Array(spec.data || []);
    const want=rec.shape[0]*rec.shape[1]*rec.shape[2];
    if (data.length < want) {
      emitError("texture " + name + " has " + data.length + " samples but its shape says " + want + "; it is not uploaded");
      return;
    }
    textureParameters(rec);
    gl.texSubImage3D(gl.TEXTURE_3D,0,0,0,0,rec.glShape[0],rec.glShape[1],rec.glShape[2],
                     gl.RED,gl.FLOAT,data.subarray(0,want));
    st.textureUploadBytes += want*4;
    st.textureUpdates++;
    if (!spec.range) {
      let lo=Infinity,hi=-Infinity;
      for (let j=0;j<want;j++){const v=data[j];if(v<lo)lo=v;if(v>hi)hi=v;}
      rec.range=[lo,hi];
    }
    rec.tileRevisions.set("full",spec.revision || null);
    markDirty(false);
  }

  function prepareTextureRegion(name, rec, update) {
    const offset=(update.offset || [0,0,0]).slice(0,3).map(v=>v|0);
    const shape=(update.shape || []).slice(0,3).map(v=>v|0);
    if (shape.length!==3 || shape.some(v=>v<=0) || offset.some(v=>v<0)
        || shape.some((v,k)=>offset[k]+v>rec.shape[k])) {
      emitError("texture region is outside " + name,{offset,shape,textureShape:rec.shape});
      return null;
    }
    const key=offset.join(",")+"|"+shape.join(",");
    const revision=update.revision || update.raw_sha256 || null;
    const data=update.data instanceof Float32Array ? update.data : new Float32Array(update.data || []);
    const want=shape[0]*shape[1]*shape[2];
    if (data.length < want) {
      emitError("texture region " + key + " has too few samples",{got:data.length,want});
      return null;
    }
    const glOffset=rec.registration ? [offset[2],offset[1],offset[0]] : offset;
    const glShape=rec.registration ? [shape[2],shape[1],shape[0]] : shape;
    return { update, offset, shape, key, revision, data, want, glOffset, glShape };
  }

  function uploadPreparedTextureRegion(rec, prepared) {
    if (prepared.revision && rec.tileRevisions.get(prepared.key)===prepared.revision) {
      st.textureSkippedUpdates++;
      return false;
    }
    textureParameters(rec);
    const {update,revision,key,data,want,glOffset,glShape}=prepared;
    gl.texSubImage3D(gl.TEXTURE_3D,0,glOffset[0],glOffset[1],glOffset[2],
                     glShape[0],glShape[1],glShape[2],gl.RED,gl.FLOAT,data.subarray(0,want));
    if (revision) rec.tileRevisions.set(key,revision);
    if (update.range) rec.range=[+update.range[0],+update.range[1]];
    st.textureUploadBytes += want*4;
    st.textureUpdates++;
    markDirty(false);
    return true;
  }

  function updateTextureRegion(name, update) {
    if (st.disposed || !update) return false;
    const rec=st.textures[name] || allocateTexture(name, update);
    if (!rec) return false;
    const prepared=prepareTextureRegion(name,rec,update);
    return prepared ? uploadPreparedTextureRegion(rec,prepared) : false;
  }

  function applyTextureRegionsAtomic(name, updates) {


    if (st.disposed || !Array.isArray(updates) || !updates.length) return 0;
    const rec=st.textures[name];
    if (!rec) {
      emitError("atomic texture update requires an allocated texture",{name});
      return 0;
    }
    const prepared=[];
    for (const update of updates) {
      const item=prepareTextureRegion(name,rec,update);
      if (!item) return 0;
      prepared.push(item);
    }
    let applied=0;
    for (const item of prepared) if (uploadPreparedTextureRegion(rec,item)) applied++;
    return applied;
  }

  function flushTextureUpdates(budgetMs) {
    if (st.disposed) {
      resolveTextureDrainWaiters();
      return 0;
    }
    if (st.textureFlushRaf) { cancelAnimationFrame(st.textureFlushRaf); st.textureFlushRaf=0; }
    const started=performance.now();
    const budget=Math.max(0.5,+budgetMs || 3.0);
    let count=0;
    const items=[...st.textureQueue.values()].sort((a,b)=>(a.priority||0)-(b.priority||0));
    for (const item of items) {
      st.textureQueue.delete(item.key);
      updateTextureRegion(item.name,item.update);
      count++;
      if (performance.now()-started>=budget) break;
    }
    if (st.textureQueue.size) {
      st.textureFlushRaf=requestAnimationFrame(()=>flushTextureUpdates(budget));
    } else {
      resolveTextureDrainWaiters();
    }
    return count;
  }

  function resolveTextureDrainWaiters() {
    if (st.textureQueue.size || !st.textureDrainWaiters.length) return;
    const waiters=st.textureDrainWaiters.splice(0);
    for (const resolve of waiters) {
      try { resolve(true); } catch (_) {                                                     }
    }
  }

  function drainTextureUpdates(budgetMs) {


    if (st.disposed || !st.textureQueue.size) return Promise.resolve(true);
    return new Promise(resolve => {
      st.textureDrainWaiters.push(resolve);
      if (!st.textureFlushRaf) {
        st.textureFlushRaf=requestAnimationFrame(()=>flushTextureUpdates(budgetMs));
      }
    });
  }

  function queueTextureRegion(name, update) {
    if (!update) return;
    const offset=(update.offset || [0,0,0]).slice(0,3);
    const shape=(update.shape || []).slice(0,3);
    const key=name+"|"+offset.join(",")+"|"+shape.join(",");
    st.textureQueue.set(key,{key,name,update,priority:+update.priority || 0});
    if (!st.textureFlushRaf) st.textureFlushRaf=requestAnimationFrame(()=>flushTextureUpdates(update.budgetMs));
  }

  function clearTextureUpdates(name) {
    let removed=0;
    for (const [key,item] of st.textureQueue) {
      if (name == null || item.name===name) {
        st.textureQueue.delete(key);
        removed++;
      }
    }
    if (!st.textureQueue.size && st.textureFlushRaf) {
      cancelAnimationFrame(st.textureFlushRaf);
      st.textureFlushRaf=0;
    }
    resolveTextureDrainWaiters();
    return removed;
  }

  function deleteTexture(name) {
    const rec=st.textures[name];
    if (!rec) return false;
    gl.deleteTexture(rec.tex);
    delete st.textures[name];
    if (st.colorBy===name) st.colorBy=null;
    markDirty(false);
    return true;
  }

  function bindTextures() {


    let unit = 1;
    for (const name in st.textures) {
      const rec = st.textures[name];
      const l = loc(name);
      if (l === null) continue;
      gl.activeTexture(gl.TEXTURE0 + unit);
      gl.bindTexture(gl.TEXTURE_3D, rec.tex);
      gl.uniform1i(l, unit);
      const b = rec.bbox;
      const llo = loc(name + "_lo"), lhi = loc(name + "_hi"),
            ldm = loc(name + "_dim");
      if (llo) gl.uniform3f(llo, b[0][0], b[0][1], b[0][2]);
      if (lhi) gl.uniform3f(lhi, b[1][0], b[1][1], b[1][2]);
      if (ldm) gl.uniform3f(ldm, rec.shape[0], rec.shape[1], rec.shape[2]);
      unit++;
    }
    const cb = st.colorBy && st.textures[st.colorBy];
    gl.activeTexture(gl.TEXTURE0);
    if (cb) {
      gl.bindTexture(gl.TEXTURE_3D, cb.tex);
      gl.uniform1i(loc("u_field"), 0);
      gl.uniform3f(loc("u_fieldLo"), cb.bbox[0][0], cb.bbox[0][1], cb.bbox[0][2]);
      gl.uniform3f(loc("u_fieldHi"), cb.bbox[1][0], cb.bbox[1][1], cb.bbox[1][2]);
      gl.uniform3f(loc("u_fieldDim"), cb.shape[0], cb.shape[1], cb.shape[2]);
      gl.uniform2f(loc("u_fieldRange"), cb.range[0], cb.range[1]);
      gl.uniform1i(loc("u_fieldPalette"), ({sequential:0,diverging:1,two_color:2,two_category:3})[cb.palette] || 0);
      const colours=cb.colorsRgb || [[0,0,0],[1,1,1]];
      gl.uniform3fv(loc("u_fieldColorLow"), colours[0]);
      gl.uniform3fv(loc("u_fieldColorHigh"), colours[1]);
      gl.uniform1f(loc("u_fieldThreshold"), cb.threshold == null ? 0.5 : cb.threshold);
      if (cb.registration) {
        gl.uniform1i(loc("u_fieldRegistered"), 1);
        gl.uniformMatrix3fv(loc("u_fieldWorldToIndex"), false, cb.registration.worldToIndexGL);
        gl.uniform3f(loc("u_fieldIndexOffset"), cb.registration.indexOffset[0],
                     cb.registration.indexOffset[1], cb.registration.indexOffset[2]);
        gl.uniform3f(loc("u_fieldIndexDim"), cb.shape[0], cb.shape[1], cb.shape[2]);
      } else {
        gl.uniform1i(loc("u_fieldRegistered"), 0);
      }
      gl.uniform1i(loc("u_colorOn"), 1);
    } else {
      gl.uniform1i(loc("u_colorOn"), 0);
      gl.uniform1i(loc("u_fieldRegistered"), 0);
      gl.uniform1i(loc("u_fieldPalette"), 0);
    }
  }


  let lastRenderedView = "";
  function render() {
    if (!st.prog || st.disposed) return;
    const dpr = Math.min(window.devicePixelRatio || 1, opts.maxDpr || 2);
    const cssW = canvas.clientWidth || canvas.width || 640;
    const cssH = canvas.clientHeight || canvas.height || 480;
    if (canvas.width !== Math.round(cssW * dpr)
        || canvas.height !== Math.round(cssH * dpr)) {
      canvas.width = Math.round(cssW * dpr);
      canvas.height = Math.round(cssH * dpr);
      markDirty(false);
    }
    const lv = LEVELS[Math.min(st.level, st.maxLevel)];
     
     
    let ss = (st.level >= st.maxLevel && lv.scale === 1.0) ? st.supersample : 1;
    while (ss > 1 && Math.max(canvas.width, canvas.height) * ss > st.maxRasterSide) ss--;
    st.supersampleEffective = ss;
    const w = ss > 1 ? canvas.width * ss : Math.max(16, Math.round(canvas.width * lv.scale));
    const h = ss > 1 ? canvas.height * ss : Math.max(16, Math.round(canvas.height * lv.scale));
    ensureFbo(w, h);

    gl.useProgram(st.prog);
    const b = basis();
    gl.uniform3f(loc("u_eye"), b.eye[0], b.eye[1], b.eye[2]);
    gl.uniform3f(loc("u_fwd"), b.fwd[0], b.fwd[1], b.fwd[2]);
    gl.uniform3f(loc("u_right"), b.right[0], b.right[1], b.right[2]);
    gl.uniform3f(loc("u_up"), b.up[0], b.up[1], b.up[2]);
    gl.uniform2f(loc("u_res"), w, h);
    gl.uniform1f(loc("u_tanHalfFov"), Math.tan(cam.fov * Math.PI / 360));
    gl.uniform1i(loc("u_orthographic"), cam.projection === "orthographic" ? 1 : 0);
    gl.uniform1f(loc("u_orthoSpan"), cam.verticalSpan);
    gl.uniform1i(loc("u_backgroundWhite"), st.backgroundWhite ? 1 : 0);
    st.renderedFrames++;
    gl.uniform1f(loc("u_stepFactor"), st.stepFactor);


    const far = cam.dist + 2.0 * st.scale;
    const near = Math.max(0.0, cam.dist - 1.05 * st.scale);


    const budget = Math.min(5.0, 1.0 / Math.max(st.stepFactor, 0.05));
    const nsteps = Math.min(1000, Math.round(
        (st.quality ? st.quality.steps : lv.steps)
        * (st.traceFixed ? 1.0 : budget)));
    gl.uniform1f(loc("u_t0"), st.traceFixed ? near : 0.0);
    gl.uniform1f(loc("u_fixedStep"),
                 Math.max((far - near) / Math.max(nsteps - 2, 8), st.eps * 2.0));
    gl.uniform1i(loc("u_traceFixed"), st.traceFixed);
    gl.uniform1f(loc("u_eps"), st.eps);
    gl.uniform1f(loc("u_epsGrowth"), st.epsGrowth);
    gl.uniform1f(loc("u_tmax"), far);
    gl.uniform1i(loc("u_maxSteps"), nsteps);
    gl.uniform1f(loc("u_scale"), st.scale);
    gl.uniform3f(loc("u_clipO"), st.clip.origin[0], st.clip.origin[1],
                 st.clip.origin[2]);
    const n = st.clip.normal;
    const nl = Math.hypot(n[0], n[1], n[2]) || 1;
    gl.uniform3f(loc("u_clipN"), n[0] / nl, n[1] / nl, n[2] / nl);
    gl.uniform1i(loc("u_clipOn"), st.clip.enabled ? 1 : 0);
    const sf = st.sectionFill;
    gl.uniform1i(loc("u_sectionFillOn"), sf ? 1 : 0);
    if (sf) {
      gl.uniform3f(loc("u_sectionN"), sf.normal[0], sf.normal[1], sf.normal[2]);
      gl.uniform1f(loc("u_sectionD"), sf.offset);
      gl.uniform3f(loc("u_sectionLo"), sf.lo[0], sf.lo[1], sf.lo[2]);
      gl.uniform3f(loc("u_sectionHi"), sf.hi[0], sf.hi[1], sf.hi[2]);
      gl.uniform3f(loc("u_sectionFill"), sf.color[0], sf.color[1], sf.color[2]);
    }
    gl.uniform1i(loc("u_baseColourOn"), st.baseColour ? 1 : 0);
    if (st.baseColour)
      gl.uniform3f(loc("u_baseColour"), st.baseColour[0], st.baseColour[1], st.baseColour[2]);
    gl.uniform1i(loc("u_shadows"), lv.shadows);
    gl.uniform1i(loc("u_ao"), lv.ao);


    let L = opts.lightDir;
    if (!L) {
      L = [0, 0, 0];
      for (let i = 0; i < 3; i++) {
        L[i] = -0.30 * b.fwd[i] - 0.66 * b.right[i] + 0.60 * b.up[i];
      }
      L[2] += 0.18;
    }
    const Ln = Math.hypot(L[0], L[1], L[2]) || 1;
    gl.uniform3f(loc("u_lightDir"), L[0] / Ln, L[1] / Ln, L[2] / Ln);
    gl.uniform1i(loc("u_groundOn"), opts.ground === false ? 0 : 1);
    gl.uniform1f(loc("u_groundZ"), st.groundZ);
    gl.uniform3f(loc("u_centre"), st.centre[0], st.centre[1], st.centre[2]);
    applyUniforms();
    bindTextures();

    gl.bindFramebuffer(gl.FRAMEBUFFER, st.fbo);
    gl.viewport(0, 0, w, h);
    drawQuad();

    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.viewport(0, 0, canvas.width, canvas.height);
    gl.useProgram(st.blit);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, st.colorTex);
    gl.uniform1i(gl.getUniformLocation(st.blit, "u_tex"), 0);
    gl.uniform1i(gl.getUniformLocation(st.blit, "u_ss"), ss);
    drawQuad();

    st.frames++;
    st.dirty = false;
    const viewSignature=JSON.stringify([b.eye,b.fwd,b.right,b.up,cam.fov,cssW,cssH]);
    if(viewSignature!==lastRenderedView){
      lastRenderedView=viewSignature;
      canvas.dispatchEvent(new CustomEvent("implexity:viewport-view-changed",{bubbles:true}));
    }
  }

  function collectStats(now) {
    if (!st.stepTex || !st.reduce) return;
    ensureReduceTarget();
    if (!st.redFbo) return;
    gl.useProgram(st.reduce);
    gl.bindFramebuffer(gl.FRAMEBUFFER, st.redFbo);
    gl.viewport(0, 0, 32, 32);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, st.stepTex);
    gl.uniform1i(gl.getUniformLocation(st.reduce, "u_tex"), 0);
    gl.uniform2f(gl.getUniformLocation(st.reduce, "u_srcRes"), st.fboW, st.fboH);
    drawQuad();
    const px = new Float32Array(32 * 32 * 2);
    gl.readPixels(0, 0, 32, 32, gl.RG, gl.FLOAT, px);
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    let s = 0, m = 0;
    for (let i = 0; i < 32 * 32; i++) { s += px[i * 2]; m = Math.max(m, px[i * 2 + 1]); }
    st.stepsAvg = s / (32 * 32);
    st.stepsMax = m;
  }

  function emitStats(now) {
    const dt = now - st.fpsAt;


    if (dt > 0 && st.frames > 0) st.fps = (st.frames * 1000) / dt;
    st.idle = st.frames === 0;
    st.frames = 0;
    st.fpsAt = now;
    const payload = {
      fps: Math.round(st.fps * 10) / 10,
      steps_avg: st.stepsAvg == null ? null : Math.round(st.stepsAvg * 10) / 10,
      steps_max: st.stepsMax == null ? null : Math.round(st.stepsMax),
      refined: st.level >= st.maxLevel,
      idle: !!st.idle,
      level: st.level,
      render_scale: LEVELS[Math.min(st.level, st.maxLevel)].scale,
      supersample: st.supersampleEffective,
      trace_mode: st.traceFixed ? "fixed" : "sphere",
      step_factor: st.stepFactor,
      field_class: st.fieldClass || null,
      step_note: st.stepNote || null,
    };
    listeners.stats.forEach((f) => { try { f(payload); } catch (e) {} });
  }

  let raf = 0;
  function loop() {
    if (st.disposed) return;
    raf = requestAnimationFrame(loop);
    const now = performance.now();
    const dt = Math.min((now - (st.lastFrame || now)) / 1000, 0.25);
    st.lastFrame = now;


    if (!st.reduced && (Math.abs(cam.vYaw) > 1e-5 || Math.abs(cam.vPitch) > 1e-5)) {
      const k = Math.exp(-dt / 0.11);
      const f = dt * 60.0;
      cam.yaw += cam.vYaw * f;
      cam.pitch = Math.max(-1.5, Math.min(1.5, cam.pitch + cam.vPitch * f));
      cam.vYaw *= k; cam.vPitch *= k;
      if (Math.abs(cam.vYaw) < 1e-4) cam.vYaw = 0;
      if (Math.abs(cam.vPitch) < 1e-4) cam.vPitch = 0;
      markDirty(true);
    }
    const settled = now - st.lastInput > (st.reduced ? 0 : 90);
    if (!st.dirty && settled && st.level < st.maxLevel) {
      st.level = st.reduced ? st.maxLevel : st.level + 1;
      st.dirty = true;
    }
    if (st.dirty) render();
    if (now - st.statsAt > 500) {
      st.statsAt = now;
      collectStats(now);
      emitStats(now);
    }
  }
  if (opts.autoStart !== false) raf = requestAnimationFrame(loop);


  let drag = null;
  const onDown = (e) => {
    if (e.button !== 0 && e.button !== 1) return;
    drag = { x: e.clientX, y: e.clientY,
             pan: e.button === 1 || e.shiftKey || e.ctrlKey };
    cam.vYaw = cam.vPitch = 0;
    try { canvas.setPointerCapture(e.pointerId); } catch (err) {}
    e.preventDefault();
  };
  const onMove = (e) => {
    if (!drag) return;
    const dx = e.clientX - drag.x, dy = e.clientY - drag.y;
    drag.x = e.clientX; drag.y = e.clientY;
    if (drag.pan) api.camera.pan(dx, dy);
    else api.camera.orbit(dx, dy);
    e.preventDefault();
  };
  const onUp = (e) => {
    drag = null;
    try { canvas.releasePointerCapture(e.pointerId); } catch (err) {}
  };
  const onWheel = (e) => {
    api.camera.zoom(Math.exp((e.deltaY > 0 ? 1 : -1) * 0.12));
    e.preventDefault();
  };
  let pinch = 0;
  const onTouch = (e) => {
    if (e.touches.length === 2) {
      const d = Math.hypot(e.touches[0].clientX - e.touches[1].clientX,
                           e.touches[0].clientY - e.touches[1].clientY);
      if (pinch) api.camera.zoom(pinch / d);
      pinch = d;
      e.preventDefault();
    } else pinch = 0;
  };
  if (opts.interactive !== false) {
    canvas.addEventListener("pointerdown", onDown);
    canvas.addEventListener("pointermove", onMove);
    canvas.addEventListener("pointerup", onUp);
    canvas.addEventListener("pointercancel", onUp);
    canvas.addEventListener("wheel", onWheel, { passive: false });
    canvas.addEventListener("touchmove", onTouch, { passive: false });
    canvas.style.touchAction = "none";
  }


  const api = {
    setShader,
    setUniforms,
    setTexture,
    allocateTexture,
    updateTextureRegion,
    applyTextureRegionsAtomic,
    queueTextureRegion,
    flushTextureUpdates,
    drainTextureUpdates,
    clearTextureUpdates,
    deleteTexture,
     
     
     
     
    setSectionFill(spec) {
      if (!spec) { st.sectionFill = null; markDirty(false); return; }
      const vec = (v, label) => {
        if (!Array.isArray(v) || v.length !== 3 || !v.every(Number.isFinite))
          throw new Error("section fill " + label + " requires three finite numbers");
        return v.slice(0, 3);
      };
      const normal = vec(spec.normal, "normal");
      const length = Math.hypot(normal[0], normal[1], normal[2]);
      if (!(length > 1e-12)) throw new Error("section fill normal must be nonzero");
      const offset = +spec.offset;
      if (!Number.isFinite(offset)) throw new Error("section fill offset must be finite");
      const lo = vec(spec.lo, "lo"), hi = vec(spec.hi, "hi");
      if (lo.some((x, i) => !(hi[i] > x))) throw new Error("section fill box must have positive extents");
      const color = vec(spec.color, "color");
      if (color.some((x) => x < 0 || x > 1)) throw new Error("section fill color must lie in 0..1");
      st.sectionFill = { normal: normal.map((x) => x / length), offset: offset / length,
                         lo, hi, color };
      markDirty(false);
    },
     
     
    setBaseColour(rgb) {
      if (rgb == null) { st.baseColour = null; markDirty(false); return; }
      if (!Array.isArray(rgb) || rgb.length !== 3 || !rgb.every((x) => Number.isFinite(x) && x >= 0 && x <= 1))
        throw new Error("base colour requires three numbers in 0..1");
      st.baseColour = rgb.slice(0, 3);
      markDirty(false);
    },
    setClip(c) {
      if (!c) return;
      if (c.origin) st.clip.origin = c.origin.slice(0, 3);
      if (c.normal) st.clip.normal = c.normal.slice(0, 3);
      if (c.enabled != null) st.clip.enabled = !!c.enabled;
      markDirty(false);
    },


    setColorBy(name, range, metadata = {}) {
      st.colorBy = name || null;
      if (name && st.textures[name]) {
        if (range) st.textures[name].range = [range[0], range[1]];
        const options=metadata&&typeof metadata==="object"?metadata:{};
        const palettes=["sequential","diverging","two_color","two_category"];
        st.textures[name].palette = options.signed ? "diverging" :
          (palettes.includes(options.palette) ? options.palette : "sequential");
        if (options.colors_rgb) st.textures[name].colorsRgb=options.colors_rgb.map(row=>row.slice());
        st.textures[name].threshold=options.threshold;
      }
      markDirty(false);
    },
    setBackground(mode) {
      if (!["white","light"].includes(mode)) throw new Error("invalid native background");
      st.backgroundWhite=mode === "white"; markDirty(false);
    },
    camera: {
      orbit(dx, dy) {
        const k = 0.006;
        cam.yaw -= dx * k;
        cam.pitch = Math.max(-1.5, Math.min(1.5, cam.pitch + dy * k));
        if (!st.reduced) { cam.vYaw = -dx * k * 0.25; cam.vPitch = dy * k * 0.25; }
        markDirty(true);
      },
      pan(dx, dy) {
        const b = basis();
        const s = (cam.projection === "orthographic" ? cam.verticalSpan
                  : cam.dist * Math.tan(cam.fov * Math.PI / 360) * 2)
                / Math.max(canvas.clientHeight || 1, 1);
        for (let i = 0; i < 3; i++) {
          cam.target[i] += (-b.right[i] * dx + b.up[i] * dy) * s;
        }
        markDirty(true);
      },
      zoom(f) {
        cam.dist = Math.max(1e-3, Math.min(1e6, cam.dist * f));
        cam.verticalSpan = Math.max(1e-3, Math.min(1e6, cam.verticalSpan * f));
        markDirty(true);
      },
      frame(bbox) {
        if (!bbox) return;
        const lo = bbox[0], hi = bbox[1];
        cam.target = [(lo[0] + hi[0]) / 2, (lo[1] + hi[1]) / 2,
                      (lo[2] + hi[2]) / 2];


        const r = 0.5 * Math.hypot(hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]);
        const half = cam.fov * Math.PI / 360;
        const aspect = Math.max((canvas.clientWidth || 1)
                              / (canvas.clientHeight || 1), 0.2);
        const eff = aspect < 1 ? Math.atan(Math.tan(half) * aspect) : half;
        cam.dist = Math.max(1.15 * r / Math.sin(eff), 1e-3);
        cam.verticalSpan = Math.max(1.15 * 2.0 * r / Math.min(aspect,1.0),1e-3);
        markDirty(true);
      },
      frustum() {
        const b=basis();
        return {eye:b.eye.slice(),fwd:b.fwd.slice(),right:b.right.slice(),up:b.up.slice(),
                target:cam.target.slice(),fov:cam.fov,
                aspect:Math.max((canvas.clientWidth||1)/(canvas.clientHeight||1),1e-6),
                width:canvas.clientWidth||canvas.width||1,height:canvas.clientHeight||canvas.height||1};
      },
      get() {
        return { target: cam.target.slice(), dist: cam.dist, yaw: cam.yaw,
                 pitch: cam.pitch, fov: cam.fov, eye: basis().eye,
                 projection:cam.projection, vertical_span_mm:cam.verticalSpan };
      },
      set(c) {
        if (!c) return;
        if (c.target) cam.target = c.target.slice(0, 3);
        if (c.dist != null) cam.dist = +c.dist;
        if (c.yaw != null) cam.yaw = +c.yaw;
        if (c.pitch != null) cam.pitch = Math.max(-1.5, Math.min(1.5, +c.pitch));
        if (c.fov != null) cam.fov = +c.fov;
        if (c.projection != null) {
          if (!["perspective","orthographic"].includes(c.projection)) throw new Error("invalid camera projection");
          cam.projection=c.projection;
        }
        if (c.vertical_span_mm != null) {
          if (!Number.isFinite(c.vertical_span_mm) || c.vertical_span_mm<=0) throw new Error("invalid orthographic span");
          cam.verticalSpan=c.vertical_span_mm;
        }
        cam.vYaw = cam.vPitch = 0;
        markDirty(false);
      },
    },
    onStats(cb) { if (cb) listeners.stats.push(cb); return api; },
    onError(cb) { if (cb) listeners.error.push(cb); return api; },

    setQuality(n) {
      if (n == null) { st.quality = null; st.maxLevel = LEVELS.length - 1; }
      else {
        const i = Math.max(0, Math.min(LEVELS.length - 1, n | 0));
        st.quality = LEVELS[i];
        st.level = i; st.maxLevel = i;
      }
      markDirty(false);
    },
    setSupersample(k) {
      const n = Number(k);
      if (!Number.isInteger(n) || n < 1 || n > 3) throw new Error("supersample must be 1, 2 or 3");
      st.supersample = n;
      markDirty(false);
    },
    redraw() { markDirty(false); if (!raf) render(); return api; },
    stats() {
      return { fps: st.fps, steps_avg: st.stepsAvg, steps_max: st.stepsMax,
               refined: st.level >= st.maxLevel, level: st.level,
               supersample: st.supersampleEffective,
               supersample_requested: st.supersample,
               raster_px: [st.fboW, st.fboH],
               canvas_px: [canvas.width, canvas.height],
               trace_mode: st.traceFixed ? "fixed" : "sphere",
               step_factor: st.stepFactor, ready: st.ready,
               reduced_motion: st.reduced,
               texture_upload_bytes: st.textureUploadBytes,
               texture_updates: st.textureUpdates,
               texture_skipped_updates: st.textureSkippedUpdates,
               texture_allocations: st.textureAllocations,
               queued_texture_updates: st.textureQueue.size,
               texture_drain_waiters: st.textureDrainWaiters.length,
               rendered_frames:st.renderedFrames,
               gl: { color_buffer_float: !!extCBF, texture_float_linear: !!extLIN } };
    },
    dispose() {
      if (st.disposed) return;
      st.disposed = true;
      if (raf) cancelAnimationFrame(raf);
      if (st.textureFlushRaf) cancelAnimationFrame(st.textureFlushRaf);
      st.textureFlushRaf = 0;
      st.textureQueue.clear();
      resolveTextureDrainWaiters();
      raf = 0;
      if (opts.interactive !== false) {
        canvas.removeEventListener("pointerdown", onDown);
        canvas.removeEventListener("pointermove", onMove);
        canvas.removeEventListener("pointerup", onUp);
        canvas.removeEventListener("pointercancel", onUp);
        canvas.removeEventListener("wheel", onWheel);
        canvas.removeEventListener("touchmove", onTouch);
      }
      for (const k in st.textures) gl.deleteTexture(st.textures[k].tex);
      if (st.fbo) gl.deleteFramebuffer(st.fbo);
      if (st.colorTex) gl.deleteTexture(st.colorTex);
      if (st.stepTex) gl.deleteTexture(st.stepTex);
      if (st.redFbo) gl.deleteFramebuffer(st.redFbo);
      if (st.redTex) gl.deleteTexture(st.redTex);
      if (st.prog) gl.deleteProgram(st.prog);
      if (st.blit) gl.deleteProgram(st.blit);
      if (st.reduce) gl.deleteProgram(st.reduce);
      gl.deleteBuffer(vbo);
      listeners.stats.length = 0;
      listeners.error.length = 0;
    },
  };
  return api;
}

function stubViewport(listeners, emitError) {


  const noop = () => {};
  const api = {
    setShader: () => ({ ok: false, log: "no WebGL2 context" }),
    setUniforms: noop, setTexture: noop, allocateTexture: noop,
    updateTextureRegion: () => false, applyTextureRegionsAtomic: () => 0,
    queueTextureRegion: noop, flushTextureUpdates: () => 0,
    drainTextureUpdates: () => Promise.resolve(true),
    clearTextureUpdates: () => 0, deleteTexture: () => false,
    setClip: noop, setSectionFill: noop, setColorBy: noop, setBackground:noop,
    camera: {
      orbit: noop, pan: noop, zoom: noop, frame: noop,
      get: () => ({ target: [0, 0, 0], dist: 1, yaw: 0, pitch: 0, fov: 32 }),
      frustum: () => ({eye:[0,0,1],fwd:[0,0,-1],right:[1,0,0],up:[0,1,0],target:[0,0,0],fov:32,aspect:1,width:1,height:1}),
      set: noop,
    },
    onStats(cb) {
      if (cb) cb({ fps: 0, steps_avg: null, steps_max: null, refined: false,
                   trace_mode: "unavailable" });
      return api;
    },
    onError(cb) { if (cb) listeners.error.push(cb); return api; },
    setQuality: noop, setSupersample: noop, redraw: noop,
    stats: () => ({ fps: 0, steps_avg: null, steps_max: null, refined: false,
                    ready: false, trace_mode: "unavailable" }),
    dispose: noop,
  };
  return api;
}

export default createViewport;
