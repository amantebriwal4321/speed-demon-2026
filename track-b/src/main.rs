use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Fx(u64);
impl Hasher for Fx {
    fn finish(&self) -> u64 { self.0.rotate_left(26) } // the multiply leaves entropy in the high bits; hashbrown indexes with the low ones
    fn write(&mut self, b: &[u8]) {
        let mut h = self.0;
        let mut c = b.chunks_exact(8);
        for x in &mut c { h = (h.rotate_left(5) ^ u64::from_le_bytes(x.try_into().unwrap())).wrapping_mul(0x517cc1b727220a95); }
        for &x in c.remainder() { h = (h.rotate_left(5) ^ x as u64).wrapping_mul(0x517cc1b727220a95); }
        self.0 = h;
    }
    fn write_usize(&mut self, x: usize) { self.0 = (self.0.rotate_left(5) ^ x as u64).wrapping_mul(0x517cc1b727220a95); }
}
type FxMap<K, V> = HashMap<K, V, BuildHasherDefault<Fx>>;

const NONE: u16 = u16::MAX;
const WIDE: u16 = u16::MAX - 1;

fn phrase_ids<'a>(p: &'a Part, d: usize) -> impl Iterator<Item = u32> + 'a {
    p.stream[p.start[d] as usize..p.start[d + 1] as usize].iter().filter(|&&t| t < WIDE).map(|&t| t as u32)
}

// term ids of one doc's tokens, from its text (u32::MAX for tokens no query uses)
fn retok(b: &[u8], lookup: &dyn Fn(&[u8]) -> Option<u32>) -> Vec<u32> {
    let end = b.iter().position(|&c| c == b'\n').unwrap_or(b.len());
    b[..end].split(|c| !c.is_ascii_alphanumeric()).filter(|t| !t.is_empty())
        .map(|t| lookup(&t.to_ascii_lowercase()).unwrap_or(u32::MAX)).collect()
}

struct Part { lo: u32, lists: Vec<u32>, loff: Vec<u32>, stream: Vec<u16>, start: Vec<u32>, extra: Vec<u32>, xstart: Vec<u32>, doff: Vec<u32> }

enum Q { Bool(Vec<(u8, u32)>), Phrase(Vec<u32>) } // op: 0 first, 1 AND, 2 OR, 3 NOT

// does sorted b contain x, searching forward from *j (galloping); moves *j along
fn seek(b: &[u32], j: &mut usize, x: u32) -> bool {
    let mut step = 1;
    while *j + step < b.len() && b[*j + step] < x { *j += step; step *= 2; }
    let hi = (*j + step + 1).min(b.len());
    *j += b[*j..hi].partition_point(|&y| y < x);
    *j < b.len() && b[*j] == x
}
fn and(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (a, b) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    if a.len() * 8 < b.len() {
        let mut j = 0;
        return a.iter().copied().filter(|&x| seek(b, &mut j, x)).collect();
    }
    let (mut i, mut j, mut o) = (0, 0, Vec::new());
    while i < a.len() && j < b.len() {
        if a[i] < b[j] { i += 1 } else if a[i] > b[j] { j += 1 } else { o.push(a[i]); i += 1; j += 1; }
    }
    o
}
fn or(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (mut i, mut j, mut o) = (0, 0, Vec::with_capacity(a.len() + b.len()));
    while i < a.len() && j < b.len() {
        if a[i] < b[j] { o.push(a[i]); i += 1 } else if a[i] > b[j] { o.push(b[j]); j += 1 } else { o.push(a[i]); i += 1; j += 1; }
    }
    o.extend_from_slice(&a[i..]); o.extend_from_slice(&b[j..]);
    o
}
fn not(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut j = 0;
    a.iter().copied().filter(|&x| !seek(b, &mut j, x)).collect()
}

// a token of up to 16 bytes, ASCII lowercased, packed little endian into a u128 (zero padded)
#[inline]
fn pack(b: &[u8], l: usize) -> u128 {
    let word = |o: usize| -> u64 {
        let mut x = [0u8; 8];
        let n = b.len().saturating_sub(o).min(8);
        x[..n].copy_from_slice(&b[o..o + n]);
        let x = u64::from_le_bytes(x);
        // set bit 5 of every byte in A..Z; token bytes are all ASCII letters or digits
        let h = x & 0x7f7f7f7f7f7f7f7f;
        let up = h.wrapping_add(0x3f3f3f3f3f3f3f3f) & !h.wrapping_add(0x2525252525252525) & !x & 0x8080808080808080;
        x | up >> 2
    };
    let mask = |n: usize| if n >= 8 { u64::MAX } else { (1u64 << (8 * n)) - 1 };
    let lo = word(0) & mask(l) as u64;
    let hi = if l > 8 { word(8) & mask(l - 8) } else { 0 };
    (hi as u128) << 64 | lo as u128
}

