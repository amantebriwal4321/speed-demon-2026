use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

type FxMap<K, V> = HashMap<K, V>;



// only the count, the sum and the first ten ids are printed
fn add(cnt: &mut usize, sum: &mut u64, r: &mut Vec<u32>, x: &[u32]) {
    *cnt += x.len();
    *sum += x.iter().map(|&d| d as u64).sum::<u64>();
    for &d in x.iter().take(10 - r.len()) { r.push(d); }
}

const BLK: usize = 1 << 21; // u16 entries per block, each block sized up front so it never grows

struct Part { lo: u32, lists: Vec<u32>, loff: Vec<u32>, doff: Vec<u32> }

enum Q { Bool(Vec<(u8, u32)>), Phrase((Vec<u32>, Vec<&'static [u8]>)) } // op: 0 first, 1 AND, 2 OR, 3 NOT

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
    // keep big blocks on their own mappings, so a freed pair block goes back to the kernel
    // (glibc would otherwise raise its mmap threshold and keep them in the heap)
    extern "C" { fn mallopt(p: i32, v: i32) -> i32; }
    unsafe { mallopt(-3, 1 << 20) }; // M_MMAP_THRESHOLD
    let data = std::env::var("DATA").unwrap_or("/data".into());
    let out = std::env::var("OUT").unwrap_or("/out".into());
    let qb = map(&format!("{data}/queries.txt"));

    // queries first: only the terms they use get indexed
    let mut ids: FxMap<&[u8], u32> = HashMap::new();
    let mut qs = Vec::new();
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
            let w: Vec<&[u8]> = inner.split(|&c| c == b' ').collect();
            qs.push(Q::Phrase((w.iter().map(|t| term(&mut ids, t)).collect(), w.clone())));
        }
    }
    let nterm = ids.len();
    let nall = nterm;
    // open addressing table from a token packed into a u128 (tokens of 16 bytes or less)
    // to its term id; longer query terms go to a small fallback map
    let cap = (nterm * 2).next_power_of_two().max(16);
    let shift = 128 - cap.trailing_zeros();
    let mut keys = vec![0u128; cap];
    let mut vals = vec![0u32; cap];
    let h = |k: u128| (((k as u64).wrapping_mul(0x9E3779B97F4A7C15) ^ ((k >> 64) as u64).wrapping_mul(0xC2B2AE3D27D4EB4F)) >> (shift - 64)) as usize;
    let mut long: FxMap<&[u8], u32> = HashMap::new();
    let hm = |k: u128| (k as u64).wrapping_mul(0x9E3779B97F4A7C15) ^ ((k >> 64) as u64).wrapping_mul(0xC2B2AE3D27D4EB4F);
    for (t, &id) in &ids {
        if t.len() > 16 { long.insert(t, id); continue; }
        let k = pack(t, t.len());
        let mut i = h(k);
        while keys[i] != 0 { i = (i + 1) & (cap - 1); }
        keys[i] = k; vals[i] = id;
    }

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
    // doc ids run in file order, so the last line gives the bitset size up front
    let maxdoc = { let t = docs.len().saturating_sub(1); let s0 = docs[..t].iter().rposition(|&c| c == b'\n').map_or(0, |x| x + 1);
        let mut v = 0usize; for &c in &docs[s0..] { if c == b'\t' { break; } v = v * 10 + (c - b'0') as usize; } v };
    let nw = maxdoc / 64 + 1;
    let use_bits = maxdoc < 1 << 26 && docs.len() > 0;
    // a 4 MB sample of the docs picks the common terms (over 1 doc in 32); those never get a
    // list: the scan marks them straight into a bitset, so their postings are never held
    let mut bslot = vec![u32::MAX; nall];
    let mut nd = 0usize;
    if use_bits {
        let mut e = docs.len().min(1 << 18);
        while e < docs.len() && docs[e - 1] != b'\n' { e += 1; }
        let (mut seen, mut c) = (vec![u32::MAX; nall], vec![0u32; nall]);
        let (mut i, mut nd0) = (0usize, 0u32);
        while i < e {
            while docs[i] != b'\t' { i += 1; }
            nd0 += 1;
            loop {
                while lower[docs[i] as usize] == 0 && docs[i] != b'\n' { i += 1; }
                if docs[i] == b'\n' { i += 1; break; }
                let s0 = i;
                while lower[docs[i] as usize] != 0 { i += 1; }
                if i - s0 > 16 { continue; }
                let kk = pack(&docs[s0..], i - s0);
                let mut j = h(kk);
                let t = loop { let x = keys[j]; if x == kk { break Some(vals[j]); } if x == 0 { break None; } j = (j + 1) & (cap - 1); };
                if let Some(t) = t { if seen[t as usize] != nd0 { seen[t as usize] = nd0; c[t as usize] += 1; } }
            }
        }
        // a margin under the 1 in 32 line, since the sample is small
        for t in 0..nall { if c[t] as u64 * 48 > nd0 as u64 { bslot[t] = nd as u32; nd += 1; } }
    }
    let abits: Vec<std::sync::atomic::AtomicU64> = (0..nd * nw).map(|_| std::sync::atomic::AtomicU64::new(0)).collect();
    // a common term's table value becomes its bitset slot with the top bit set, so the scan
    // marks it without touching last[] or a side table
    for j in 0..cap { if keys[j] != 0 && bslot[vals[j] as usize] != u32::MAX { vals[j] = 1 << 31 | bslot[vals[j] as usize]; } }
    for v in long.values_mut() { if bslot[*v as usize] != u32::MAX { *v = 1 << 31 | bslot[*v as usize]; } }
    let scan: Vec<(Part, Vec<Vec<u16>>, Vec<u32>)> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..nt).map(|k| {
            let c = &docs[cuts[k]..cuts[k + 1]];
            let base = cuts[k];
            let (keys, vals, long, lower, abits) = (&keys, &vals, &long, &lower, &abits);
            sc.spawn(move || {
                let mut p = Part { lo: u32::MAX, lists: Vec::new(), loff: Vec::new(), doff: Vec::new() };
                let mut last = vec![u32::MAX; nall];
                let mut cnt = vec![0u32; nall + 1];
                let (mut blocks, mut blk) = (Vec::new(), Vec::<u16>::with_capacity(BLK + 4096));
                let (mut acc, mut aw) = (vec![0u64; nd], 0usize);
                let mut i = 0;
                while i < c.len() {
                    let mut doc = 0u32;
                    while c[i] != b'\t' { doc = doc * 10 + (c[i] - b'0') as u32; i += 1; }
                    if p.lo == u32::MAX { p.lo = doc; }
                    // (doc, term) pairs go in blocks of whole docs, a MAX before each doc, so
                    // the fill below can free each block as it goes and the peak stays low
                    if blk.len() >= BLK { blocks.push(std::mem::replace(&mut blk, Vec::with_capacity(BLK + 4096))); }
                    blk.push(u16::MAX);
                    p.doff.push((base + i) as u32);
                    let d = p.doff.len() as u32 - 1;
                    // common terms gather 64 docs of bits locally, then one OR per word goes out
                    let g = (p.lo + d) as usize >> 6;
                    if g != aw { for (x, a) in acc.iter_mut().enumerate() { if *a != 0 { abits[x * nw + aw].fetch_or(std::mem::take(a), Ordering::Relaxed); } } aw = g; }
                    i += 1;
                    macro_rules! handle { ($id:expr) => {{ let id: Option<u32> = $id;
                        if let Some(t) = id {
                            if t >> 31 == 1 { acc[(t & 0x7FFF_FFFF) as usize] |= 1 << ((p.lo + d) & 63); } else if last[t as usize] != d { last[t as usize] = d; { put(&mut blk, t); cnt[t as usize + 1] += 1; } }
                        } }} }
                    let lookup = |p: (u8, u128, usize, Option<u32>)| -> Option<u32> {
                        if p.0 == 2 { return p.3; }
                        let (k, mut j) = (p.1, p.2);
                        loop { let x = keys[j]; if x == k { break Some(vals[j]); } if x == 0 { break None; } j = (j + 1) & (cap - 1); }
                    };
                    let mut pend: (u8, u128, usize, Option<u32>) = (0, 0, 0, None);
                    // token and LF bitmasks of a 64 byte window, then walk the bits
                    let mut o = 64; // offset inside the current window, 64 = need a new one
                    let (mut tm, mut nm) = (0u64, 0u64);
                    loop {
                        if o >= 64 { (tm, nm) = masks(docs, base + i); o = 0; }
                        let bits = (tm | nm) >> o;
                        if bits == 0 { i += 64 - o; o = 64; continue; }
                        let z = bits.trailing_zeros() as usize;
                        i += z; o += z;
                        if nm >> o & 1 == 1 { if pend.0 != 0 { handle!(lookup(pend)); pend.0 = 0; } break; }
                        let s = i;
                        let run = (!(tm >> o)).trailing_zeros() as usize; // 64 when the run fills the window
                        i += run; o += run;
                        if o >= 64 { while lower[c[i] as usize] != 0 { i += 1; } o = 64; }
                        let l = i - s;
                        // a short token's slot is prefetched and looked up one token later, so the
                        // table miss overlaps the scan of the next token
                        let nx: (u8, u128, usize, Option<u32>) = if l <= 16 {
                            let k = pack(&docs[base + s..], l);
                            let j = (hm(k) >> (shift - 64)) as usize;
                            unsafe { std::arch::x86_64::_mm_prefetch(keys.as_ptr().add(j) as *const i8, std::arch::x86_64::_MM_HINT_T0); }
                            (1, k, j, None)
                        } else if l <= 32 {
                            let mut t = [0u8; 32];
                            for (a, &b) in t.iter_mut().zip(&c[s..i]) { *a = lower[b as usize]; }
                            (2, 0, 0, long.get(&t[..l]).copied())
                        } else { (2, 0, 0, None) };
                        if pend.0 != 0 { handle!(lookup(pend)); }
                        pend = nx;
                    }
                    i += 1;
                }
                for (x, a) in acc.iter_mut().enumerate() { if *a != 0 { abits[x * nw + aw].fetch_or(*a, Ordering::Relaxed); } }
                blocks.push(blk);
                (p, blocks, cnt)
            })
        }).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    // terms in more than 1 doc in 32 go straight into a bitset of all docs and get no list,
    // which leaves out nearly half the postings
    let parts: Vec<Part> = std::thread::scope(|sc| {
        let hs: Vec<_> = scan.into_iter().map(|(mut p, blocks, mut cnt)| {
            sc.spawn(move || {
                for t in 0..nall { cnt[t + 1] += cnt[t]; }
                let mut cur = cnt.clone();
                let mut lists = vec![0u32; cnt[nall] as usize];
                let mut d = p.lo.wrapping_sub(1);
                for b in blocks {
                    let mut q = 0;
                    while q < b.len() {
                        let mut t = b[q] as u32; q += 1;
                        if t == 0xFFFF { d = d.wrapping_add(1); continue; }
                        if t == 0xFFFE { t = (b[q] as u32) << 16 | b[q + 1] as u32; q += 2; }
                        lists[cur[t as usize] as usize] = d;
                        cur[t as usize] += 1;
                    }
                }
                p.lists = lists; p.loff = cnt;
                p
            })
        }).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });

    fn dl(p: &Part, t: u32) -> &[u32] { &p.lists[p.loff[t as usize] as usize..p.loff[t as usize + 1] as usize] }

    // BOOL queries over long lists run on a bitset of all docs: OR sets bits, NOT clears
    // them, AND keeps the list's docs whose bit is set. Ids come out ascending for free.
    let bits: Vec<u64> = abits.into_iter().map(|a| a.into_inner()).collect();
    let tlen = |t: usize| if bslot[t] != u32::MAX { 4 * nw } else { parts.iter().map(|p| dl(p, t as u32).len()).sum::<usize>() };
    let bs = |t: u32| { let i = bslot[t as usize]; (i != u32::MAX).then(|| &bits[i as usize * nw..(i as usize + 1) * nw]) };
    let psum: Vec<u64> = (0..256u32).map(|b| (0..8).filter(|i| b >> i & 1 == 1).sum::<u32>() as u64).collect();
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
                let big = use_bits && match &qs[k] {
                    Q::Bool(v) => v.iter().map(|&(_, t)| tlen(t as usize)).sum::<usize>() > 2 * nw,
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
                    // count and sum word by word (a byte table gives the sum of bit positions);
                    // only the first ten ids are pulled out one by one
                    for (w, x) in acc.iter_mut().enumerate() {
                        let mut b = std::mem::take(x);
                        if b == 0 { continue; }
                        cnt += b.count_ones() as usize;
                        sum += b.count_ones() as u64 * (w as u64 * 64) + (0..8).map(|j| psum[(b >> (8 * j)) as u8 as usize] + 8 * j as u64 * ((b >> (8 * j)) as u8).count_ones() as u64).sum::<u64>();
                        while b != 0 && r.len() < 10 { r.push((w * 64) as u32 + b.trailing_zeros()); b &= b - 1; }
                    }
                }
                for p in parts.iter().filter(|_| !big) {
                    match &qs[k] {
                        Q::Bool(v) => {
                            let mut x = dl(p, v[0].1).to_vec();
                            for &(op, t) in &v[1..] {
                                x = match op { 1 => and(&x, dl(p, t)), 2 => or(&x, dl(p, t)), _ => not(&x, dl(p, t)) };
                            }
                            add(&mut cnt, &mut sum, &mut r, &x);
                        }
                        Q::Phrase((ts, ks)) => {
                            // docs holding every term, then the doc text decides
                            let mut order: Vec<u32> = ts.clone(); order.sort_unstable(); order.dedup();
                            let (dn, ld): (Vec<u32>, Vec<u32>) = order.iter().partition(|&&t| bslot[t as usize] != u32::MAX);
                            let mut ld = ld; ld.sort_by_key(|&t| dl(p, t).len());
                            let mut hit = if let Some(&t0) = ld.first() { dl(p, t0).to_vec() } else {
                                let hi = p.lo as usize + p.doff.len();
                                (p.lo as usize..hi).filter(|&d| { let d = d as u32; dn.iter().all(|&t| bs(t).unwrap()[d as usize >> 6] >> (d & 63) & 1 == 1) }).map(|d| d as u32).collect()
                            };
                            for &t in ld.iter().skip(1) { if hit.is_empty() { break; } hit = and(&hit, dl(p, t)); }
                            if !ld.is_empty() { hit.retain(|&d| dn.iter().all(|&t| bs(t).unwrap()[d as usize >> 6] >> (d & 63) & 1 == 1)); }
                            if ts.len() > 1 { hit.retain(|&d| has2(docs, p.doff[(d - p.lo) as usize] as usize, ks, &lower)); }
                            add(&mut cnt, &mut sum, &mut r, &hit);
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
    let flags = 2; // MAP_PRIVATE
    unsafe { std::slice::from_raw_parts(mmap(std::ptr::null_mut(), len, 1, flags, f.as_raw_fd(), 0), len) }
}

// a block entry: a term id below 0xFFFE as one u16, a larger one as 0xFFFE then two halves;
// 0xFFFF starts a doc. Most postings are common terms, which the queries name early.
#[inline]
fn put(b: &mut Vec<u16>, t: u32) {
    if t < 0xFFFE { b.push(t as u16) } else { b.extend_from_slice(&[0xFFFE, (t >> 16) as u16, t as u16]) }
}

// phrase check on the doc text: find the first word's first byte 16 at a time (case folded),
// then match the words exactly as whole tokens through the lowercase table
fn has2(docs: &[u8], start: usize, ks: &[&[u8]], lower: &[u8; 256]) -> bool {
    use std::arch::x86_64::*;
    let w0 = ks[0];
    let at = |j: usize| -> bool {
        let mut j = j;
        for (n, &k) in ks.iter().enumerate() {
            let (w, l) = (k, k.len());
            for m in 0..l { if lower[docs[j + m] as usize] != w[m] { return false; } }
            j += l;
            if lower[docs[j] as usize] != 0 { return false; }
            if n + 1 < ks.len() { while lower[docs[j] as usize] == 0 { if docs[j] == b'\n' { return false; } j += 1; } }
        }
        true
    };
    let mut i = start + 1;
    unsafe {
        let (f, lf, fold) = (_mm_set1_epi8(w0[0] as i8), _mm_set1_epi8(b'\n' as i8), _mm_set1_epi8(0x20));
        loop {
            if i + 16 > docs.len() {
                while i < docs.len() && docs[i] != b'\n' { if docs[i] | 0x20 == w0[0] && lower[docs[i - 1] as usize] == 0 && at(i) { return true; } i += 1; }
                return false;
            }
            let x = _mm_loadu_si128(docs.as_ptr().add(i) as *const __m128i);
            let mut m = _mm_movemask_epi8(_mm_cmpeq_epi8(_mm_or_si128(x, fold), f)) as u32;
            let e = _mm_movemask_epi8(_mm_cmpeq_epi8(x, lf)) as u32;
            if e != 0 { m &= (1 << e.trailing_zeros()) - 1; }
            while m != 0 {
                let j = i + m.trailing_zeros() as usize;
                m &= m - 1;
                if lower[docs[j - 1] as usize] == 0 && at(j) { return true; }
            }
            if e != 0 { return false; }
            i += 16;
        }
    }
}
