// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;
const W_BITS: usize = 15;
const W_SIZE: usize = 1 << W_BITS;
const W_MASK: usize = W_SIZE - 1;
const WINDOW_SIZE: usize = 2 * W_SIZE;
const MAX_DIST: usize = W_SIZE - MIN_LOOKAHEAD;
const HASH_BITS: usize = 8 + 7;
const HASH_SIZE: usize = 1 << HASH_BITS;
const HASH_MASK: usize = HASH_SIZE - 1;
const HASH_SHIFT: usize = HASH_BITS.div_ceil(3);
const LIT_BUFSIZE: usize = 1 << (8 + 6);
const SYM_END: usize = (LIT_BUFSIZE - 1) * 3;
const TOO_FAR: usize = 4096;
const WIN_INIT: usize = MAX_MATCH;

const LENGTH_CODES: usize = 29;
const LITERALS: usize = 256;
const L_CODES: usize = LITERALS + 1 + LENGTH_CODES;
const D_CODES: usize = 30;
const BL_CODES: usize = 19;
const HEAP_SIZE: usize = 2 * L_CODES + 1;
const MAX_BITS: usize = 15;
const MAX_BL_BITS: usize = 7;
const END_BLOCK: usize = 256;
const REP_3_6: usize = 16;
const REPZ_3_10: usize = 17;
const REPZ_11_138: usize = 18;

const EXTRA_LBITS: [usize; LENGTH_CODES] =
    [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const EXTRA_DBITS: [usize; D_CODES] =
    [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
const EXTRA_BLBITS: [usize; BL_CODES] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];
const BL_ORDER: [usize; BL_CODES] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

const CONFIG: [(usize, usize, usize, usize, bool); 10] = [
    (0, 0, 0, 0, false),
    (4, 4, 8, 4, false),
    (4, 5, 16, 8, false),
    (4, 6, 32, 32, false),
    (4, 4, 16, 16, true),
    (8, 16, 32, 32, true),
    (8, 16, 128, 128, true),
    (8, 32, 128, 256, true),
    (32, 128, 258, 1024, true),
    (32, 258, 258, 4096, true),
];

struct Tables {
    length_code: [u8; 256],
    dist_code: [u8; 512],
    base_length: [usize; LENGTH_CODES],
    base_dist: [usize; D_CODES],
    static_ltree: Vec<(u16, u16)>,
    static_dtree: Vec<(u16, u16)>,
}

fn bi_reverse(mut code: usize, mut len: usize) -> u16 {
    let mut res = 0usize;
    loop {
        res |= code & 1;
        code >>= 1;
        res <<= 1;
        len -= 1;
        if len == 0 {
            break;
        }
    }
    u16::try_from(res >> 1).unwrap_or(0)
}

fn gen_codes(codes: &mut [u16], lens: &[u16], max_code: usize, bl_count: &[usize; MAX_BITS + 1]) {
    let mut next_code = [0usize; MAX_BITS + 1];
    let mut code = 0usize;
    for bits in 1..=MAX_BITS {
        code = (code + bl_count[bits - 1]) << 1;
        next_code[bits] = code;
    }
    for n in 0..=max_code {
        let len = usize::from(lens[n]);
        if len == 0 {
            continue;
        }
        codes[n] = bi_reverse(next_code[len], len);
        next_code[len] += 1;
    }
}

fn tables() -> &'static Tables {
    static T: std::sync::OnceLock<Tables> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut length_code = [0u8; 256];
        let mut base_length = [0usize; LENGTH_CODES];
        let mut length = 0usize;
        let mut code = 0usize;
        while code < LENGTH_CODES - 1 {
            base_length[code] = length;
            for _ in 0..(1usize << EXTRA_LBITS[code]) {
                length_code[length] = u8::try_from(code).unwrap_or(0);
                length += 1;
            }
            code += 1;
        }
        length_code[length - 1] = u8::try_from(code).unwrap_or(0);
        let mut dist_code = [0u8; 512];
        let mut base_dist = [0usize; D_CODES];
        let mut dist = 0usize;
        code = 0;
        while code < 16 {
            base_dist[code] = dist;
            for _ in 0..(1usize << EXTRA_DBITS[code]) {
                dist_code[dist] = u8::try_from(code).unwrap_or(0);
                dist += 1;
            }
            code += 1;
        }
        dist >>= 7;
        while code < D_CODES {
            base_dist[code] = dist << 7;
            for _ in 0..(1usize << (EXTRA_DBITS[code] - 7)) {
                dist_code[256 + dist] = u8::try_from(code).unwrap_or(0);
                dist += 1;
            }
            code += 1;
        }
        let mut bl_count = [0usize; MAX_BITS + 1];
        let mut lens = vec![0u16; L_CODES + 2];
        for (n, l) in lens.iter_mut().enumerate() {
            let len = match n {
                0..=143 => 8,
                144..=255 => 9,
                256..=279 => 7,
                _ => 8,
            };
            *l = len;
            bl_count[usize::from(len)] += 1;
        }
        let mut codes = vec![0u16; L_CODES + 2];
        gen_codes(&mut codes, &lens, L_CODES + 1, &bl_count);
        let static_ltree = codes.into_iter().zip(lens).collect();
        let static_dtree = (0..D_CODES).map(|n| (bi_reverse(n, 5), 5u16)).collect();
        Tables { length_code, dist_code, base_length, base_dist, static_ltree, static_dtree }
    })
}

