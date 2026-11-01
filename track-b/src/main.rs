use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};




// only the count, the sum and the first ten ids are printed
fn add(cnt: &mut usize, sum: &mut u64, r: &mut Vec<u32>, x: &[u16], lo: u32) {
    *cnt += x.len();
    *sum += x.iter().map(|&d| d as u64).sum::<u64>() + lo as u64 * x.len() as u64;
    for &d in x.iter().take(10 - r.len()) { r.push(lo + d as u32); }
}

const BLK: usize = 1 << 21; // u16 entries per block, each block sized up front so it never grows

struct Part { lo: u32, lists: Vec<u16>, loff: Vec<u32>, doff: Vec<u32> }

enum Q<'a> { Bool(&'a [(u8, u32)]), Phrase(&'a [u32], &'a [&'static [u8]]) } // op: 0 first, 1 AND, 2 OR, 3 NOT

// does sorted b contain x, searching forward from *j (galloping); moves *j along
fn seek(b: &[u16], j: &mut usize, x: u16) -> bool {
    let mut step = 1;
    while *j + step < b.len() && b[*j + step] < x { *j += step; step *= 2; }
    let hi = (*j + step + 1).min(b.len());
    *j += b[*j..hi].partition_point(|&y| y < x);
    *j < b.len() && b[*j] == x
}
// the list ops write into o, a buffer the caller reuses from query to query
fn and(a: &[u16], b: &[u16], o: &mut Vec<u16>) {
    o.clear();
    let (a, b) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    if a.len() * 8 < b.len() {
        let mut j = 0;
        return o.extend(a.iter().copied().filter(|&x| seek(b, &mut j, x)));
    }
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] < b[j] { i += 1 } else if a[i] > b[j] { j += 1 } else { o.push(a[i]); i += 1; j += 1; }
    }
}
fn or(a: &[u16], b: &[u16], o: &mut Vec<u16>) {
    let (mut i, mut j) = (0, 0);
    o.clear();
    while i < a.len() && j < b.len() {
        if a[i] < b[j] { o.push(a[i]); i += 1 } else if a[i] > b[j] { o.push(b[j]); j += 1 } else { o.push(a[i]); i += 1; j += 1; }
    }
    o.extend_from_slice(&a[i..]); o.extend_from_slice(&b[j..]);
}
fn not(a: &[u16], b: &[u16], o: &mut Vec<u16>) {
    let mut j = 0;
    o.clear();
    o.extend(a.iter().copied().filter(|&x| !seek(b, &mut j, x)));
}

// a token of up to 16 bytes, lowercased, packed little endian into a u128 (zero padded).
// Token bytes are ASCII letters and digits, and setting bit 5 lowercases all of them.
#[inline]
fn pack(b: &[u8], l: usize) -> u128 {
    let mut x = [0u8; 16];
    if b.len() >= 16 { x.copy_from_slice(&b[..16]) } else { x[..b.len()].copy_from_slice(b) }
    let m = if l >= 16 { u128::MAX } else { (1u128 << (8 * l)) - 1 };
    (u128::from_le_bytes(x) | 0x2020_2020_2020_2020_2020_2020_2020_2020) & m
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

fn hm(k: u128) -> u64 { (k as u64).wrapping_mul(0x9E3779B97F4A7C15) ^ ((k >> 64) as u64).wrapping_mul(0xC2B2AE3D27D4EB4F) }
// query terms go straight into the scan's table: open addressing on the packed token, grown
// at half full; terms over 16 bytes go to a small map. Ids are handed out in first-seen order.
struct Tab<'a> { keys: Vec<u128>, vals: Vec<u32>, n: u32, long: HashMap<&'a [u8], u32> }
impl<'a> Tab<'a> {
    fn put(keys: &mut [u128], vals: &mut [u32], k: u128, v: u32) -> u32 {
        let (m, sh) = (keys.len() - 1, 64 - keys.len().trailing_zeros());
        let mut i = (hm(k) >> sh) as usize;
        loop { if keys[i] == k { return vals[i]; } if keys[i] == 0 { keys[i] = k; vals[i] = v; return v; } i = (i + 1) & m; }
    }
    fn term(&mut self, t: &'a [u8]) -> u32 {
        let n = self.n;
        let id = if t.len() > 16 { *self.long.entry(t).or_insert(n) } else { Self::put(&mut self.keys, &mut self.vals, pack(t, t.len()), n) };
        if id == n { self.n += 1; }
        if self.n as usize * 2 > self.keys.len() {
            let c = self.keys.len() * 2;
            let (ok, ov) = (std::mem::replace(&mut self.keys, vec![0; c]), std::mem::replace(&mut self.vals, vec![0; c]));
            for (k, v) in ok.into_iter().zip(ov) { if k != 0 { Self::put(&mut self.keys, &mut self.vals, k, v); } }
        }
        id
    }
}

