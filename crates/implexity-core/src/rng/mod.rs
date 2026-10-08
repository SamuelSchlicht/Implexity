// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


mod tables;

use crate::error::{CaeError, CaeResult};
use tables::{FI_DOUBLE, KI_DOUBLE, WI_DOUBLE, ZIGGURAT_NOR_INV_R, ZIGGURAT_NOR_R};

const POOL_SIZE: usize = 4;
const INIT_A: u32 = 0x43b0_d7e5;
const MULT_A: u32 = 0x931e_8875;
const INIT_B: u32 = 0x8b51_f9dd;
const MULT_B: u32 = 0x58f3_8ded;
const MIX_MULT_L: u32 = 0xca01_f9dd;
const MIX_MULT_R: u32 = 0x4973_f715;
const XSHIFT: u32 = 16;

fn hashmix(value: u32, hash_const: &mut u32) -> u32 {
    let mut value = value ^ *hash_const;
    *hash_const = hash_const.wrapping_mul(MULT_A);
    value = value.wrapping_mul(*hash_const);
    value ^ (value >> XSHIFT)
}

fn mix(x: u32, y: u32) -> u32 {
    let result = MIX_MULT_L.wrapping_mul(x).wrapping_sub(MIX_MULT_R.wrapping_mul(y));
    result ^ (result >> XSHIFT)
}