fn d_code(t: &Tables, dist: usize) -> usize {
    if dist < 256 { usize::from(t.dist_code[dist]) } else { usize::from(t.dist_code[256 + (dist >> 7)]) }
}

struct Tree {
    freq: Vec<usize>,
    code: Vec<u16>,
    dad: Vec<usize>,
    len: Vec<u16>,
    max_code: usize,
    elems: usize,
    extra: &'static [usize],
    extra_base: usize,
    max_length: usize,
    has_static: bool,
}

impl Tree {
    fn new(
        size: usize,
        elems: usize,
        extra: &'static [usize],
        extra_base: usize,
        max_length: usize,
        has_static: bool,
    ) -> Self {
        Self {
            freq: vec![0; size],
            code: vec![0; size],
            dad: vec![0; size],
            len: vec![0; size],
            max_code: 0,
            elems,
            extra,
            extra_base,
            max_length,
            has_static,
        }
    }
}

struct BitWriter {
    out: Vec<u8>,
    bit_buf: u32,
    bit_count: u32,
}

impl BitWriter {
    fn send_bits(&mut self, value: usize, length: usize) {
        let value = u32::try_from(value).unwrap_or(0);
        let length = u32::try_from(length).unwrap_or(0);
        self.bit_buf |= value << self.bit_count;
        self.bit_count += length;
        while self.bit_count >= 8 {
            self.out.push((self.bit_buf & 0xff) as u8);
            self.bit_buf >>= 8;
            self.bit_count -= 8;
        }
    }

    fn windup(&mut self) {
        if self.bit_count > 0 {
            self.out.push((self.bit_buf & 0xff) as u8);
        }
        self.bit_buf = 0;
        self.bit_count = 0;
    }
}

struct Deflate<'a> {
    input: &'a [u8],
    next_in: usize,
    window: Vec<u8>,
    prev: Vec<u16>,
    head: Vec<u16>,
    ins_h: usize,
    strstart: usize,
    block_start: isize,
    match_length: usize,
    prev_match: usize,
    match_available: bool,
    match_start: usize,
    prev_length: usize,
    lookahead: usize,
    insert: usize,
    high_water: usize,
    good_match: usize,
    max_lazy: usize,
    nice_match: usize,
    max_chain: usize,
    sym: Vec<u8>,
    ltree: Tree,
    dtree: Tree,
    bltree: Tree,
    heap: Vec<usize>,
    heap_len: usize,
    heap_max: usize,
    depth: Vec<u8>,
    bl_count: [usize; MAX_BITS + 1],
    opt_len: usize,
    static_len: usize,
    bits: BitWriter,
}

