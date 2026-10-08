// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



#[inline]
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn f32_of(x: f64) -> f32 {
    x as f32
}

#[inline]
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn trunc_i64(x: f64) -> i64 {
    x as i64
}

#[inline]
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn trunc_usize(x: f64) -> usize {
    if x > 0.0 { x as usize } else { 0 }
}

#[inline]
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn idx(i: i64) -> usize {
    debug_assert!(i >= 0, "negative index {i}");
    i.max(0) as usize
}

#[inline]
#[must_use]
#[allow(clippy::cast_possible_wrap)]
pub fn i64_of(u: usize) -> i64 {
    u as i64
}

#[inline]
#[must_use]
pub fn u32_sat(u: usize) -> u32 {
    u32::try_from(u).unwrap_or(u32::MAX)
}

#[inline]
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
pub fn i32_wrap(u: usize) -> i32 {
    u as i32
}
