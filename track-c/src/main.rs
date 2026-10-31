use std::borrow::Cow;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::Write;

// small FxHash-style hasher, enough for byte strings we own
#[derive(Default)]
struct Fx(u64);
impl Hasher for Fx {
    fn finish(&self) -> u64 { self.0 }
    fn write(&mut self, b: &[u8]) {
        let mut h = self.0;
        let mut c = b.chunks_exact(8);
        for x in &mut c { h = (h.rotate_left(5) ^ u64::from_le_bytes(x.try_into().unwrap())).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95); }
        for &x in c.remainder() { h = (h.rotate_left(5) ^ x as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95); }
        self.0 = h;
    }
    fn write_u16(&mut self, x: u16) { self.0 = (self.0.rotate_left(5) ^ x as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95); }
    fn write_usize(&mut self, x: usize) { self.0 = (self.0.rotate_left(5) ^ x as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95); }
}
type FxMap<K, V> = std::collections::HashMap<K, V, BuildHasherDefault<Fx>>;

fn fx(b: &[u8]) -> u64 { let mut h = Fx(0); h.write(b); h.finish() }

#[derive(Clone, Copy)]
struct Agg { count: u64, id_sum: i64, sum: i64, min: i64, max: i64, name_bytes: u64, note_bytes: u64 }
impl Agg { fn new() -> Self { Agg { count: 0, id_sum: 0, sum: 0, min: i64::MAX, max: i64::MIN, name_bytes: 0, note_bytes: 0 } } }

// index of the first byte equal to `c`, eight bytes at a time
#[inline]
fn find(b: &[u8], c: u8) -> usize {
    let pat = 0x0101010101010101u64 * c as u64;
    let mut i = 0;
    while i + 8 <= b.len() {
        let x = u64::from_le_bytes(b[i..i + 8].try_into().unwrap()) ^ pat;
        let z = x.wrapping_sub(0x0101010101010101) & !x & 0x8080808080808080;
        if z != 0 { return i + (z.trailing_zeros() / 8) as usize; }
        i += 8;
    }
    i + b[i..].iter().position(|&x| x == c).unwrap()
}

const BOM: &[u8] = b"\xEF\xBB\xBF";

// parse one text field at b[*i..]; returns (value, unescaped length) and leaves *i after the comma
fn field<'a>(b: &'a [u8], i: &mut usize) -> (Cow<'a, [u8]>, u64) {
    if b[*i] == b'"' {
        let start = *i + 1;
        let mut j = start;
        let mut esc = false;
        loop {
            let q = j + find(&b[j..], b'"');
            if q + 1 < b.len() && b[q + 1] == b'"' { esc = true; j = q + 2; continue; }
            *i = q + 2; // closing quote and comma
            let raw = &b[start..q];
            if !esc { return (Cow::Borrowed(raw), raw.len() as u64); }
            let mut v = Vec::with_capacity(raw.len());
            let mut k = 0;
            while k < raw.len() { v.push(raw[k]); if raw[k] == b'"' { k += 1; } k += 1; }
            let l = v.len() as u64;
            return (Cow::Owned(v), l);
        }
    }
    let start = *i;
    let e = start + find(&b[start..], b',');
    *i = e + 1;
    (Cow::Borrowed(&b[start..e]), (e - start) as u64)
}