impl<'a> Deflate<'a> {
    fn new(input: &'a [u8], level: usize) -> Self {
        let (good, lazy, nice, chain, _) = CONFIG[level];
        let mut s = Self {
            input,
            next_in: 0,
            window: vec![0; WINDOW_SIZE],
            prev: vec![0; W_SIZE],
            head: vec![0; HASH_SIZE],
            ins_h: 0,
            strstart: 0,
            block_start: 0,
            match_length: MIN_MATCH - 1,
            prev_match: 0,
            match_available: false,
            match_start: 0,
            prev_length: MIN_MATCH - 1,
            lookahead: 0,
            insert: 0,
            high_water: 0,
            good_match: good,
            max_lazy: lazy,
            nice_match: nice,
            max_chain: chain,
            sym: Vec::with_capacity(SYM_END),
            ltree: Tree::new(HEAP_SIZE, L_CODES, &EXTRA_LBITS, LITERALS + 1, MAX_BITS, true),
            dtree: Tree::new(2 * D_CODES + 1, D_CODES, &EXTRA_DBITS, 0, MAX_BITS, true),
            bltree: Tree::new(2 * BL_CODES + 1, BL_CODES, &EXTRA_BLBITS, 0, MAX_BL_BITS, false),
            heap: vec![0; 2 * L_CODES + 1],
            heap_len: 0,
            heap_max: 0,
            depth: vec![0; 2 * L_CODES + 1],
            bl_count: [0; MAX_BITS + 1],
            opt_len: 0,
            static_len: 0,
            bits: BitWriter { out: Vec::new(), bit_buf: 0, bit_count: 0 },
        };
        s.init_block();
        s
    }

    fn init_block(&mut self) {
        for n in 0..L_CODES {
            self.ltree.freq[n] = 0;
        }
        for n in 0..D_CODES {
            self.dtree.freq[n] = 0;
        }
        for n in 0..BL_CODES {
            self.bltree.freq[n] = 0;
        }
        self.ltree.freq[END_BLOCK] = 1;
        self.opt_len = 0;
        self.static_len = 0;
        self.sym.clear();
    }

    fn update_hash(h: usize, c: u8) -> usize {
        ((h << HASH_SHIFT) ^ usize::from(c)) & HASH_MASK
    }

    fn insert_string(&mut self, pos: usize) -> usize {
        self.ins_h = Self::update_hash(self.ins_h, self.window[pos + MIN_MATCH - 1]);
        let head = usize::from(self.head[self.ins_h]);
        self.prev[pos & W_MASK] = self.head[self.ins_h];
        self.head[self.ins_h] = u16::try_from(pos).unwrap_or(0);
        head
    }

    fn slide_hash(&mut self) {
        for h in &mut self.head {
            *h = if usize::from(*h) >= W_SIZE {
                u16::try_from(usize::from(*h) - W_SIZE).unwrap_or(0)
            } else {
                0
            };
        }
        for p in &mut self.prev {
            *p = if usize::from(*p) >= W_SIZE {
                u16::try_from(usize::from(*p) - W_SIZE).unwrap_or(0)
            } else {
                0
            };
        }
    }

    fn fill_window(&mut self) {
        loop {
            let mut more = WINDOW_SIZE - self.lookahead - self.strstart;
            if self.strstart >= W_SIZE + MAX_DIST {
                self.window.copy_within(W_SIZE..W_SIZE + (W_SIZE - more), 0);
                self.match_start = self.match_start.wrapping_sub(W_SIZE);
                self.strstart -= W_SIZE;
                self.block_start -= W_SIZE as isize;
                if self.insert > self.strstart {
                    self.insert = self.strstart;
                }
                self.slide_hash();
                more += W_SIZE;
            }
            if self.next_in >= self.input.len() {
                break;
            }
            let n = more.min(self.input.len() - self.next_in);
            let at = self.strstart + self.lookahead;
            self.window[at..at + n].copy_from_slice(&self.input[self.next_in..self.next_in + n]);
            self.next_in += n;
            self.lookahead += n;
            if self.lookahead + self.insert >= MIN_MATCH {
                let mut pos = self.strstart - self.insert;
                self.ins_h = usize::from(self.window[pos]);
                self.ins_h = Self::update_hash(self.ins_h, self.window[pos + 1]);
                while self.insert > 0 {
                    self.ins_h = Self::update_hash(self.ins_h, self.window[pos + MIN_MATCH - 1]);
                    self.prev[pos & W_MASK] = self.head[self.ins_h];
                    self.head[self.ins_h] = u16::try_from(pos).unwrap_or(0);
                    pos += 1;
                    self.insert -= 1;
                    if self.lookahead + self.insert < MIN_MATCH {
                        break;
                    }
                }
            }
            if !(self.lookahead < MIN_LOOKAHEAD && self.next_in < self.input.len()) {
                break;
            }
        }
        if self.high_water < WINDOW_SIZE {
            let curr = self.strstart + self.lookahead;
            if self.high_water < curr {
                let init = (WINDOW_SIZE - curr).min(WIN_INIT);
                self.window[curr..curr + init].fill(0);
                self.high_water = curr + init;
            } else if self.high_water < curr + WIN_INIT {
                let init = (curr + WIN_INIT - self.high_water).min(WINDOW_SIZE - self.high_water);
                let hw = self.high_water;
                self.window[hw..hw + init].fill(0);
                self.high_water += init;
            }
        }
    }