fn main() {
    // keep big blocks on their own mappings, so a freed posting block goes back to the kernel
    // (glibc would otherwise raise its mmap threshold and keep them in the heap)
    extern "C" { fn mallopt(p: i32, v: i32) -> i32; }
    unsafe { mallopt(-3, 1 << 20) }; // M_MMAP_THRESHOLD
    let data = std::env::var("DATA").unwrap_or("/data".into());
    let out = std::env::var("OUT").unwrap_or("/out".into());
    let qb = map(&format!("{data}/queries.txt"));

    // queries first: only the terms they use get indexed
    let mut tab = Tab { keys: vec![0; 1 << 12], vals: vec![0; 1 << 12], n: 0, long: HashMap::new() };
    // the queries, flat: a BOOL query is a range of (op, term) pairs in `ops`, a phrase a range
    // of terms in `pt` with its words in `pw`, so reading them allocates nothing per query
    let (mut ops, mut pt, mut pw, mut qr) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for line in qb.split(|&c| c == b'\n') {
        if line.is_empty() { continue; }
        if line.starts_with(b"BOOL ") {
            let (a, mut w) = (ops.len(), line[5..].split(|&c| c == b' '));
            ops.push((0u8, tab.term(w.next().unwrap())));
            while let (Some(op), Some(t)) = (w.next(), w.next()) {
                ops.push((match op { b"AND" => 1, b"OR" => 2, _ => 3 }, tab.term(t)));
            }
            qr.push((true, a, ops.len()));
        } else {
            let a = pt.len();
            for t in line[8..line.len() - 1].split(|&c| c == b' ') { pt.push(tab.term(t)); pw.push(t); }
            qr.push((false, a, pt.len()));
        }
    }
    let nterm = tab.n as usize;
    let nall = nterm;
    let Tab { keys, mut vals, mut long, .. } = tab;
    let cap = keys.len();
    let shift = 128 - cap.trailing_zeros();
    let h = |k: u128| (hm(k) >> (shift - 64)) as usize;
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

    // doc ids run in file order, so the last line gives the bitset size up front
    let maxdoc = { let t = docs.len().saturating_sub(1); let s0 = docs[..t].iter().rposition(|&c| c == b'\n').map_or(0, |x| x + 1);
        let mut v = 0usize; for &c in &docs[s0..] { if c == b'\t' { break; } v = v * 10 + (c - b'0') as usize; } v };
    let nw = maxdoc / 64 + 1;
    let use_bits = maxdoc < 1 << 26 && docs.len() > 0;
    // a 256 KB sample of the docs counts each term's docs. Term ids are renumbered by that
    // count, most common first, so the counters the scan touches most sit together. The
    // first nd terms (over 1 doc in 24 of the sample) never get a list: the scan marks them straight into
    // a bitset, so their postings are never held.
    let mut c = vec![0u32; nall];
    let mut nd = 0usize;
    if use_bits {
        let mut e = docs.len().min(1 << 18);
        while e < docs.len() && docs[e - 1] != b'\n' { e += 1; }
        let mut seen = vec![u32::MAX; nall];
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
        // at the fill peak a bitset (31 KB) costs less than a list from about here on
        nd = c.iter().filter(|&&x| x as u64 * 24 > nd0 as u64).count();
    }
    let mut order: Vec<u32> = (0..nall as u32).collect();
    order.sort_by_key(|&t| std::cmp::Reverse(c[t as usize]));
    let mut new = vec![0u32; nall];
    for (i, &t) in order.iter().enumerate() { new[t as usize] = i as u32; }
    for j in 0..cap { if keys[j] != 0 { vals[j] = new[vals[j] as usize]; } }
    for v in long.values_mut() { *v = new[*v as usize]; }
    for e in ops.iter_mut() { e.1 = new[e.1 as usize]; }
    for t in pt.iter_mut() { *t = new[*t as usize]; }
    let qs: Vec<Q> = qr.iter().map(|&(b, a, e)| if b { Q::Bool(&ops[a..e]) } else { Q::Phrase(&pt[a..e], &pw[a..e]) }).collect();
    let abits: Vec<std::sync::atomic::AtomicU64> = zeroed(nd * nw);
    // the scan's tables: tokens of 8 bytes or less in one, 9 to 16 bytes in the other, so a
    // slot holds the whole token and its value and a lookup is one access
    let short = Probe::<2>::new(keys.iter().zip(&vals).filter(|e| *e.0 != 0 && *e.0 >> 64 == 0).map(|(&k, &v)| (k, v)));
    let longer = Probe::<4>::new(keys.iter().zip(&vals).filter(|e| *e.0 >> 64 != 0).map(|(&k, &v)| (k, v)));
    drop((keys, vals));
    let scan: Vec<Vec<(Part, Vec<Vec<u16>>, Vec<u32>)>> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..nt).map(|k| {
            let c = &docs[cuts[k]..cuts[k + 1]];
            let base = cuts[k];
            let (short, longer, long, lower, abits) = (&short, &longer, &long, &lower, &abits);
            sc.spawn(move || {
                let mut p = Part { lo: u32::MAX, lists: Vec::new(), loff: Vec::new(), doff: Vec::new() };
                // per term: the last doc that listed it and its list length so far, side by
                // side so a posting touches one cache line
                let mut lc = vec![[u16::MAX, 0]; nall];
                let counts = |lc: &mut Vec<[u16; 2]>| -> Vec<u32> { std::iter::once(0).chain(lc.iter_mut().map(|e| std::mem::replace(e, [u16::MAX, 0])[1] as u32)).collect() };
                let (mut blocks, mut blk) = (Vec::new(), Vec::<u16>::with_capacity(BLK + 4096));
                let (mut acc, mut aw, mut fw) = (vec![0u64; nd], usize::MAX, 0);
                let mut i = 0;
                // lists hold doc ids local to their part as u16, so a part closes at 65,535 docs
                // (a doc count then fits in u16 too, and u16::MAX means "no doc yet")
                let mut done = Vec::new();
                while i < c.len() {
                    let mut doc = 0u32;
                    while c[i] != b'\t' { doc = doc * 10 + (c[i] - b'0') as u32; i += 1; }
                    if p.doff.len() == 0xFFFF {
                        blocks.push(std::mem::replace(&mut blk, Vec::with_capacity(BLK + 4096)));
                        let q = std::mem::replace(&mut p, Part { lo: u32::MAX, lists: Vec::new(), loff: Vec::new(), doff: Vec::new() });
                        done.push((q, std::mem::take(&mut blocks), counts(&mut lc)));
                    }
                    if p.lo == u32::MAX { p.lo = doc; }
                    // (doc, term) pairs go in blocks of whole docs, a MAX before each doc, so
                    // the fill below can free each block as it goes and the peak stays low
                    if blk.len() >= BLK { blocks.push(std::mem::replace(&mut blk, Vec::with_capacity(BLK + 4096))); }
                    blk.push(u16::MAX);
                    p.doff.push((base + i) as u32);
                    let d = p.doff.len() as u32 - 1;
                    // common terms gather 64 docs of bits locally, then each word goes out at once;
                    // words between this thread's first and last hold only its docs, so a plain
                    // store does, and only the first word (the last is below) needs an atomic OR
                    let g = (p.lo + d) as usize >> 6;
                    if g != aw {
                        if aw == usize::MAX { fw = g; }
                        for (x, a) in acc.iter_mut().enumerate().filter(|a| *a.1 != 0) {
                            if aw == fw { abits[x * nw + aw].fetch_or(std::mem::take(a), Ordering::Relaxed); } else { abits[x * nw + aw].store(std::mem::take(a), Ordering::Relaxed); }
                        }
                        aw = g;
                    }
                    i += 1;
                    macro_rules! handle { ($id:expr) => {{ let id: Option<u32> = $id;
                        if let Some(t) = id {
                            if (t as usize) < nd { acc[t as usize] |= 1 << ((p.lo + d) & 63); } else if lc[t as usize][0] != d as u16 { lc[t as usize] = [d as u16, lc[t as usize][1] + 1]; put(&mut blk, t); }
                        } }} }
                    let lookup = |p: (u8, u128, usize, Option<u32>)| match p.0 { 1 => short.get(p.1, p.2), 3 => longer.get(p.1, p.2), _ => p.3 };
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
                        if nm >> o & 1 == 1 { if pend.0 != 0 { handle!(lookup(pend)); } break; }
                        let s = i;
                        let run = (!(tm >> o)).trailing_zeros() as usize; // 64 when the run fills the window
                        i += run; o += run;
                        if o >= 64 { while lower[c[i] as usize] != 0 { i += 1; } o = 64; }
                        let l = i - s;
                        // a short token's slot is prefetched and looked up one token later, so the
                        // table miss overlaps the scan of the next token
                        let nx: (u8, u128, usize, Option<u32>) = if l <= 8 {
                            let k = pack(&docs[base + s..], l);
                            (1, k, short.at(k), None)
                        } else if l <= 16 {
                            let k = pack(&docs[base + s..], l);
                            (3, k, longer.at(k), None)
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
                done.push((p, blocks, counts(&mut lc)));
                done
            })
        }).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    drop((short, longer));
    // the common terms are already in bitsets of all docs and get no list, which leaves out
    // nearly half the postings
    // the fill runs in two waves: a part's blocks and its lists are both alive while it fills,
    // so filling half the parts at a time keeps half the lists off the peak
    let mut todo: Vec<_> = scan.into_iter().flatten().collect();
    let wave = todo.len().div_ceil(2);
    let mut parts: Vec<Part> = Vec::new();
    while !todo.is_empty() {
    let now: Vec<_> = todo.drain(..wave.min(todo.len())).collect();
    parts.extend(std::thread::scope(|sc| {
        let hs: Vec<_> = now.into_iter().map(|(mut p, blocks, mut cnt)| {
            sc.spawn(move || {
                for t in 0..nall { cnt[t + 1] += cnt[t]; }
                let mut lists = vec![0u16; cnt[nall] as usize];
                let mut d = u16::MAX;
                for b in blocks {
                    let mut q = 0;
                    while q < b.len() {
                        let mut t = b[q] as u32; q += 1;
                        if t == 0xFFFF { d = d.wrapping_add(1); continue; }
                        if t == 0xFFFE { t = (b[q] as u32) << 16 | b[q + 1] as u32; q += 2; }
                        lists[cnt[t as usize] as usize] = d;
                        cnt[t as usize] += 1;
                    }
                }
                // each start was moved to its term's end, the next term's start: shift back
                cnt.rotate_right(1);
                cnt[0] = 0;
                p.lists = lists; p.loff = cnt;
                p
            })
        }).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect::<Vec<_>>()
    }));
    }
    fn dl(p: &Part, t: u32) -> &[u16] { &p.lists[p.loff[t as usize] as usize..p.loff[t as usize + 1] as usize] }

    // BOOL queries over long lists run on a bitset of all docs: OR sets bits, NOT clears
    // them, AND keeps the list's docs whose bit is set. Ids come out ascending for free.
    let bits: Vec<u64> = abits.into_iter().map(|a| a.into_inner()).collect();
    let tlen = |t: usize| if t < nd { 4 * nw } else { parts.iter().map(|p| dl(p, t as u32).len()).sum::<usize>() };
    let bs = |t: u32| { let t = t as usize; (t < nd).then(|| &bits[t * nw..(t + 1) * nw]) };
    let res: Vec<std::sync::OnceLock<Vec<u8>>> = (0..qs.len()).map(|_| Default::default()).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|sc| {
        for _ in 0..nt {
            sc.spawn(|| {
            let (mut acc, mut tmp) = (Vec::<u64>::new(), Vec::<u64>::new());
            let (mut x, mut y) = (Vec::<u16>::new(), Vec::<u16>::new());
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
                        None => for p in &parts { for d in dl(p, v[0].1).iter().map(|&x| p.lo + x as u32) { acc[d as usize >> 6] |= 1 << (d & 63); } },
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
                                for p in &parts { for d in dl(p, t).iter().map(|&x| p.lo + x as u32) { if acc[d as usize >> 6] >> (d & 63) & 1 == 1 { tmp[d as usize >> 6] |= 1 << (d & 63); } } }
                                std::mem::swap(&mut acc, &mut tmp);
                                tmp.fill(0);
                            }
                            2 => for p in &parts { for d in dl(p, t).iter().map(|&x| p.lo + x as u32) { acc[d as usize >> 6] |= 1 << (d & 63); } },
                            _ => for p in &parts { for d in dl(p, t).iter().map(|&x| p.lo + x as u32) { acc[d as usize >> 6] &= !(1 << (d & 63)); } },
                        }
                    }
                    // count and sum word by word: bit k of a position is set on the positions
                    // in mask k, so the positions of a word sum to the popcounts under the masks
                    // shifted by k; only the first ten ids are pulled out one by one
                    const M: [u64; 6] = [0xAAAA_AAAA_AAAA_AAAA, 0xCCCC_CCCC_CCCC_CCCC, 0xF0F0_F0F0_F0F0_F0F0, 0xFF00_FF00_FF00_FF00, 0xFFFF_0000_FFFF_0000, 0xFFFF_FFFF_0000_0000];
                    for w in 0..nw {
                        let mut b = std::mem::take(&mut acc[w]);
                        if b == 0 { continue; }
                        cnt += b.count_ones() as usize;
                        sum += (b.count_ones() as usize * w * 64) as u64 + (0..6).map(|k| ((b & M[k]).count_ones() as u64) << k).sum::<u64>();
                        while b != 0 && r.len() < 10 { r.push((w * 64) as u32 + b.trailing_zeros()); b &= b - 1; }
                    }
                }
                // a phrase's terms are sorted out once, not per part: the rarest word anchors the
                // text check, the common ones are tested on their bitsets, the rest are ANDed
                // shortest first
                let (mut anchor, mut dn, mut ld) = (0, Vec::new(), Vec::new());
                if let Q::Phrase(ts, ks) = &qs[k] {
                    anchor = (0..ts.len()).min_by_key(|&m| (tlen(ts[m] as usize), usize::MAX - ks[m].len())).unwrap();
                    let mut order: Vec<u32> = ts.to_vec();
                    order.sort_unstable();
                    order.dedup();
                    (dn, ld) = order.into_iter().partition(|&t| (t as usize) < nd);
                    ld.sort_by_key(|&t| tlen(t as usize));
                }
                for p in parts.iter().filter(|_| !big) {
                    match &qs[k] {
                        Q::Bool(v) => {
                            x.clear();
                            x.extend_from_slice(dl(p, v[0].1));
                            for &(op, t) in &v[1..] {
                                match op { 1 => and(&x, dl(p, t), &mut y), 2 => or(&x, dl(p, t), &mut y), _ => not(&x, dl(p, t), &mut y) }
                                std::mem::swap(&mut x, &mut y);
                            }
                            add(&mut cnt, &mut sum, &mut r, &x, p.lo);
                        }
                        Q::Phrase(ts, ks) => {
                            // docs holding every term, then the doc text decides
                            let hit = &mut x;
                            hit.clear();
                            match ld.first() {
                                Some(&t0) => hit.extend_from_slice(dl(p, t0)),
                                None => hit.extend((0..p.doff.len() as u32).filter(|&x| { let d = p.lo + x; dn.iter().all(|&t| bs(t).unwrap()[d as usize >> 6] >> (d & 63) & 1 == 1) }).map(|x| x as u16)),
                            }
                            for &t in ld.iter().skip(1) { if hit.is_empty() { break; } and(hit, dl(p, t), &mut y); std::mem::swap(hit, &mut y); }
                            if !ld.is_empty() { hit.retain(|&x| { let d = p.lo + x as u32; dn.iter().all(|&t| bs(t).unwrap()[d as usize >> 6] >> (d & 63) & 1 == 1) }); }
                            if ts.len() > 1 { hit.retain(|&d| has2(docs, p.doff[d as usize] as usize, ks, anchor, &lower)); }
                            add(&mut cnt, &mut sum, &mut r, &hit, p.lo);
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

// open addressing on a packed token: W = 2 holds tokens of up to 8 bytes as [bytes, value],
// W = 4 longer ones as [low bytes, high bytes, value, 0]. The slots come zeroed and page
// aligned from mmap, so none straddles a cache line, and are asked for 2 MB pages, so
// random probes don't miss the TLB.
struct Probe<const W: usize> { s: &'static mut [[u64; W]], sh: u32 }
// unmapped as soon as the scan is done, so the tables are gone before the fill's peak
impl<const W: usize> Drop for Probe<W> {
    fn drop(&mut self) {
        extern "C" { fn munmap(a: *mut u8, l: usize) -> i32; }
        unsafe { munmap(self.s.as_mut_ptr() as *mut u8, self.s.len() * 8 * W) };
    }
}
impl<const W: usize> Probe<W> {
    fn new(items: impl Iterator<Item = (u128, u32)> + Clone) -> Self {
        extern "C" { fn mmap(a: *mut u8, l: usize, p: i32, f: i32, fd: i32, o: i64) -> *mut u8; fn madvise(a: *mut u8, l: usize, advice: i32) -> i32; }
        let cap = (2 * items.clone().count()).next_power_of_two().max(16);
        let s = unsafe {
            let a = mmap(std::ptr::null_mut(), cap * 8 * W, 3, 0x22, -1, 0); // read/write, private anonymous
            madvise(a, cap * 8 * W, 14); // MADV_HUGEPAGE
            std::slice::from_raw_parts_mut(a as *mut [u64; W], cap)
        };
        let p = Probe { s, sh: 64 - cap.trailing_zeros() };
        for (k, v) in items {
            let mut j = p.at(k);
            while p.s[j][0] != 0 { j = (j + 1) & (cap - 1); }
            p.s[j][0] = k as u64;
            if W == 4 { p.s[j][1] = (k >> 64) as u64; }
            p.s[j][W / 2] = v as u64;
        }
        p
    }
    // the home slot, prefetched: the lookup comes one token later
    fn at(&self, k: u128) -> usize {
        let j = (hm(k) >> self.sh) as usize;
        unsafe { std::arch::x86_64::_mm_prefetch(self.s.as_ptr().add(j) as *const i8, std::arch::x86_64::_MM_HINT_T0) };
        j
    }
    fn get(&self, k: u128, mut j: usize) -> Option<u32> {
        loop {
            let e = &self.s[j];
            if e[0] == 0 { return None; }
            if e[0] == k as u64 && (W == 2 || e[1] == (k >> 64) as u64) { return Some(e[W / 2] as u32); }
            j = (j + 1) & (self.s.len() - 1);
        }
    }
}

// n zeroed atomics straight from calloc: the kernel hands out zero pages, so nothing is
// written up front on one thread, and the pages fault in on the threads that use them
fn zeroed<T>(n: usize) -> Vec<T> {
    let l = std::alloc::Layout::array::<T>(n.max(1)).unwrap();
    unsafe { Vec::from_raw_parts(std::alloc::alloc_zeroed(l) as *mut T, n, n.max(1)) }
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

// phrase check on the doc text: find where word a (the rarest) could start by its first two
// bytes, case folded, 32 at a time; then match the words after it forward and the words before
// it backward, all as whole tokens through the lowercase table
fn has2(docs: &[u8], start: usize, ks: &[&[u8]], a: usize, lower: &[u8; 256]) -> bool {
    use std::arch::x86_64::*;
    let at = |j: usize| -> bool {
        let mut e = j;
        for w in ks[..a].iter().rev() {
            let mut q = e - 1;
            while lower[docs[q] as usize] == 0 { if docs[q] == b'\t' { return false; } q -= 1; }
            if q < start + w.len() { return false; }
            let s = q + 1 - w.len();
            if lower[docs[s - 1] as usize] != 0 || (0..w.len()).any(|m| lower[docs[s + m] as usize] != w[m]) { return false; }
            e = s;
        }
        let mut j = j;
        for (n, w) in ks[a..].iter().enumerate() {
            if (0..w.len()).any(|m| lower[docs[j + m] as usize] != w[m]) || lower[docs[j + w.len()] as usize] != 0 { return false; }
            j += w.len();
            if a + n + 1 < ks.len() { while lower[docs[j] as usize] == 0 { if docs[j] == b'\n' { return false; } j += 1; } }
        }
        true
    };
    let w = ks[a];
    let (c0, c1) = (w[0], if w.len() > 1 { w[1] } else { 0 });
    // c1 == 0 matches any second byte, for a one-byte word
    let hit = |j: usize| docs[j] | 0x20 == c0 && (c1 == 0 || docs[j + 1] | 0x20 == c1) && lower[docs[j - 1] as usize] == 0 && at(j);
    let mut i = start + 1;
    unsafe {
        let (f0, f1, lf, fold) = (_mm256_set1_epi8(c0 as i8), _mm256_set1_epi8(c1 as i8), _mm256_set1_epi8(b'\n' as i8), _mm256_set1_epi8(0x20));
        loop {
            if i + 33 > docs.len() {
                while i < docs.len() && docs[i] != b'\n' { if hit(i) { return true; } i += 1; }
                return false;
            }
            let x = _mm256_loadu_si256(docs.as_ptr().add(i) as *const __m256i);
            let mut m = _mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_or_si256(x, fold), f0)) as u32;
            if c1 != 0 {
                let y = _mm256_loadu_si256(docs.as_ptr().add(i + 1) as *const __m256i);
                m &= _mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_or_si256(y, fold), f1)) as u32;
            }
            let e = _mm256_movemask_epi8(_mm256_cmpeq_epi8(x, lf)) as u32;
            if e != 0 { m &= (1 << e.trailing_zeros()) - 1; }
            while m != 0 {
                let j = i + m.trailing_zeros() as usize;
                m &= m - 1;
                if lower[docs[j - 1] as usize] == 0 && at(j) { return true; }
            }
            if e != 0 { return false; }
            i += 32;
        }
    }
}