fn main() {
    let data = std::env::var("DATA").unwrap_or("/data".into());
    let out = std::env::var("OUT").unwrap_or("/out".into());

    // regions: region_id -> country index, countries numbered in sorted order, _UNMATCHED last.
    // A code [A-Z]{3} is a number below 26^3, so counting the codes in numeric order sorts them.
    let r = std::fs::read(format!("{data}/regions.csv")).unwrap();
    let mut i = r.iter().position(|&c| c == b'\n').unwrap() + 1;
    let mut raw: Vec<u16> = Vec::new(); // region_id -> code, u16::MAX if absent
    while i < r.len() {
        let mut id = 0usize;
        while r[i] != b',' { id = id * 10 + (r[i] - b'0') as usize; i += 1; }
        let c = &r[i + 1..i + 4];
        if raw.len() <= id { raw.resize(id + 1, u16::MAX); }
        raw[id] = (c[0] - b'A') as u16 * 676 + (c[1] - b'A') as u16 * 26 + (c[2] - b'A') as u16;
        i += 5;
    }
    let mut rank = vec![u16::MAX; 17576];
    for &c in &raw { if c != u16::MAX { rank[c as usize] = 0; } }
    let mut names: Vec<[u8; 3]> = Vec::new();
    for c in 0..17576 {
        if rank[c] == 0 { rank[c] = names.len() as u16; names.push([b'A' + (c / 676) as u8, b'A' + (c / 26 % 26) as u8, b'A' + (c % 26) as u8]); }
    }
    let unm = names.len() as u16;
    let lookup: Vec<u16> = raw.iter().map(|&c| if c == u16::MAX { unm } else { rank[c as usize] }).collect();
    let ng = names.len() + 1;

    let b = map(&format!("{data}/records.csv"));
    let mut p = 0;
    if b.starts_with(BOM) { p = 3; }
    p += b[p..].iter().position(|&c| c == b'\n').unwrap() + 1;
    let body = &b[p..];
    let nt = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16);
    // a CR only ever appears as the first byte of a CRLF terminator, so cutting after "\r\n" is safe
    let mut cuts = vec![0usize];
    for k in 1..nt {
        let mut q = (body.len() * k / nt).max(*cuts.last().unwrap());
        while q < body.len() && !(q >= 2 && body[q - 2] == b'\r' && body[q - 1] == b'\n') { q += 1; }
        cuts.push(q);
    }
    cuts.push(body.len());

    let results: Vec<(Vec<Agg>, Vec<Cow<[u8]>>, Vec<Vec<u64>>, Vec<Vec<u32>>)> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..nt).map(|k| {
            let c = &body[cuts[k]..cuts[k + 1]];
            let lookup = &lookup;
            sc.spawn(move || {
                let mut agg = vec![Agg::new(); ng];
                // names interned to local ids; one bitset of seen name ids per country
                let mut ids: FxMap<Cow<[u8]>, u32> = FxMap::default();
                let mut bits: Vec<Vec<u64>> = vec![Vec::new(); ng];
                let mut i = 0;
                while i < c.len() {
                    if c[i..].starts_with(BOM) { i += 3; }
                    let mut id = 0i64;
                    while c[i] != b',' { id = id * 10 + (c[i] - b'0') as i64; i += 1; }
                    i += 1;
                    let (name, nl) = field(c, &mut i);
                    let (_, tl) = field(c, &mut i);
                    let mut rid = 0usize;
                    while c[i] != b',' { rid = rid * 10 + (c[i] - b'0') as usize; i += 1; }
                    i += 1;
                    let neg = c[i] == b'-';
                    if neg { i += 1; }
                    let mut a = 0i64;
                    while c[i] != b'\r' && c[i] != b'\n' { if c[i] != b'.' { a = a * 10 + (c[i] - b'0') as i64; } i += 1; }
                    if c[i] == b'\r' { i += 1; }
                    i += 1;
                    if neg { a = -a; }
                    let g = if rid < lookup.len() { lookup[rid] } else { unm };
                    let x = &mut agg[g as usize];
                    x.count += 1; x.id_sum += id; x.sum += a;
                    if a < x.min { x.min = a; }
                    if a > x.max { x.max = a; }
                    x.name_bytes += nl; x.note_bytes += tl;
                    let n = ids.len() as u32;
                    let id = *ids.entry(name).or_insert(n) as usize;
                    let bs = &mut bits[g as usize];
                    if bs.len() <= id / 64 { bs.resize(id / 64 + 1, 0); }
                    bs[id / 64] |= 1 << (id % 64);
                }
                let mut names = vec![Cow::Borrowed(&b""[..]); ids.len()];
                for (k, v) in ids { names[v as usize] = k; }
                // local ids bucketed by the merge shard that will own them
                let mut bucket: Vec<Vec<u32>> = vec![Vec::new(); nt];
                for (l, nm) in names.iter().enumerate() { bucket[fx(nm) as usize % nt].push(l as u32); }
                (agg, names, bits, bucket)
            })
        }).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let mut total = vec![Agg::new(); ng];
    for (a, _, _, _) in &results {
        for g in 0..ng {
            let (t, x) = (&mut total[g], &a[g]);
            t.count += x.count; t.id_sum += x.id_sum; t.sum += x.sum;
            t.min = t.min.min(x.min); t.max = t.max.max(x.max);
            t.name_bytes += x.name_bytes; t.note_bytes += x.note_bytes;
        }
    }
    // global name ids, built in nt shards by hash so the merge runs in parallel.
    // global id = shard + nt * index inside the shard
    let shards: Vec<Vec<Vec<u32>>> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..nt).map(|s| {
            let results = &results;
            sc.spawn(move || {
                let mut m: FxMap<&[u8], u32> = FxMap::default();
                results.iter().map(|(_, names, _, bucket)| bucket[s].iter().map(|&l| {
                    let n = m.len() as u32;
                    s as u32 + nt as u32 * *m.entry(&names[l as usize][..]).or_insert(n)
                }).collect()).collect()
            })
        }).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    // per part, local id -> global id
    let gid: Vec<Vec<u32>> = (0..results.len()).map(|p| {
        let mut g = vec![0u32; results[p].1.len()];
        for s in 0..nt { for (&l, &x) in results[p].3[s].iter().zip(&shards[s][p]) { g[l as usize] = x; } }
        g
    }).collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let distinct: Vec<std::sync::atomic::AtomicU64> = (0..ng).map(|_| Default::default()).collect();
    std::thread::scope(|sc| {
        for _ in 0..nt {
            sc.spawn(|| {
                let mut seen: Vec<u64> = Vec::new();
                loop {
                    let g = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if g >= ng { break; }
                    seen.clear();
                    let mut c = 0;
                    for (p, (_, _, bits, _)) in results.iter().enumerate() {
                        for (w, &word) in bits[g].iter().enumerate() {
                            let mut x = word;
                            while x != 0 {
                                let id = gid[p][w * 64 + x.trailing_zeros() as usize] as usize;
                                x &= x - 1;
                                if seen.len() <= id / 64 { seen.resize(id / 64 + 1, 0); }
                                if seen[id / 64] & (1 << (id % 64)) == 0 { seen[id / 64] |= 1 << (id % 64); c += 1; }
                            }
                        }
                    }
                    distinct[g].store(c, std::sync::atomic::Ordering::Relaxed);
                }
            });
        }
    });
    let distinct: Vec<u64> = distinct.iter().map(|x| x.load(std::sync::atomic::Ordering::Relaxed)).collect();

    let money = |v: i64| format!("{}{}.{:02}", if v < 0 { "-" } else { "" }, v.abs() / 100, v.abs() % 100);
    let mut o = Vec::new();
    for g in 0..ng {
        let t = &total[g];
        if t.count == 0 { continue; }
        let key = if g == ng - 1 { "_UNMATCHED".to_string() } else { String::from_utf8_lossy(&names[g]).into_owned() };
        writeln!(o, "{} {} {} {} {} {} {} {} {}", key, t.count, t.id_sum, money(t.sum), money(t.min), money(t.max), distinct[g], t.name_bytes, t.note_bytes).unwrap();
    }
    std::fs::create_dir_all(&out).ok();
    std::fs::write(format!("{out}/aggregates.txt"), o).unwrap();
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