    fn longest_match(&mut self, mut cur_match: usize) -> usize {
        let mut chain_length = self.max_chain;
        let scan = self.strstart;
        let mut best_len = self.prev_length;
        let mut nice_match = self.nice_match;
        let limit = self.strstart.saturating_sub(MAX_DIST);
        let strend = self.strstart + MAX_MATCH;
        let w = &self.window;
        let mut scan_end1 = w[scan + best_len - 1];
        let mut scan_end = w[scan + best_len];
        if self.prev_length >= self.good_match {
            chain_length >>= 2;
        }
        if nice_match > self.lookahead {
            nice_match = self.lookahead;
        }
        loop {
            let m = cur_match;
            let skip = w[m + best_len] != scan_end
                || w[m + best_len - 1] != scan_end1
                || w[m] != w[scan]
                || w[m + 1] != w[scan + 1];
            if !skip {

                let mut s = scan + 2;
                let mut mm = m + 2;
                loop {
                    let mut stop = false;
                    for _ in 0..8 {
                        s += 1;
                        mm += 1;
                        if w[s] != w[mm] {
                            stop = true;
                            break;
                        }
                    }
                    if stop || s >= strend {
                        break;
                    }
                }
                let len = MAX_MATCH - (strend - s);
                if len > best_len {
                    self.match_start = cur_match;
                    best_len = len;
                    if len >= nice_match {
                        break;
                    }
                    scan_end1 = w[scan + best_len - 1];
                    scan_end = w[scan + best_len];
                }
            }
            cur_match = usize::from(self.prev[cur_match & W_MASK]);
            if cur_match <= limit {
                break;
            }
            chain_length -= 1;
            if chain_length == 0 {
                break;
            }
        }
        if best_len <= self.lookahead { best_len } else { self.lookahead }
    }

    fn tally_lit(&mut self, c: u8) -> bool {
        self.sym.extend_from_slice(&[0, 0, c]);
        self.ltree.freq[usize::from(c)] += 1;
        self.sym.len() == SYM_END
    }

    fn tally_dist(&mut self, dist: usize, len: usize) -> bool {
        let t = tables();
        self.sym.extend_from_slice(&[(dist & 0xff) as u8, (dist >> 8) as u8, u8::try_from(len).unwrap_or(0)]);
        let d = dist - 1;
        self.ltree.freq[usize::from(t.length_code[len]) + LITERALS + 1] += 1;
        self.dtree.freq[d_code(t, d)] += 1;
        self.sym.len() == SYM_END
    }

    fn flush_block(&mut self, last: bool) {
        let stored_len = self.strstart as isize - self.block_start;
        let buf = (self.block_start >= 0).then(|| {
            let bs = usize::try_from(self.block_start).unwrap_or(0);
            (bs, usize::try_from(stored_len).unwrap_or(0))
        });
        self.tr_flush_block(buf, usize::try_from(stored_len).unwrap_or(0), last);
        self.block_start = self.strstart as isize;
    }

    fn deflate_fast(&mut self) {
        loop {
            if self.lookahead < MIN_LOOKAHEAD {
                self.fill_window();
                if self.lookahead == 0 {
                    break;
                }
            }
            let mut hash_head = 0;
            if self.lookahead >= MIN_MATCH {
                hash_head = self.insert_string(self.strstart);
            }
            if hash_head != 0 && self.strstart - hash_head <= MAX_DIST {
                self.match_length = self.longest_match(hash_head);
            }
            let bflush;
            if self.match_length >= MIN_MATCH {
                bflush = self.tally_dist(self.strstart - self.match_start, self.match_length - MIN_MATCH);
                self.lookahead -= self.match_length;
                if self.match_length <= self.max_lazy && self.lookahead >= MIN_MATCH {
                    self.match_length -= 1;
                    loop {
                        self.strstart += 1;
                        self.insert_string(self.strstart);
                        self.match_length -= 1;
                        if self.match_length == 0 {
                            break;
                        }
                    }
                    self.strstart += 1;
                } else {
                    self.strstart += self.match_length;
                    self.match_length = 0;
                    self.ins_h = usize::from(self.window[self.strstart]);
                    self.ins_h = Self::update_hash(self.ins_h, self.window[self.strstart + 1]);
                }
            } else {
                bflush = self.tally_lit(self.window[self.strstart]);
                self.lookahead -= 1;
                self.strstart += 1;
            }
            if bflush {
                self.flush_block(false);
            }
        }
        self.insert = self.strstart.min(MIN_MATCH - 1);
        self.flush_block(true);
    }