// bit i set when b[at + i] is a token byte (a-z, A-Z, 0-9), and when it is LF; SSE2 only
fn masks(b: &[u8], at: usize) -> (u64, u64) {
    use std::arch::x86_64::*;
    let mut pad = [0u8; 64];
    let src = if at + 64 <= b.len() { &b[at..at + 64] } else { let n = b.len() - at; pad[..n].copy_from_slice(&b[at..]); &pad[..] };
    let (mut t, mut n) = (0u64, 0u64);
    unsafe {
        for q in 0..4 {
            let x = _mm_loadu_si128(src.as_ptr().add(16 * q) as *const __m128i);
            let flip = _mm_set1_epi8(-128);
            // unsigned v < k  <=>  (v ^ 0x80) < (k ^ 0x80) as signed bytes
            let letter = _mm_cmplt_epi8(_mm_xor_si128(_mm_sub_epi8(_mm_or_si128(x, _mm_set1_epi8(0x20)), _mm_set1_epi8(b'a' as i8)), flip), _mm_set1_epi8((26u8 ^ 0x80) as i8));
            let digit = _mm_cmplt_epi8(_mm_xor_si128(_mm_sub_epi8(x, _mm_set1_epi8(b'0' as i8)), flip), _mm_set1_epi8((10u8 ^ 0x80) as i8));
            let lf = _mm_cmpeq_epi8(x, _mm_set1_epi8(b'\n' as i8));
            t |= (_mm_movemask_epi8(_mm_or_si128(letter, digit)) as u16 as u64) << (16 * q);
            n |= (_mm_movemask_epi8(lf) as u16 as u64) << (16 * q);
        }
    }
    (t, n)
}

fn term<'a>(ids: &mut FxMap<&'a [u8], u32>, t: &'a [u8]) -> u32 {
    let n = ids.len() as u32;
    *ids.entry(t).or_insert(n)
}

