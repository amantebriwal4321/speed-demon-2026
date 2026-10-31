use std::sync::atomic::{AtomicU64, Ordering::{Acquire, Relaxed, Release}};
use std::hash::Hasher;
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

fn fx(b: &[u8]) -> u64 { let mut h = Fx(0); h.write(b); h.finish() }

#[derive(Clone, Copy)]
struct Agg { count: u64, id_sum: i64, sum: i64, min: i64, max: i64, name_bytes: u64, note_bytes: u64 }
impl Agg { fn new() -> Self { Agg { count: 0, id_sum: 0, sum: 0, min: i64::MAX, max: i64::MIN, name_bytes: 0, note_bytes: 0 } } }

// index of the first byte equal to `c`: SSE2, 16 bytes at a time, then eight at a time near the end
#[inline]
fn find(b: &[u8], c: u8) -> usize {
    use std::arch::x86_64::*;
    let mut i = 0;
    unsafe {
        let pat = _mm_set1_epi8(c as i8);
        while i + 16 <= b.len() {
            let m = _mm_movemask_epi8(_mm_cmpeq_epi8(_mm_loadu_si128(b.as_ptr().add(i) as *const __m128i), pat));
            if m != 0 { return i + m.trailing_zeros() as usize; }
            i += 16;
        }
    }
    let pat = 0x0101010101010101u64 * c as u64;
    while i + 8 <= b.len() {
        let x = u64::from_le_bytes(b[i..i + 8].try_into().unwrap()) ^ pat;
        let z = x.wrapping_sub(0x0101010101010101) & !x & 0x8080808080808080;
        if z != 0 { return i + (z.trailing_zeros() / 8) as usize; }
        i += 8;
    }
    i + b[i..].iter().position(|&x| x == c).unwrap()
}

const BOM: &[u8] = b"\xEF\xBB\xBF";

// parse one text field at b[*i..]. Returns the raw bytes between the quotes (or the unquoted
// bytes) and the unescaped length, and leaves *i after the comma. Unescaping is one to one
// ("" <-> "), so two raw slices are equal exactly when their values are.
fn field(b: &[u8], i: &mut usize) -> (usize, usize, u64) {
    if b[*i] == b'"' {
        let start = *i + 1;
        let mut j = start;
        let mut pairs = 0;
        loop {
            let q = j + find(&b[j..], b'"');
            if b[q + 1] == b'"' { pairs += 1; j = q + 2; continue; }
            *i = q + 2; // closing quote and comma
            return (start, q, (q - start - pairs) as u64);
        }
    }
    let start = *i;
    let e = start + find(&b[start..], b',');
    *i = e + 1;
    (start, e, (e - start) as u64)
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

    // one name table shared by all threads, filled with CAS. A slot is
    // tag << 48 | len << 32 | (offset + 1), where offset and len point into `body`.
    // The slot index is the name's id, and each country keeps an atomic bitset of ids.
    const SLOTS: usize = 1 << 18; // the name pool is 100,000 per the spec
    let table: Vec<AtomicU64> = (0..SLOTS).map(|_| AtomicU64::new(0)).collect();
    // the raw bytes of names up to 16 long sit next to the table, so a hit compares two
    // words instead of reading the first occurrence back from somewhere in the file
    let inl: Vec<[AtomicU64; 2]> = (0..SLOTS).map(|_| [AtomicU64::new(0), AtomicU64::new(0)]).collect();
    let words = SLOTS / 64;
    let seen: Vec<AtomicU64> = (0..ng * words).map(|_| AtomicU64::new(0)).collect();
    let aggs: Vec<Vec<Agg>> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..nt).map(|k| {
            let (lo, hi) = (cuts[k], cuts[k + 1]);
            let (lookup, table, seen, inl) = (&lookup, &table, &seen, &inl);
            sc.spawn(move || {
                let c = body;
                let mut agg = vec![Agg::new(); ng];
                let mut i = lo;
                while i < hi {
                    if c[i..].starts_with(BOM) { i += 3; }
                    let mut id = 0i64;
                    while c[i] != b',' { id = id * 10 + (c[i] - b'0') as i64; i += 1; }
                    i += 1;
                    let (ns, ne, nl) = field(c, &mut i);
                    let (_, _, tl) = field(c, &mut i);
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
                    let g = if rid < lookup.len() { lookup[rid] } else { unm } as usize;
                    let x = &mut agg[g];
                    x.count += 1; x.id_sum += id; x.sum += a;
                    if a < x.min { x.min = a; }
                    if a > x.max { x.max = a; }
                    x.name_bytes += nl; x.note_bytes += tl;
                    let name = &c[ns..ne];
                    let h = fx(name);
                    let me = (h >> 48) << 48 | (name.len() as u64) << 32 | (ns as u64 + 1);
                    let short = name.len() <= 16;
                    let (w0, w1) = if short {
                        let mut t = [0u8; 16];
                        t[..name.len()].copy_from_slice(name);
                        (u64::from_le_bytes(t[..8].try_into().unwrap()), u64::from_le_bytes(t[8..].try_into().unwrap()))
                    } else { (0, 0) };
                    const PENDING: u64 = 1; // a claimed slot whose inline bytes are not out yet
                    let mut j = h as usize & (SLOTS - 1);
                    loop {
                        let mut cur = table[j].load(Acquire);
                        if cur == 0 {
                            match table[j].compare_exchange(0, PENDING, Acquire, Acquire) {
                                Ok(_) => {
                                    inl[j][0].store(w0, Relaxed); inl[j][1].store(w1, Relaxed);
                                    table[j].store(me, Release);
                                    break;
                                }
                                Err(v) => cur = v,
                            }
                        }
                        while cur == PENDING { std::hint::spin_loop(); cur = table[j].load(Acquire); }
                        if cur >> 48 == me >> 48 && (cur >> 32 & 0xFFFF) == name.len() as u64 {
                            if short {
                                if inl[j][0].load(Relaxed) == w0 && inl[j][1].load(Relaxed) == w1 { break; }
                            } else {
                                let o = (cur & 0xFFFF_FFFF) as usize - 1;
                                if &c[o..o + name.len()] == name { break; }
                            }
                        }
                        j = (j + 1) & (SLOTS - 1);
                    }
                    seen[g * words + j / 64].fetch_or(1 << (j % 64), Relaxed);
                }
                agg
            })
        }).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let mut total = vec![Agg::new(); ng];
    for a in &aggs {
        for g in 0..ng {
            let (t, x) = (&mut total[g], &a[g]);
            t.count += x.count; t.id_sum += x.id_sum; t.sum += x.sum;
            t.min = t.min.min(x.min); t.max = t.max.max(x.max);
            t.name_bytes += x.name_bytes; t.note_bytes += x.note_bytes;
        }
    }
    let distinct: Vec<u64> = (0..ng).map(|g| seen[g * words..(g + 1) * words].iter().map(|w| w.load(Relaxed).count_ones() as u64).sum()).collect();

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
