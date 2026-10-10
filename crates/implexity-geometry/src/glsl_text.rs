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


pub const PRELUDE: &str = r" 
 
 
 
 

const float IMPLEXITY_SAFE_EPS = 1e-24;    
const float IMPLEXITY_PI       = 3.14159265358979324;
const float IMPLEXITY_ROOT_HALF = 0.707106781186547524;    

 
 
float vlen3(vec3 v) { return sqrt(dot(v, v) + IMPLEXITY_SAFE_EPS); }

 
float vlen2(float a, float b) { return sqrt(a * a + b * b + IMPLEXITY_SAFE_EPS); }

 
 
 
 
 
 
 
 
 
 
 
 
 
 
 
 
 
 
 
 
float atan2s(float y, float x) {
    float ax = abs(x), ay = abs(y);
    if (ax == 0.0 && ay == 0.0) return 0.0;
    float r = (ax >= ay) ? atan(ay / ax)
                         : (1.57079632679489662 - atan(ax / ay));
    if (x < 0.0) r = 3.14159265358979324 - r;
    return (y < 0.0) ? -r : r;
}

 
 
float log1p_(float x) {
    float u = 1.0 + x;
    return (u == 1.0) ? x : x * log(u) / (u - 1.0);
}

 
float smin_poly(float a, float b, float k) {
    float h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
    return b + h * (a - b) - k * h * (1.0 - h);
}
float smax_poly(float a, float b, float k) {
    return -smin_poly(-a, -b, k);
}

 
float smin_exp(float a, float b, float k) {
    float m = min(a, b);
    return m - k * log1p_(exp(-abs(a - b) / k));
}
float smax_exp(float a, float b, float k) {
    return -smin_exp(-a, -b, k);
}

 
 
 
 
mat3 implexityRot(float rxDeg, float ryDeg, float rzDeg) {
    float d = IMPLEXITY_PI / 180.0;
    float cx = cos(rxDeg * d), sx = sin(rxDeg * d);
    float cy = cos(ryDeg * d), sy = sin(ryDeg * d);
    float cz = cos(rzDeg * d), sz = sin(rzDeg * d);
    mat3 Rx = mat3(1.0, 0.0, 0.0,   0.0,  cx,  sx,   0.0, -sx,  cx);
    mat3 Ry = mat3( cy, 0.0, -sy,   0.0, 1.0, 0.0,    sy, 0.0,  cy);
    mat3 Rz = mat3( cz,  sz, 0.0,   -sz,  cz, 0.0,   0.0, 0.0, 1.0);
    return Rz * Ry * Rx;
}
";

pub const TEXTURE_BODY: &str = r"vec3 ext = {tex}_hi - {tex}_lo;
vec3 t   = (p - {tex}_lo) / ext;
vec3 tc  = clamp(t, 0.0, 1.0);
 
 
 
vec3 uvw = (tc * ({tex}_dim - 1.0) + 0.5) / {tex}_dim;
float v = texture({tex}, uvw).r;
 
 
 
 
 
 
 
 
 
vec3 od = (tc - t) * ext;
float d2 = dot(od, od);
if (d2 <= 0.0) return v;
 
 
 
 
 
float dbox = sqrt(d2);
return max(dbox, v - dbox);";
