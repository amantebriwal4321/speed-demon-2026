use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Fx(u64);
impl Hasher for Fx {
    fn finish(&self) -> u64 { self.0 }
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
    struct Part { lo: u32, docs: Vec<Vec<u32>>, stream: Vec<u32>, start: Vec<u32> }
    let parts: Vec<Part> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..nt).map(|k| {
            let c = &docs[cuts[k]..cuts[k + 1]];
            let (keys, vals, long, lower, h) = (&keys, &vals, &long, &lower, &h);
            sc.spawn(move || {
                let mut p = Part { lo: u32::MAX, docs: vec![Vec::new(); nterm], stream: Vec::with_capacity(c.len() / 6), start: Vec::new() };
                let mut i = 0;
                while i < c.len() {
                    let mut doc = 0u32;
                    while c[i] != b'\t' { doc = doc * 10 + (c[i] - b'0') as u32; i += 1; }
                    if p.lo == u32::MAX { p.lo = doc; }
                    p.start.push(p.stream.len() as u32);
                    i += 1;
                    loop {
                        while lower[c[i] as usize] == 0 && c[i] != b'\n' { i += 1; }
                        if c[i] == b'\n' { break; }
                        let s = i;
                        let mut k = 0u128;
                        let mut l = 0;
                        while lower[c[i] as usize] != 0 { if l < 16 { k |= (lower[c[i] as usize] as u128) << (8 * l); } l += 1; i += 1; }
                        let id = if l <= 16 {
                            let mut j = h(k);
                            loop { let x = keys[j]; if x == k { break Some(vals[j]); } if x == 0 { break None; } j = (j + 1) & (cap - 1); }
                        } else if l <= 32 {
                            let mut t = [0u8; 32];
                            for (a, &b) in t.iter_mut().zip(&c[s..i]) { *a = lower[b as usize]; }
                            long.get(&t[..l]).copied()
                        } else { None };
                        match id {
                            Some(t) => {
                                if p.docs[t as usize].last() != Some(&doc) { p.docs[t as usize].push(doc); }
                                p.stream.push(t);
                            }
                            None => p.stream.push(u32::MAX),
                        }
                    }
                    i += 1;
                }
                p.start.push(p.stream.len() as u32);
                p
            })
        }).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });

    fn dl(p: &Part, t: u32) -> &Vec<u32> { &p.docs[t as usize] }

    let res: Vec<std::sync::OnceLock<Vec<u8>>> = (0..qs.len()).map(|_| Default::default()).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|sc| {
        for _ in 0..nt {
            sc.spawn(|| loop {
                let k = next.fetch_add(1, Ordering::Relaxed);
                if k >= qs.len() { break; }
                // parts cover disjoint, ascending doc ranges, so each query runs per part
                // and the per part answers just concatenate
                let mut r: Vec<u32> = Vec::new();
                for p in &parts {
                    match &qs[k] {
                        Q::Bool(v) => {
                            let mut x = dl(p, v[0].1).clone();
                            for &(op, t) in &v[1..] {
                                x = match op { 1 => and(&x, dl(p, t)), 2 => or(&x, dl(p, t)), _ => not(&x, dl(p, t)) };
                            }
                            r.extend_from_slice(&x);
                        }
                        Q::Phrase(ts) => {
                            // docs holding the two rarest terms, then an int compare scan of each
                            let mut order: Vec<u32> = ts.clone();
                            order.sort_by_key(|&t| dl(p, t).len());
                            let n = ts.len();
                            for d in and(dl(p, order[0]), dl(p, order[1])) {
                                let i = (d - p.lo) as usize;
                                let toks = &p.stream[p.start[i] as usize..p.start[i + 1] as usize];
                                if toks.windows(n).any(|w| w == &ts[..]) { r.push(d); }
                            }
                        }
                    }
                }
                let mut o = Vec::new();
                let sum: u64 = r.iter().map(|&x| x as u64).sum();
                write!(o, "{} {}", r.len(), sum).unwrap();
                for x in r.iter().take(10) { write!(o, " {x}").unwrap(); }
                o.push(b'\n');
                res[k].set(o).ok();
            });
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