    fn deflate_slow(&mut self) {
        loop {
            if self.lookahead < MIN_LOOKAHEAD {
                self.fill_window();
                if self.lookahead == 0 {
                    break;
                }
            }
            let mut hash_head = 0;
            if self.lookahead >= MIN_MATCH {
                hash_head = self.insert_string(self.strstart);
            }
            self.prev_length = self.match_length;
            self.prev_match = self.match_start;
            self.match_length = MIN_MATCH - 1;
            if hash_head != 0 && self.prev_length < self.max_lazy && self.strstart - hash_head <= MAX_DIST {
                self.match_length = self.longest_match(hash_head);
                if self.match_length <= 5
                    && self.match_length == MIN_MATCH
                    && self.strstart - self.match_start > TOO_FAR
                {
                    self.match_length = MIN_MATCH - 1;
                }
            }
            if self.prev_length >= MIN_MATCH && self.match_length <= self.prev_length {
                let max_insert = self.strstart + self.lookahead - MIN_MATCH;
                let bflush =
                    self.tally_dist(self.strstart - 1 - self.prev_match, self.prev_length - MIN_MATCH);
                self.lookahead -= self.prev_length - 1;
                self.prev_length -= 2;
                loop {
                    self.strstart += 1;
                    if self.strstart <= max_insert {
                        self.insert_string(self.strstart);
                    }
                    self.prev_length -= 1;
                    if self.prev_length == 0 {
                        break;
                    }
                }
                self.match_available = false;
                self.match_length = MIN_MATCH - 1;
                self.strstart += 1;
                if bflush {
                    self.flush_block(false);
                }
            } else if self.match_available {
                let bflush = self.tally_lit(self.window[self.strstart - 1]);
                if bflush {
                    self.flush_block(false);
                }
                self.strstart += 1;
                self.lookahead -= 1;
            } else {
                self.match_available = true;
                self.strstart += 1;
                self.lookahead -= 1;
            }
        }
        if self.match_available {
            self.tally_lit(self.window[self.strstart - 1]);
            self.match_available = false;
        }
        self.insert = self.strstart.min(MIN_MATCH - 1);
        self.flush_block(true);
    }

    fn smaller(tree: &Tree, depth: &[u8], n: usize, m: usize) -> bool {
        tree.freq[n] < tree.freq[m] || (tree.freq[n] == tree.freq[m] && depth[n] <= depth[m])
    }

    fn pqdownheap(heap: &mut [usize], heap_len: usize, tree: &Tree, depth: &[u8], mut k: usize) {
        let v = heap[k];
        let mut j = k << 1;
        while j <= heap_len {
            if j < heap_len && Self::smaller(tree, depth, heap[j + 1], heap[j]) {
                j += 1;
            }
            if Self::smaller(tree, depth, v, heap[j]) {
                break;
            }
            heap[k] = heap[j];
            k = j;
            j <<= 1;
        }
        heap[k] = v;
    }