#[must_use]
pub fn int_to_words(n: u128) -> Vec<u32> {
    if n == 0 {
        return vec![0];
    }
    let mut out = Vec::new();
    let mut n = n;
    while n > 0 {
        out.push(u32::try_from(n & 0xffff_ffff_u128).unwrap_or(0));
        n >>= 32;
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedSequence {
    entropy: Vec<u32>,
    spawn_key: Vec<u32>,
    n_children_spawned: u32,
    pool: [u32; POOL_SIZE],
}

impl SeedSequence {
    #[must_use]
    pub fn new(seed: u128) -> Self {
        Self::from_words(&int_to_words(seed), &[])
    }

    #[must_use]
    pub fn from_seeds(seeds: &[u128]) -> Self {
        let words: Vec<u32> = seeds.iter().flat_map(|&s| int_to_words(s)).collect();
        Self::from_words(&words, &[])
    }

    #[must_use]
    pub fn from_words(entropy: &[u32], spawn_key: &[u32]) -> Self {
        let mut run = entropy.to_vec();
        if !spawn_key.is_empty() && run.len() < POOL_SIZE {
            run.resize(POOL_SIZE, 0);
        }
        let assembled: Vec<u32> = run.iter().chain(spawn_key.iter()).copied().collect();
        let mut pool = [0u32; POOL_SIZE];
        let mut hash_const = INIT_A;
        for (i, slot) in pool.iter_mut().enumerate() {
            *slot = hashmix(assembled.get(i).copied().unwrap_or(0), &mut hash_const);
        }
        for i_src in 0..POOL_SIZE {
            for i_dst in 0..POOL_SIZE {
                if i_src != i_dst {
                    let h = hashmix(pool[i_src], &mut hash_const);
                    pool[i_dst] = mix(pool[i_dst], h);
                }
            }
        }
        for &word in assembled.iter().skip(POOL_SIZE) {
            for slot in &mut pool {
                let h = hashmix(word, &mut hash_const);
                *slot = mix(*slot, h);
            }
        }
        Self { entropy: entropy.to_vec(), spawn_key: spawn_key.to_vec(), n_children_spawned: 0, pool }
    }

    #[must_use]
    pub fn generate_state_u32(&self, n_words: usize) -> Vec<u32> {
        let mut hash_const = INIT_B;
        (0..n_words)
            .map(|i| {
                let mut v = self.pool[i % POOL_SIZE] ^ hash_const;
                hash_const = hash_const.wrapping_mul(MULT_B);
                v = v.wrapping_mul(hash_const);
                v ^ (v >> XSHIFT)
            })
            .collect()
    }

    #[must_use]
    pub fn generate_state_u64(&self, n_words: usize) -> Vec<u64> {
        self.generate_state_u32(2 * n_words)
            .chunks_exact(2)
            .map(|p| u64::from(p[0]) | (u64::from(p[1]) << 32))
            .collect()
    }

    pub fn spawn(&mut self, n_children: u32) -> Vec<SeedSequence> {
        let start = self.n_children_spawned;
        self.n_children_spawned += n_children;
        (start..start + n_children)
            .map(|i| {
                let mut key = self.spawn_key.clone();
                key.extend(int_to_words(u128::from(i)));
                SeedSequence::from_words(&self.entropy, &key)
            })
            .collect()
    }

    #[must_use]
    pub fn pool(&self) -> [u32; POOL_SIZE] {
        self.pool
    }
}

const PCG_MULT: u128 = 0x2360_ed05_1fc6_5da4_4385_df64_9fcc_f645;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcg64 {
    state: u128,
    inc: u128,
    has_uint32: bool,
    uinteger: u32,
}

impl Pcg64 {
    #[must_use]
    pub fn from_seed_sequence(seq: &SeedSequence) -> Self {
        let v = seq.generate_state_u64(4);
        let seed = (u128::from(v[0]) << 64) | u128::from(v[1]);
        let inc = (u128::from(v[2]) << 64) | u128::from(v[3]);
        let mut rng = Self { state: 0, inc: (inc << 1) | 1, has_uint32: false, uinteger: 0 };
        rng.step();
        rng.state = rng.state.wrapping_add(seed);
        rng.step();
        rng
    }

    fn step(&mut self) {
        self.state = self.state.wrapping_mul(PCG_MULT).wrapping_add(self.inc);
    }

    pub fn next_u64(&mut self) -> u64 {
        self.step();
        let hi = u64::try_from(self.state >> 64).unwrap_or(0);
        let lo = u64::try_from(self.state & u128::from(u64::MAX)).unwrap_or(0);
        let rot = u32::try_from(self.state >> 122).unwrap_or(0);
        (hi ^ lo).rotate_right(rot)
    }

    pub fn next_u32(&mut self) -> u32 {
        if self.has_uint32 {
            self.has_uint32 = false;
            return self.uinteger;
        }
        let next = self.next_u64();
        self.has_uint32 = true;
        self.uinteger = u32::try_from(next >> 32).unwrap_or(0);
        u32::try_from(next & 0xffff_ffff).unwrap_or(0)
    }

    pub fn next_f64(&mut self) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let x = (self.next_u64() >> 11) as f64;
        x * (1.0 / 9_007_199_254_740_992.0)
    }

    #[must_use]
    pub fn state(&self) -> (u128, u128) {
        (self.state, self.inc)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generator {
    bits: Pcg64,
}

#[must_use]
pub fn default_rng(seed: u128) -> Generator {
    Generator::new(&SeedSequence::new(seed))
}

impl Generator {
    #[must_use]
    pub fn new(seq: &SeedSequence) -> Self {
        Self { bits: Pcg64::from_seed_sequence(seq) }
    }

    pub fn bit_generator(&mut self) -> &mut Pcg64 {
        &mut self.bits
    }

    pub fn random(&mut self) -> f64 {
        self.bits.next_f64()
    }

    pub fn random_vec(&mut self, n: usize) -> Vec<f64> {
        (0..n).map(|_| self.bits.next_f64()).collect()
    }


    pub fn uniform(&mut self, low: f64, high: f64) -> CaeResult<f64> {
        let range = high - low;
        if !range.is_finite() {
            return Err(CaeError::contract("Range exceeds valid bounds"));
        }
        Ok(low + range * self.bits.next_f64())
    }


    pub fn uniform_vec(&mut self, low: f64, high: f64, n: usize) -> CaeResult<Vec<f64>> {
        (0..n).map(|_| self.uniform(low, high)).collect()
    }

    pub fn standard_normal(&mut self) -> f64 {
        loop {
            let mut r = self.bits.next_u64();
            let idx = usize::try_from(r & 0xff).unwrap_or(0);
            r >>= 8;
            let sign = r & 1;
            let rabs = (r >> 1) & 0x000f_ffff_ffff_ffff;
            #[allow(clippy::cast_precision_loss)]
            let mut x = rabs as f64 * WI_DOUBLE[idx];
            if sign & 1 == 1 {
                x = -x;
            }
            if rabs < KI_DOUBLE[idx] {
                return x;
            }
            if idx == 0 {
                loop {
                    let xx = -ZIGGURAT_NOR_INV_R * (-self.bits.next_f64()).ln_1p();
                    let yy = -(-self.bits.next_f64()).ln_1p();
                    if yy + yy > xx * xx {
                        return if (rabs >> 8) & 1 == 1 {
                            -(ZIGGURAT_NOR_R + xx)
                        } else {
                            ZIGGURAT_NOR_R + xx
                        };
                    }
                }
            } else if (FI_DOUBLE[idx - 1] - FI_DOUBLE[idx]) * self.bits.next_f64() + FI_DOUBLE[idx]
                < (-0.5 * x * x).exp()
            {
                return x;
            }
        }
    }

    pub fn standard_normal_vec(&mut self, n: usize) -> Vec<f64> {
        (0..n).map(|_| self.standard_normal()).collect()
    }


    pub fn normal(&mut self, loc: f64, scale: f64) -> CaeResult<f64> {
        if scale.is_nan() || scale < 0.0 {
            return Err(CaeError::contract("scale < 0"));
        }
        Ok(loc + scale * self.standard_normal())
    }


    pub fn normal_vec(&mut self, loc: f64, scale: f64, n: usize) -> CaeResult<Vec<f64>> {
        (0..n).map(|_| self.normal(loc, scale)).collect()
    }

    pub fn bounded_u64(&mut self, off: u64, rng: u64) -> u64 {
        if rng == 0 {
            off
        } else if rng <= 0xffff_ffff {
            if rng == 0xffff_ffff {
                return off.wrapping_add(u64::from(self.bits.next_u32()));
            }
            off.wrapping_add(u64::from(self.lemire_u32(u32::try_from(rng).unwrap_or(u32::MAX))))
        } else if rng == u64::MAX {
            off.wrapping_add(self.bits.next_u64())
        } else {
            off.wrapping_add(self.lemire_u64(rng))
        }
    }

    fn lemire_u32(&mut self, rng: u32) -> u32 {
        let rng_excl = rng + 1;
        let mut m = u64::from(self.bits.next_u32()) * u64::from(rng_excl);
        let mut leftover = u32::try_from(m & 0xffff_ffff).unwrap_or(0);
        if leftover < rng_excl {
            let threshold = (u32::MAX - rng) % rng_excl;
            while leftover < threshold {
                m = u64::from(self.bits.next_u32()) * u64::from(rng_excl);
                leftover = u32::try_from(m & 0xffff_ffff).unwrap_or(0);
            }
        }
        u32::try_from(m >> 32).unwrap_or(0)
    }

    fn lemire_u64(&mut self, rng: u64) -> u64 {
        let rng_excl = rng + 1;
        let mut m = u128::from(self.bits.next_u64()) * u128::from(rng_excl);
        let mut leftover = u64::try_from(m & u128::from(u64::MAX)).unwrap_or(0);
        if leftover < rng_excl {
            let threshold = (u64::MAX - rng) % rng_excl;
            while leftover < threshold {
                m = u128::from(self.bits.next_u64()) * u128::from(rng_excl);
                leftover = u64::try_from(m & u128::from(u64::MAX)).unwrap_or(0);
            }
        }
        u64::try_from(m >> 64).unwrap_or(0)
    }


    pub fn integers(&mut self, low: i64, high: i64, n: usize) -> CaeResult<Vec<i64>> {
        self.integers_endpoint(low, high, n, false)
    }


    pub fn integers_endpoint(
        &mut self,
        low: i64,
        high: i64,
        n: usize,
        endpoint: bool,
    ) -> CaeResult<Vec<i64>> {
        let high_closed = if endpoint {
            high
        } else {
            high.checked_sub(1).ok_or_else(|| CaeError::contract("high is out of bounds for int64"))?
        };
        if low > high_closed {
            return Err(CaeError::contract(if endpoint { "low > high" } else { "low >= high" }));
        }
        let rng = high_closed.wrapping_sub(low).cast_unsigned();
        let off = low.cast_unsigned();
        Ok((0..n).map(|_| self.bounded_fill_one(off, rng).cast_signed()).collect())
    }

    fn bounded_fill_one(&mut self, off: u64, rng: u64) -> u64 {
        self.bounded_u64(off, rng)
    }

    fn shuffle_indices(&mut self, data: &mut [i64], first: usize) {
        let n = data.len();
        for i in (first..n).rev() {
            let j = usize::try_from(self.bounded_u64(0, i as u64)).unwrap_or(0);
            data.swap(i, j);
        }
    }

    pub fn random_interval(&mut self, max: u64) -> u64 {
        if max == 0 {
            return 0;
        }
        let mut mask = max;
        for s in [1, 2, 4, 8, 16, 32] {
            mask |= mask >> s;
        }
        loop {
            let value = if max <= 0xffff_ffff {
                u64::from(self.bits.next_u32()) & mask
            } else {
                self.bits.next_u64() & mask
            };
            if value <= max {
                return value;
            }
        }
    }

    pub fn shuffle<T>(&mut self, data: &mut [T]) {
        for i in (1..data.len()).rev() {
            let j = usize::try_from(self.random_interval(i as u64)).unwrap_or(0);
            data.swap(i, j);
        }
    }

    pub fn permutation(&mut self, n: usize) -> Vec<i64> {
        let mut data: Vec<i64> = (0..n).map(|i| i64::try_from(i).unwrap_or(i64::MAX)).collect();
        self.shuffle(&mut data);
        data
    }


    pub fn choice_with_replacement(&mut self, pop_size: usize, size: usize) -> CaeResult<Vec<i64>> {
        if pop_size == 0 && size != 0 {
            return Err(CaeError::contract("a must be a positive integer unless no samples are taken"));
        }
        if size == 0 {
            return Ok(Vec::new());
        }
        self.integers(0, i64::try_from(pop_size).unwrap_or(i64::MAX), size)
    }


    pub fn choice_without_replacement(&mut self, pop_size: usize, size: usize) -> CaeResult<Vec<i64>> {
        self.choice_without_replacement_shuffle(pop_size, size, true)
    }


    pub fn choice_without_replacement_shuffle(
        &mut self,
        pop_size: usize,
        size: usize,
        shuffle: bool,
    ) -> CaeResult<Vec<i64>> {
        if pop_size == 0 && size != 0 {
            return Err(CaeError::contract("a must be a positive integer unless no samples are taken"));
        }
        if size > pop_size {
            return Err(CaeError::contract(
                "Cannot take a larger sample than population when replace is False",
            ));
        }
        let cutoff = if shuffle { 50 } else { 20 };
        if pop_size > 10_000 && size > pop_size / cutoff {
            let mut idx: Vec<i64> = (0..pop_size).map(|i| i64::try_from(i).unwrap_or(i64::MAX)).collect();
            let first = (pop_size - size).max(1);
            self.shuffle_indices(&mut idx, first);
            return Ok(idx[pop_size - size..].to_vec());
        }
        let mut idx = vec![0i64; size];
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let set_size = (1.2 * size as f64) as u64;
        let mask = gen_mask(set_size);
        let set_len = usize::try_from(mask + 1).unwrap_or(usize::MAX);
        let mut hash_set = vec![u64::MAX; set_len];
        for j in (pop_size - size)..pop_size {
            let ju = j as u64;
            let val = self.bounded_u64(0, ju);
            let mut loc = usize::try_from(val & mask).unwrap_or(0);
            while hash_set[loc] != u64::MAX && hash_set[loc] != val {
                loc = (loc + 1) & usize::try_from(mask).unwrap_or(0);
            }
            let slot = j - (pop_size - size);
            if hash_set[loc] == u64::MAX {
                hash_set[loc] = val;
                idx[slot] = val.cast_signed();
            } else {
                loc = usize::try_from(ju & mask).unwrap_or(0);
                while hash_set[loc] != u64::MAX {
                    loc = (loc + 1) & usize::try_from(mask).unwrap_or(0);
                }
                hash_set[loc] = ju;
                idx[slot] = ju.cast_signed();
            }
        }
        if shuffle && size > 1 {
            self.shuffle_indices(&mut idx, 1);
        }
        Ok(idx)
    }
}

fn gen_mask(max: u64) -> u64 {
    let mut mask = max;
    mask |= mask >> 1;
    mask |= mask >> 2;
    mask |= mask >> 4;
    mask |= mask >> 8;
    mask |= mask >> 16;
    mask |= mask >> 32;
    mask
}

