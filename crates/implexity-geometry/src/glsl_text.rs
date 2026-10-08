// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