    fn gen_bitlen(&mut self, which: u8) {
        let heap = &self.heap;
        let tree = match which {
            0 => &mut self.ltree,
            1 => &mut self.dtree,
            _ => &mut self.bltree,
        };
        let t = tables();
        let stree: Option<&Vec<(u16, u16)>> = match which {
            0 => Some(&t.static_ltree),
            1 => Some(&t.static_dtree),
            _ => None,
        };
        let max_code = tree.max_code;
        let max_length = tree.max_length;
        let mut overflow = 0isize;
        self.bl_count = [0; MAX_BITS + 1];
        tree.len[heap[self.heap_max]] = 0;
        let mut h = self.heap_max + 1;
        while h < HEAP_SIZE {
            let n = heap[h];
            let mut bits = usize::from(tree.len[tree.dad[n]]) + 1;
            if bits > max_length {
                bits = max_length;
                overflow += 1;
            }
            tree.len[n] = u16::try_from(bits).unwrap_or(0);
            h += 1;
            if n > max_code {
                continue;
            }
            self.bl_count[bits] += 1;
            let xbits =
                if n >= tree.extra_base && tree.has_extra() { tree.extra[n - tree.extra_base] } else { 0 };
            let f = tree.freq[n];
            self.opt_len = self.opt_len.wrapping_add(f.wrapping_mul(bits + xbits));
            if let (true, Some(st)) = (tree.has_static, stree) {
                self.static_len = self.static_len.wrapping_add(f.wrapping_mul(usize::from(st[n].1) + xbits));
            }
        }
        if overflow == 0 {
            return;
        }
        loop {
            let mut bits = max_length - 1;
            while self.bl_count[bits] == 0 {
                bits -= 1;
            }
            self.bl_count[bits] -= 1;
            self.bl_count[bits + 1] += 2;
            self.bl_count[max_length] -= 1;
            overflow -= 2;
            if overflow <= 0 {
                break;
            }
        }
        let mut h = HEAP_SIZE;
        for bits in (1..=max_length).rev() {
            let mut n = self.bl_count[bits];
            while n != 0 {
                h -= 1;
                let m = heap[h];
                if m > max_code {
                    continue;
                }
                if usize::from(tree.len[m]) != bits {
                    self.opt_len = self
                        .opt_len
                        .wrapping_add(bits.wrapping_sub(usize::from(tree.len[m])).wrapping_mul(tree.freq[m]));
                    tree.len[m] = u16::try_from(bits).unwrap_or(0);
                }
                n -= 1;
            }
        }
    }

    fn build_tree(&mut self, which: u8) {
        let t = tables();
        {
            let tree = match which {
                0 => &mut self.ltree,
                1 => &mut self.dtree,
                _ => &mut self.bltree,
            };
            let elems = tree.elems;
            let mut max_code: isize = -1;
            self.heap_len = 0;
            self.heap_max = HEAP_SIZE;
            for n in 0..elems {
                if tree.freq[n] != 0 {
                    self.heap_len += 1;
                    self.heap[self.heap_len] = n;
                    max_code = n as isize;
                    self.depth[n] = 0;
                } else {
                    tree.len[n] = 0;
                }
            }
            while self.heap_len < 2 {
                let node = if max_code < 2 {
                    max_code += 1;
                    usize::try_from(max_code).unwrap_or(0)
                } else {
                    0
                };
                self.heap_len += 1;
                self.heap[self.heap_len] = node;
                tree.freq[node] = 1;
                self.depth[node] = 0;
                self.opt_len = self.opt_len.wrapping_sub(1);
                if which == 0 {
                    self.static_len = self.static_len.wrapping_sub(usize::from(t.static_ltree[node].1));
                } else if which == 1 {
                    self.static_len = self.static_len.wrapping_sub(usize::from(t.static_dtree[node].1));
                }
            }
            tree.max_code = usize::try_from(max_code).unwrap_or(0);
            let mut n = self.heap_len / 2;
            while n >= 1 {
                Self::pqdownheap(&mut self.heap, self.heap_len, tree, &self.depth, n);
                n -= 1;
            }
            let mut node = elems;
            loop {

                let n = self.heap[1];
                self.heap[1] = self.heap[self.heap_len];
                self.heap_len -= 1;
                Self::pqdownheap(&mut self.heap, self.heap_len, tree, &self.depth, 1);
                let m = self.heap[1];
                self.heap_max -= 1;
                self.heap[self.heap_max] = n;
                self.heap_max -= 1;
                self.heap[self.heap_max] = m;
                tree.freq[node] = tree.freq[n] + tree.freq[m];
                self.depth[node] = self.depth[n].max(self.depth[m]) + 1;
                tree.dad[n] = node;
                tree.dad[m] = node;
                self.heap[1] = node;
                node += 1;
                Self::pqdownheap(&mut self.heap, self.heap_len, tree, &self.depth, 1);
                if self.heap_len < 2 {
                    break;
                }
            }
            self.heap_max -= 1;
            self.heap[self.heap_max] = self.heap[1];
        }
        self.gen_bitlen(which);
        let bl_count = self.bl_count;
        let tree = match which {
            0 => &mut self.ltree,
            1 => &mut self.dtree,
            _ => &mut self.bltree,
        };
        let max_code = tree.max_code;
        let lens = tree.len.clone();
        gen_codes(&mut tree.code, &lens, max_code, &bl_count);
    }

