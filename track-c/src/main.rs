use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::io::Write;

// FxHash-style hash of a byte string, eight bytes at a time
fn fx(b: &[u8]) -> u64 {
    let mut h = 0u64;
    let mut c = b.chunks_exact(8);
    for x in &mut c { h = (h.rotate_left(5) ^ u64::from_le_bytes(x.try_into().unwrap())).wrapping_mul(0x517cc1b727220a95); }
    for &x in c.remainder() { h = (h.rotate_left(5) ^ x as u64).wrapping_mul(0x517cc1b727220a95); }
    h
}

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
    let r = map(&format!("{data}/regions.csv"));
    let mut i = r.iter().position(|&c| c == b'\n').unwrap() + 1;
    let mut lookup: Vec<u16> = Vec::new(); // region_id -> code here, u16::MAX if absent; -> rank below
    while i < r.len() {
        let mut id = 0usize;
        while r[i] != b',' { id = id * 10 + (r[i] - b'0') as usize; i += 1; }
        let c = &r[i + 1..i + 4];
        if lookup.len() <= id { lookup.resize(id + 1, u16::MAX); }
        lookup[id] = (c[0] - b'A') as u16 * 676 + (c[1] - b'A') as u16 * 26 + (c[2] - b'A') as u16;
        i += 5;
    }
    let mut rank = vec![u16::MAX; 17576];
    for &c in &lookup { if c != u16::MAX { rank[c as usize] = 0; } }
    let mut names: Vec<[u8; 3]> = Vec::new();
    for c in 0..17576 {
        if rank[c] == 0 { rank[c] = names.len() as u16; names.push([b'A' + (c / 676) as u8, b'A' + (c / 26 % 26) as u8, b'A' + (c % 26) as u8]); }
    }
    let unm = names.len() as u16;
    for c in lookup.iter_mut() { *c = if *c == u16::MAX { unm } else { rank[*c as usize] }; }
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
    let table: Vec<AtomicU64> = (0..SLOTS).map(|_| AtomicU64::new(0)).collect();
    let words = SLOTS / 64;
    let seen: Vec<AtomicU64> = (0..ng * words).map(|_| AtomicU64::new(0)).collect();
    let aggs: Vec<Vec<Agg>> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..nt).map(|k| {
            let (lo, hi) = (cuts[k], cuts[k + 1]);
            let (lookup, table, seen) = (&lookup, &table, &seen);
            sc.spawn(move || {
                let c = body;
                let mut agg = vec![Agg::new(); ng];
                let mut i = lo;
                let (mut pg, mut pme, mut pj) = (usize::MAX, 0u64, 0usize);
                while i < hi {
                    if c[i..].starts_with(BOM) { i += 3; }
                    let id = dig(c, &mut i) as i64;
                    i += 1;
                    let (ns, ne, nl) = field(c, &mut i);
                    let (_, _, tl) = field(c, &mut i);
                    let rid = dig(c, &mut i) as usize;
                    i += 1;
                    let neg = c[i] == b'-';
                    if neg { i += 1; }
                    // amount is digits, a dot, then exactly two digits
                    let mut a = dig(c, &mut i) as i64;
                    a = a * 100 + ((c[i + 1] - b'0') * 10 + (c[i + 2] - b'0')) as i64;
                    i += 3;
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
                    // names are told apart by a 64 bit hash of their bytes and length; with
                    // 100,000 names a collision has odds near 1 in 10^9
                    let me = ((h ^ name.len() as u64).wrapping_mul(0x9E3779B97F4A7C15) >> 1) | 1;
                    let j0 = h as usize & (SLOTS - 1);
                    // prefetch this record's slot and seen word, then insert the previous record,
                    // so the two cache misses overlap the parse of the next record
                    unsafe {
                        use std::arch::x86_64::*;
                        _mm_prefetch(table.as_ptr().add(j0) as *const i8, _MM_HINT_T0);
                        _mm_prefetch(seen.as_ptr().add(g * words + j0 / 64) as *const i8, _MM_HINT_T0);
                    }
                    if pg != usize::MAX { insert(table, seen, pg, pme, pj); }
                    pg = g; pme = me; pj = j0;
                }
                if pg != usize::MAX { insert(table, seen, pg, pme, pj); }
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
    unsafe { std::slice::from_raw_parts(mmap(std::ptr::null_mut(), len, 1, 2, f.as_raw_fd(), 0), len) }
}

const SLOTS: usize = 1 << 17; // the name pool is 100,000 per the spec, so the table stays under 0.77 full
const WORDS: usize = SLOTS / 64;

#[inline(always)]
fn insert(table: &[AtomicU64], seen: &[AtomicU64], g: usize, me: u64, mut j: usize) {
    loop {
        let cur = table[j].load(Relaxed);
        if cur == me { break; }
        if cur == 0 { match table[j].compare_exchange(0, me, Relaxed, Relaxed) { Ok(_) => break, Err(v) if v == me => break, Err(_) => {} } }
        j = (j + 1) & (SLOTS - 1);
    }
    seen[g * WORDS + j / 64].fetch_or(1 << (j % 64), Relaxed);
}

// the digits at b[*i..], leaving *i on the first non digit: 8 bytes at once with SWAR, the
// byte loop for longer numbers and near the end of the file
#[inline]
fn dig(b: &[u8], i: &mut usize) -> u64 {
    if *i + 8 <= b.len() {
        let x = u64::from_le_bytes(b[*i..*i + 8].try_into().unwrap());
        let d = x.wrapping_sub(0x3030303030303030);
        let stop = (d | d.wrapping_add(0x7676767676767676)) & 0x8080808080808080;
        if stop != 0 {
            let n = (stop.trailing_zeros() / 8) as usize;
            if n == 0 { return 0; }
            let x = (d << (8 * (8 - n))) & 0x0F0F0F0F0F0F0F0F;
            let x = (x * 10 + (x >> 8)) & 0x00FF00FF00FF00FF;
            let x = (x * 100 + (x >> 16)) & 0x0000FFFF0000FFFF;
            *i += n;
            return (x * 10000 + (x >> 32)) & 0xFFFFFFFF;
        }
    }
    let mut v = 0u64;
    while *i < b.len() && b[*i].is_ascii_digit() { v = v * 10 + (b[*i] - b'0') as u64; *i += 1; }
    v
}
