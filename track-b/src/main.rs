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

fn and(a: &[u32], b: &[u32]) -> Vec<u32> {
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
    let (mut i, mut j, mut o) = (0, 0, Vec::new());
    while i < a.len() {
        while j < b.len() && b[j] < a[i] { j += 1; }
        if j >= b.len() || b[j] != a[i] { o.push(a[i]); }
        i += 1;
    }
    o
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

    let docs = std::fs::read(format!("{data}/docs.txt")).unwrap();
    let nt = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16);
    // chunk the corpus on line boundaries; each chunk needs its first doc id
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

    // per thread: for each term, its (doc, position) occurrences in file order
    let parts: Vec<Vec<Vec<(u32, u32)>>> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..nt).map(|k| {
            let c = &docs[cuts[k]..cuts[k + 1]];
            let (ids, lower) = (&ids, &lower);
            sc.spawn(move || {
                let mut post: Vec<Vec<(u32, u32)>> = vec![Vec::new(); nterm];
                let mut tok = [0u8; 64];
                let mut i = 0;
                while i < c.len() {
                    let mut doc = 0u32;
                    while c[i] != b'\t' { doc = doc * 10 + (c[i] - b'0') as u32; i += 1; }
                    i += 1;
                    let mut pos = 0u32;
                    loop {
                        while c[i] != b'\n' && lower[c[i] as usize] == 0 { i += 1; }
                        if c[i] == b'\n' { break; }
                        let mut l = 0;
                        while lower[c[i] as usize] != 0 { if l < 64 { tok[l] = lower[c[i] as usize]; } l += 1; i += 1; }
                        if l <= 32 { if let Some(&t) = ids.get(&tok[..l]) { post[t as usize].push((doc, pos)); } }
                        pos += 1;
                    }
                    i += 1;
                }
                post
            })
        }).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // merge in chunk order (keeps doc-then-position order) and derive doc lists, in parallel over terms
    let next = AtomicUsize::new(0);
    let merged: Vec<std::sync::OnceLock<(Vec<(u32, u32)>, Vec<u32>)>> = (0..nterm).map(|_| Default::default()).collect();
    std::thread::scope(|sc| {
        for _ in 0..nt {
            sc.spawn(|| loop {
                let t = next.fetch_add(1, Ordering::Relaxed);
                if t >= nterm { break; }
                let occ: Vec<(u32, u32)> = parts.iter().flat_map(|p| p[t].iter().copied()).collect();
                let mut d: Vec<u32> = occ.iter().map(|x| x.0).collect();
                d.dedup();
                merged[t].set((occ, d)).ok();
            });
        }
    });
    drop(parts);
    let occ = |t: u32| &merged[t as usize].get().unwrap().0;
    let dl = |t: u32| &merged[t as usize].get().unwrap().1;

    let res: Vec<std::sync::OnceLock<Vec<u8>>> = (0..qs.len()).map(|_| Default::default()).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|sc| {
        for _ in 0..nt {
            sc.spawn(|| loop {
                let k = next.fetch_add(1, Ordering::Relaxed);
                if k >= qs.len() { break; }
                let r: Vec<u32> = match &qs[k] {
                    Q::Bool(v) => {
                        let mut r = dl(v[0].1).clone();
                        for &(op, t) in &v[1..] {
                            r = match op { 1 => and(&r, dl(t)), 2 => or(&r, dl(t)), _ => not(&r, dl(t)) };
                        }
                        r
                    }
                    Q::Phrase(ts) => {
                        // drive from the rarest term, probe the others by (doc, pos) binary search
                        let a = (0..ts.len()).min_by_key(|&i| occ(ts[i]).len()).unwrap();
                        let mut r = Vec::new();
                        for &(d, p) in occ(ts[a]) {
                            if r.last() == Some(&d) || (p as usize) < a { continue; }
                            let start = p - a as u32;
                            if ts.iter().enumerate().all(|(j, &t)| j == a || occ(t).binary_search(&(d, start + j as u32)).is_ok()) { r.push(d); }
                        }
                        r
                    }
                };
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