    fn scan_tree(&mut self, which: u8) {
        let tree = if which == 0 { &mut self.ltree } else { &mut self.dtree };
        let max_code = tree.max_code;
        let mut prevlen: isize = -1;
        let mut nextlen = isize::from(tree.len[0] as i16);
        let mut count = 0usize;
        let (mut max_count, mut min_count) = (7usize, 4usize);
        if nextlen == 0 {
            max_count = 138;
            min_count = 3;
        }
        tree.len[max_code + 1] = 0xffff;
        for n in 0..=max_code {
            let curlen = nextlen;
            nextlen = if tree.len[n + 1] == 0xffff { -1 } else { isize::from(tree.len[n + 1] as i16) };
            count += 1;
            if count < max_count && curlen == nextlen {
                continue;
            }
            if count < min_count {
                self.bltree.freq[usize::try_from(curlen).unwrap_or(0)] += count;
            } else if curlen != 0 {
                if curlen != prevlen {
                    self.bltree.freq[usize::try_from(curlen).unwrap_or(0)] += 1;
                }
                self.bltree.freq[REP_3_6] += 1;
            } else if count <= 10 {
                self.bltree.freq[REPZ_3_10] += 1;
            } else {
                self.bltree.freq[REPZ_11_138] += 1;
            }
            count = 0;
            prevlen = curlen;
            if nextlen == 0 {
                max_count = 138;
                min_count = 3;
            } else if curlen == nextlen {
                max_count = 6;
                min_count = 3;
            } else {
                max_count = 7;
                min_count = 4;
            }
        }
    }

    fn send_code(bits: &mut BitWriter, tree: &Tree, c: usize) {
        bits.send_bits(usize::from(tree.code[c]), usize::from(tree.len[c]));
    }

    fn send_tree(&mut self, which: u8) {
        let tree = if which == 0 { &self.ltree } else { &self.dtree };
        let max_code = tree.max_code;
        let mut prevlen: isize = -1;
        let mut nextlen = isize::from(tree.len[0] as i16);
        let mut count = 0usize;
        let (mut max_count, mut min_count) = (7usize, 4usize);
        if nextlen == 0 {
            max_count = 138;
            min_count = 3;
        }
        for n in 0..=max_code {
            let curlen = nextlen;
            nextlen = if tree.len[n + 1] == 0xffff { -1 } else { isize::from(tree.len[n + 1] as i16) };
            count += 1;
            if count < max_count && curlen == nextlen {
                continue;
            }
            if count < min_count {
                loop {
                    Self::send_code(&mut self.bits, &self.bltree, usize::try_from(curlen).unwrap_or(0));
                    count -= 1;
                    if count == 0 {
                        break;
                    }
                }
            } else if curlen != 0 {
                if curlen != prevlen {
                    Self::send_code(&mut self.bits, &self.bltree, usize::try_from(curlen).unwrap_or(0));
                    count -= 1;
                }
                Self::send_code(&mut self.bits, &self.bltree, REP_3_6);
                self.bits.send_bits(count - 3, 2);
            } else if count <= 10 {
                Self::send_code(&mut self.bits, &self.bltree, REPZ_3_10);
                self.bits.send_bits(count - 3, 3);
            } else {
                Self::send_code(&mut self.bits, &self.bltree, REPZ_11_138);
                self.bits.send_bits(count - 11, 7);
            }
            count = 0;
            prevlen = curlen;
            if nextlen == 0 {
                max_count = 138;
                min_count = 3;
            } else if curlen == nextlen {
                max_count = 6;
                min_count = 3;
            } else {
                max_count = 7;
                min_count = 4;
            }
        }
    }

    fn build_bl_tree(&mut self) -> usize {
        self.scan_tree(0);
        self.scan_tree(1);
        self.build_tree(2);
        let mut max_blindex = BL_CODES - 1;
        while max_blindex >= 3 {
            if self.bltree.len[BL_ORDER[max_blindex]] != 0 {
                break;
            }
            max_blindex -= 1;
        }
        self.opt_len = self.opt_len.wrapping_add(3 * (max_blindex + 1) + 5 + 5 + 4);
        max_blindex
    }