fn main() {
    let data = std::env::var("DATA").unwrap_or("/data".into());
    let out = std::env::var("OUT").unwrap_or("/out".into());
    let qb = std::fs::read(format!("{data}/queries.txt")).unwrap();

    // queries first: only the terms they use get indexed
    let mut ids: FxMap<&[u8], u32> = FxMap::default();
    let mut qs = Vec::new();
    // phrase terms take the low ids so the token stream can be u16
    for line in qb.split(|&c| c == b'\n') {
        if line.starts_with(b"PHRASE ") { for t in line[8..line.len() - 1].split(|&c| c == b' ') { term(&mut ids, t); } }
    }
    let np = ids.len() as u32; // ids below np are phrase terms
    for line in qb.split(|&c| c == b'\n') {
        if line.is_empty() { continue; }
        if line.starts_with(b"BOOL ") {
            let w: Vec<&[u8]> = line[5..].split(|&c| c == b' ').collect();
            let mut v = vec![(0u8, term(&mut ids, w[0]))];
            for k in (1..w.len()).step_by(2) {
                let op = match w[k] { b"AND" => 1, b"OR" => 2, _ => 3 };
                v.push((op, term(&mut ids, w[k + 1])));
            }
            qs.push(Q::Bool(v));
        } else {
            let inner = &line[8..line.len() - 1];
            qs.push(Q::Phrase(inner.split(|&c| c == b' ').map(|t| term(&mut ids, t)).collect()));
        }
    }
    let nterm = ids.len();
    // open addressing table from a token packed into a u128 (tokens of 16 bytes or less)
    // to its term id; longer query terms go to a small fallback map
    let cap = (nterm * 4).next_power_of_two().max(16);
    let shift = 128 - cap.trailing_zeros();
    let mut keys = vec![0u128; cap];
    let mut vals = vec![0u32; cap];
    let h = |k: u128| (((k as u64).wrapping_mul(0x9E3779B97F4A7C15) ^ ((k >> 64) as u64).wrapping_mul(0xC2B2AE3D27D4EB4F)) >> (shift - 64)) as usize;
    let mut long: FxMap<&[u8], u32> = FxMap::default();
    for (t, &id) in &ids {
        if t.len() > 16 { long.insert(t, id); continue; }
        let mut k = 0u128;
        for (i, &c) in t.iter().enumerate() { k |= (c as u128) << (8 * i); }
        let mut i = h(k);
        while keys[i] != 0 { i = (i + 1) & (cap - 1); }
        keys[i] = k; vals[i] = id;
    }

    let lookup = |t: &[u8]| -> Option<u32> {
        if t.len() > 16 { return long.get(t).copied(); }
        let k = pack(t, t.len());
        let mut j = h(k);
        loop { let x = keys[j]; if x == k { return Some(vals[j]); } if x == 0 { return None; } j = (j + 1) & (cap - 1); }
    };
    let docs = map(&format!("{data}/docs.txt"));
    let nt = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16);
    let mut cuts = vec![0usize];
    for k in 1..nt {
        let mut p = (docs.len() * k / nt).max(*cuts.last().unwrap());
        while p < docs.len() && p > 0 && docs[p - 1] != b'\n' { p += 1; }
        cuts.push(p);
    }
    cuts.push(docs.len());
    let mut lower = [0u8; 256]; // 0 = separator, else the lowercased byte
    for c in b'a'..=b'z' { lower[c as usize] = c; }
    for c in b'0'..=b'9' { lower[c as usize] = c; }
    for c in b'A'..=b'Z' { lower[c as usize] = c + 32; }

    // per thread: a doc list for every query term, plus the chunk's token stream as term ids
    // (u32::MAX for tokens no query uses) with each doc's start, so phrases are checked
    // by comparing ints. Chunks are in file order, so each part covers one doc range.
    let parts: Vec<Part> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..nt).map(|k| {
            let c = &docs[cuts[k]..cuts[k + 1]];
            let base = cuts[k];
            let (keys, vals, long, lower, h) = (&keys, &vals, &long, &lower, &h);
            sc.spawn(move || {
                let mut p = Part { lo: u32::MAX, lists: Vec::new(), loff: Vec::new(), stream: Vec::with_capacity(c.len() / 6), start: Vec::new(), extra: Vec::new(), xstart: Vec::new(), doff: Vec::new() };
                let mut last = vec![u32::MAX; nterm];
                let mut i = 0;
                while i < c.len() {
                    let mut doc = 0u32;
                    while c[i] != b'\t' { doc = doc * 10 + (c[i] - b'0') as u32; i += 1; }
                    if p.lo == u32::MAX { p.lo = doc; }
                    p.start.push(p.stream.len() as u32);
                    p.xstart.push(p.extra.len() as u32);
                    p.doff.push((base + i) as u32);
                    let d = p.start.len() as u32 - 1;
                    i += 1;
                    // token and LF bitmasks of a 64 byte window, then walk the bits
                    let mut o = 64; // offset inside the current window, 64 = need a new one
                    let (mut tm, mut nm) = (0u64, 0u64);
                    loop {
                        if o >= 64 { (tm, nm) = masks(docs, base + i); o = 0; }
                        let bits = (tm | nm) >> o;
                        if bits == 0 { i += 64 - o; o = 64; continue; }
                        let z = bits.trailing_zeros() as usize;
                        i += z; o += z;
                        if nm >> o & 1 == 1 { break; }
                        let s = i;
                        let run = (!(tm >> o)).trailing_zeros() as usize; // 64 when the run fills the window
                        i += run; o += run;
                        if o >= 64 { while lower[c[i] as usize] != 0 { i += 1; } o = 64; }
                        let l = i - s;
                        let id = if l <= 16 {
                            let k = pack(&docs[base + s..], l);
                            let mut j = h(k);
                            loop { let x = keys[j]; if x == k { break Some(vals[j]); } if x == 0 { break None; } j = (j + 1) & (cap - 1); }
                        } else if l <= 32 {
                            let mut t = [0u8; 32];
                            for (a, &b) in t.iter_mut().zip(&c[s..i]) { *a = lower[b as usize]; }
                            long.get(&t[..l]).copied()
                        } else { None };
                        // stream: phrase term ids, NONE for anything else, WIDE for a phrase id
                        // that does not fit (then the doc text decides). Other query terms
                        // only need presence, so they go to the per doc extra list once.
                        match id {
                            Some(t) if t < WIDE as u32 => p.stream.push(t as u16),
                            Some(t) => { p.stream.push(if t < np { WIDE } else { NONE }); if last[t as usize] != d { last[t as usize] = d; p.extra.push(t); } }
                            None => p.stream.push(NONE),
                        }
                    }
                    i += 1;
                }
                p.start.push(p.stream.len() as u32);
                p.xstart.push(p.extra.len() as u32);
                // doc lists by counting sort over the stream: count, prefix, fill. Sequential
                // reads of the stream, and only small per term arrays get random access
                last.fill(u32::MAX);
                let mut cnt = vec![0u32; nterm + 1];
                for d in 0..p.start.len() - 1 {
                    for t in phrase_ids(&p, d) {
                        if last[t as usize] != d as u32 { last[t as usize] = d as u32; cnt[t as usize + 1] += 1; }
                    }
                    for &t in &p.extra[p.xstart[d] as usize..p.xstart[d + 1] as usize] { cnt[t as usize + 1] += 1; }
                }
                for t in 0..nterm { cnt[t + 1] += cnt[t]; }
                let mut cur = cnt.clone();
                let mut lists = vec![0u32; cnt[nterm] as usize];
                last.fill(u32::MAX);
                for d in 0..p.start.len() - 1 {
                    for t in phrase_ids(&p, d) {
                        if last[t as usize] != d as u32 {
                            last[t as usize] = d as u32;
                            lists[cur[t as usize] as usize] = p.lo + d as u32;
                            cur[t as usize] += 1;
                        }
                    }
                    for &t in &p.extra[p.xstart[d] as usize..p.xstart[d + 1] as usize] {
                        lists[cur[t as usize] as usize] = p.lo + d as u32;
                        cur[t as usize] += 1;
                    }
                }
                p.lists = lists; p.loff = cnt;
                p.extra = Vec::new(); p.xstart = Vec::new();
                p
            })
        }).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });

    fn dl(p: &Part, t: u32) -> &[u32] { &p.lists[p.loff[t as usize] as usize..p.loff[t as usize + 1] as usize] }

    // BOOL queries over long lists run on a bitset of all docs: OR sets bits, NOT clears
    // them, AND keeps the list's docs whose bit is set. Ids come out ascending for free.
    let maxdoc = parts.iter().filter_map(|p| (p.start.len() > 1).then(|| p.lo + p.start.len() as u32 - 2)).max().unwrap_or(0) as usize;
    let nw = maxdoc / 64 + 1;
    let use_bits = maxdoc < 1 << 26;
    // terms in more than 1 doc in 32 also get a ready made bitset (a few MB in all), so an op
    // on them is a word wise pass instead of a walk over a long list
    let tlen = |t: usize| parts.iter().map(|p| dl(p, t as u32).len()).sum::<usize>();
    let dense: Vec<u32> = if use_bits { (0..nterm).filter(|&t| tlen(t) > 2 * nw).map(|t| t as u32).collect() } else { Vec::new() };
    let mut bslot = vec![u32::MAX; nterm];
    for (i, &t) in dense.iter().enumerate() { bslot[t as usize] = i as u32; }
    let mut bits = vec![0u64; dense.len() * nw];
    std::thread::scope(|sc| {
        let per = dense.len().div_ceil(nt).max(1);
        for (ch, ts) in bits.chunks_mut(per * nw).zip(dense.chunks(per)) {
            let parts = &parts;
            sc.spawn(move || for (b, &t) in ch.chunks_mut(nw).zip(ts) {
                for p in parts { for &d in dl(p, t) { b[d as usize >> 6] |= 1 << (d & 63); } }
            });
        }
    });
    let bs = |t: u32| { let i = bslot[t as usize]; (i != u32::MAX).then(|| &bits[i as usize * nw..(i as usize + 1) * nw]) };
    let res: Vec<std::sync::OnceLock<Vec<u8>>> = (0..qs.len()).map(|_| Default::default()).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|sc| {
        for _ in 0..nt {
            sc.spawn(|| {
            let (mut acc, mut tmp) = (Vec::<u64>::new(), Vec::<u64>::new());
            loop {
                let k = next.fetch_add(1, Ordering::Relaxed);
                if k >= qs.len() { break; }
                // parts cover disjoint, ascending doc ranges, so each query runs per part
                // and the per part answers just concatenate
                // only the count, the sum and the first ten ids are printed
                let (mut cnt, mut sum, mut r) = (0usize, 0u64, Vec::with_capacity(10));
                let mut add = |x: &[u32]| {
                    cnt += x.len();
                    sum += x.iter().map(|&d| d as u64).sum::<u64>();
                    for &d in x.iter().take(10 - r.len()) { r.push(d); }
                };
                let big = use_bits && match &qs[k] {
                    Q::Bool(v) => v.iter().map(|&(_, t)| parts.iter().map(|p| dl(p, t).len()).sum::<usize>()).sum::<usize>() > 2 * nw,
                    _ => false,
                };
                if big {
                    if acc.is_empty() { acc = vec![0; nw]; tmp = vec![0; nw]; }
                    let Q::Bool(v) = &qs[k] else { unreachable!() };
                    match bs(v[0].1) {
                        Some(b) => acc.copy_from_slice(b),
                        None => for p in &parts { for &d in dl(p, v[0].1) { acc[d as usize >> 6] |= 1 << (d & 63); } },
                    }
                    for &(op, t) in &v[1..] {
                        if let Some(b) = bs(t) {
                            match op {
                                1 => for (a, &x) in acc.iter_mut().zip(b) { *a &= x },
                                2 => for (a, &x) in acc.iter_mut().zip(b) { *a |= x },
                                _ => for (a, &x) in acc.iter_mut().zip(b) { *a &= !x },
                            }
                            continue;
                        }
                        match op {
                            1 => {
                                for p in &parts { for &d in dl(p, t) { if acc[d as usize >> 6] >> (d & 63) & 1 == 1 { tmp[d as usize >> 6] |= 1 << (d & 63); } } }
                                std::mem::swap(&mut acc, &mut tmp);
                                tmp.fill(0);
                            }
                            2 => for p in &parts { for &d in dl(p, t) { acc[d as usize >> 6] |= 1 << (d & 63); } },
                            _ => for p in &parts { for &d in dl(p, t) { acc[d as usize >> 6] &= !(1 << (d & 63)); } },
                        }
                    }
                    for (w, x) in acc.iter_mut().enumerate() {
                        let mut b = *x;
                        if b == 0 { continue; }
                        *x = 0;
                        let (mut one, mut m) = ([0u32; 64], 0);
                        while b != 0 { one[m] = (w * 64) as u32 + b.trailing_zeros(); m += 1; b &= b - 1; }
                        add(&one[..m]);
                    }
                }
                for p in parts.iter().filter(|_| !big) {
                    match &qs[k] {
                        Q::Bool(v) => {
                            let mut x = dl(p, v[0].1).to_vec();
                            for &(op, t) in &v[1..] {
                                x = match op { 1 => and(&x, dl(p, t)), 2 => or(&x, dl(p, t)), _ => not(&x, dl(p, t)) };
                            }
                            add(&x);
                        }
                        Q::Phrase(ts) => {
                            // docs holding the two rarest terms, then an int compare scan of each
                            let mut order: Vec<u32> = ts.clone();
                            order.sort_by_key(|&t| dl(p, t).len());
                            let n = ts.len();
                            let wide = ts.iter().any(|&t| t >= WIDE as u32);
                            let ts16: Vec<u16> = ts.iter().map(|&t| if t >= WIDE as u32 { WIDE } else { t as u16 }).collect();
                            let mut hit = and(dl(p, order[0]), dl(p, order[1]));
                            hit.retain(|&d| {
                                let i = (d - p.lo) as usize;
                                let toks = &p.stream[p.start[i] as usize..p.start[i + 1] as usize];
                                if !wide { return toks.windows(n).any(|w| w == &ts16[..]); }
                                // rare: a phrase id past u16, so retokenize this doc's text
                                if !toks.windows(n).any(|w| w == &ts16[..]) { return false; }
                                let full = retok(&docs[p.doff[i] as usize..], &lookup);
                                full.windows(n).any(|w| w == &ts[..])
                            });
                            add(&hit);
                        }
                    }
                }
                let mut o = Vec::new();
                write!(o, "{} {}", cnt, sum).unwrap();
                for x in r.iter().take(10) { write!(o, " {x}").unwrap(); }
                o.push(b'\n');
                res[k].set(o).ok();
            }});
        }
    });
    let mut o = Vec::new();
    for r in &res { o.extend_from_slice(r.get().unwrap()); }
    std::fs::create_dir_all(&out).ok();
    std::fs::write(format!("{out}/results.txt"), o).unwrap();
    std::process::exit(0);
}

// read-only mmap of an input file, so nothing is copied and nothing is charged to our heap
fn map(path: &str) -> &'static [u8] {
    use std::os::unix::io::AsRawFd;
    extern "C" { fn mmap(a: *mut u8, l: usize, p: i32, f: i32, fd: i32, o: i64) -> *mut u8; }
    let f = std::fs::File::open(path).unwrap();
    let len = f.metadata().unwrap().len() as usize;
    if len == 0 { return &[]; }
    let flags = 2 | if std::env::var("POP").is_ok() { 0x8000 } else { 0 }; // MAP_PRIVATE, MAP_POPULATE
    unsafe { std::slice::from_raw_parts(mmap(std::ptr::null_mut(), len, 1, flags, f.as_raw_fd(), 0), len) }
}