    fn compress_block(&mut self, dynamic: bool) {
        let t = tables();
        let (lcode, llen, dcode, dlen): (Vec<u16>, Vec<u16>, Vec<u16>, Vec<u16>) = if dynamic {
            (self.ltree.code.clone(), self.ltree.len.clone(), self.dtree.code.clone(), self.dtree.len.clone())
        } else {
            (
                t.static_ltree.iter().map(|p| p.0).collect(),
                t.static_ltree.iter().map(|p| p.1).collect(),
                t.static_dtree.iter().map(|p| p.0).collect(),
                t.static_dtree.iter().map(|p| p.1).collect(),
            )
        };
        let send = |bits: &mut BitWriter, codes: &[u16], lens: &[u16], c: usize| {
            bits.send_bits(usize::from(codes[c]), usize::from(lens[c]));
        };
        for chunk in self.sym.chunks_exact(3) {
            let mut dist = usize::from(chunk[0]) | (usize::from(chunk[1]) << 8);
            let mut lc = usize::from(chunk[2]);
            if dist == 0 {
                send(&mut self.bits, &lcode, &llen, lc);
            } else {
                let code = usize::from(t.length_code[lc]);
                send(&mut self.bits, &lcode, &llen, code + LITERALS + 1);
                let extra = EXTRA_LBITS[code];
                if extra != 0 {
                    lc -= t.base_length[code];
                    self.bits.send_bits(lc, extra);
                }
                dist -= 1;
                let code = d_code(t, dist);
                send(&mut self.bits, &dcode, &dlen, code);
                let extra = EXTRA_DBITS[code];
                if extra != 0 {
                    dist -= t.base_dist[code];
                    self.bits.send_bits(dist, extra);
                }
            }
        }
        send(&mut self.bits, &lcode, &llen, END_BLOCK);
    }

    fn tr_flush_block(&mut self, buf: Option<(usize, usize)>, stored_len: usize, last: bool) {
        self.build_tree(0);
        self.build_tree(1);
        let max_blindex = self.build_bl_tree();
        let mut opt_lenb = self.opt_len.wrapping_add(3 + 7) >> 3;
        let static_lenb = self.static_len.wrapping_add(3 + 7) >> 3;
        if static_lenb <= opt_lenb {
            opt_lenb = static_lenb;
        }
        let last_bit = usize::from(last);
        if stored_len + 4 <= opt_lenb && buf.is_some() {
            let (start, len) = buf.unwrap_or((0, 0));
            self.bits.send_bits(last_bit, 3);
            self.bits.windup();
            let len16 = u16::try_from(len & 0xffff).unwrap_or(0);
            self.bits.out.extend_from_slice(&len16.to_le_bytes());
            self.bits.out.extend_from_slice(&(!len16).to_le_bytes());
            self.bits.out.extend_from_slice(&self.window[start..start + len]);
        } else if static_lenb == opt_lenb {
            self.bits.send_bits((1 << 1) + last_bit, 3);
            self.compress_block(false);
        } else {
            self.bits.send_bits((2 << 1) + last_bit, 3);
            let (lcodes, dcodes, blcodes) =
                (self.ltree.max_code + 1, self.dtree.max_code + 1, max_blindex + 1);
            self.bits.send_bits(lcodes - 257, 5);
            self.bits.send_bits(dcodes - 1, 5);
            self.bits.send_bits(blcodes - 4, 4);
            for rank in 0..blcodes {
                let l = usize::from(self.bltree.len[BL_ORDER[rank]]);
                self.bits.send_bits(l, 3);
            }
            self.send_tree(0);
            self.send_tree(1);
            self.compress_block(true);
        }
        self.init_block();
        if last {
            self.bits.windup();
        }
    }
}

impl Tree {
    fn has_extra(&self) -> bool {
        !self.extra.is_empty()
    }
}

#[must_use]
pub fn deflate_raw(data: &[u8], level: u32) -> Vec<u8> {
    let level = usize::try_from(level.clamp(1, 9)).unwrap_or(6);
    let mut s = Deflate::new(data, level);
    if CONFIG[level].4 {
        s.deflate_slow();
    } else {
        s.deflate_fast();
    }
    s.bits.out
}

#[must_use]
pub fn gzip(data: &[u8], level: u32) -> Vec<u8> {
    let xfl = match level.clamp(1, 9) {
        9 => 2,
        1 => 4,
        _ => 0,
    };
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, xfl, 3];
    out.extend_from_slice(&deflate_raw(data, level));
    let mut crc = flate2::Crc::new();
    crc.update(data);
    out.extend_from_slice(&crc.sum().to_le_bytes());
    out.extend_from_slice(&((data.len() & 0xffff_ffff) as u32).to_le_bytes());
    out
}

