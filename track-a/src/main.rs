use std::io::Write;

const HOURS: usize = 72; // 2026-10-30T18 .. 2026-11-02T17

#[inline(always)]
fn ld8(b: &[u8], i: usize) -> u64 { unsafe { (b.as_ptr().add(i) as *const u64).read_unaligned() } }

// one thread's endpoints: open addressing on (first 8 bytes, last 8 bytes, length), checked
// against the bytes of the first occurrence, which stay in the mmap
// one hour's rows in fixed blocks, so nothing is ever copied by a growing Vec. A row is
// local id << 21 | is_5xx << 20 | latency; latencies from 2^20 - 1 up (about 1%) and ids past
// 2046 go to `side` as full u64 rows instead
const LAT_ESC: u32 = (1 << 20) - 1;
const BLK: usize = 16384; // 64 KB, so far fewer mmap and munmap calls; untouched pages of a last block cost nothing
struct Hr { blocks: Vec<Vec<u32>>, side: Vec<u64> }
impl Hr {
    #[inline(always)]
    fn push(&mut self, x: u32) {
        match self.blocks.last_mut() {
            Some(b) if b.len() < BLK => b.push(x),
            _ => { let mut b = Vec::with_capacity(BLK); b.push(x); self.blocks.push(b); }
        }
    }
}

struct Eps { slot: Vec<u32>, start: Vec<u32>, len: Vec<u32>, key: Vec<(u64, u64, u32)> }
const TB: usize = 1 << 13;

fn main() {
    let data = std::env::var("DATA").unwrap_or("/data".into());
    let out = std::env::var("OUT").unwrap_or("/out".into());
    // blocks of 16 KB and up get their own mmap, so a freed hour really leaves the process
    extern "C" { fn mallopt(p: i32, v: i32) -> i32; }
    unsafe { mallopt(-3, 16 * 1024 - 64) }; // M_MMAP_THRESHOLD
    let b = map(&format!("{data}/requests.log"));
    // method length from its first two bytes
    let mut mlen = vec![0u8; 1 << 16];
    for m in ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"] { let m = m.as_bytes(); mlen[(m[0] as usize) << 8 | m[1] as usize] = m.len() as u8; }
    let mlen = &mlen;
    let nt = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(32);
    let mut cuts = vec![0usize];
    for k in 1..nt {
        let mut p = (b.len() * k / nt).max(*cuts.last().unwrap());
        while p < b.len() && p > 0 && b[p - 1] != b'\n' { p += 1; }
        cuts.push(p);
    }
    cuts.push(b.len());

    // per thread: endpoint table, and per hour rows of local id << 33 | latency << 1 | is_5xx
    let parts: Vec<(Eps, Vec<Hr>)> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..nt).map(|k| {
            let (lo, hi) = (cuts[k], cuts[k + 1]);
            sc.spawn(move || {
                let mut e = Eps { slot: vec![u32::MAX; TB], start: Vec::new(), len: Vec::new(), key: Vec::new() };
                let mut rows: Vec<Hr> = (0..HOURS).map(|_| Hr { blocks: Vec::new(), side: Vec::new() }).collect();
                // walk the LF bits of 64 byte blocks. Each line is parsed from its own two ends,
                // so finding the next line never waits on parsing this one.
                let mut start = lo;
                let mut blk = lo;
                while blk < hi {
                    let mut m = if blk + 64 <= b.len() { lf64(b, blk) } else {
                        let mut t = [0u8; 64];
                        t[..b.len() - blk].copy_from_slice(&b[blk..]);
                        lf64(&t, 0)
                    };
                    if blk + 64 > hi { m &= (1u64 << (hi - blk)) - 1; }
                    while m != 0 {
                        let lf = blk + m.trailing_zeros() as usize;
                        m &= m - 1;
                        let j = start;
                        start = lf + 1;
                        let c = b;
                        // 2026-MM-DDTHH: day offset from Oct 30, then the hour
                        let ts = ld8(c, j + 5); // "MM-DDTHH" at +5 .. +12, little endian
                        let m1 = (ts >> 8) as u8; // second month digit: '0' for Oct (10), '1' for Nov (11)
                        let dd = (((ts >> 24) as u8 - b'0') * 10 + ((ts >> 32) as u8 - b'0')) as usize;
                        let hh = (((ts >> 48) as u8 - b'0') * 10 + ((ts >> 56) as u8 - b'0')) as usize;
                        let day = if m1 == b'0' { dd - 30 } else { dd + 1 };
                        let hour = day * 24 + hh - 18;
                        let ml = unsafe { *mlen.get_unchecked((c[j + 28] as usize) << 8 | c[j + 29] as usize) } as usize;
                        let es = j + 29 + ml;
                        // from the back: latency is at most 10 digits, so the space before it is in
                        // the last 16 bytes, and the endpoint ends 4 bytes before that space
                        let sm = sp16(c, lf - 16);
                        let s4 = lf - 16 + (31 - sm.leading_zeros()) as usize;
                        let ee = s4 - 4;
                        let five = c[ee + 1] == b'5';
                        let nd = lf - s4 - 1; // latency digits, 1 to 10
                        let v = if nd <= 8 {
                            // the 8 bytes ending at the LF, junk bytes in front masked off, then SWAR
                            let x = ld8(c, lf - 8) & 0x0F0F0F0F0F0F0F0F & (u64::MAX << (8 * (8 - nd)));
                            let x = (x * 10 + (x >> 8)) & 0x00FF00FF00FF00FF;
                            let x = (x * 100 + (x >> 16)) & 0x0000FFFF0000FFFF;
                            (x * 10000 + (x >> 32)) & 0xFFFFFFFF
                        } else {
                            let mut v = 0u64;
                            for &d in &c[s4 + 1..lf] { v = v * 10 + (d - b'0') as u64; }
                            v
                        };
                        let base = 0usize;
                        let l = ee - es;
                        let (w0, w1) = if l >= 8 { (ld8(c, es), ld8(c, ee - 8)) } else { (ld8(c, es) & ((1u64 << (8 * l)) - 1), 0) };
                        let h = ((w0.wrapping_mul(0x9E3779B97F4A7C15) ^ w1.wrapping_mul(0xC2B2AE3D27D4EB4F) ^ (l as u64).wrapping_mul(0x165667B19E3779F9)) >> 50) as usize & (TB - 1);
                        let mut s = h;
                        let id = loop {
                            let x = unsafe { *e.slot.get_unchecked(s) };
                            if x == u32::MAX {
                                let id = e.start.len() as u32;
                                e.slot[s] = id; e.start.push((es + base) as u32); e.len.push(l as u32);
                                e.key.push((w0, w1, l as u32));
                                break id;
                            }
                            let k = unsafe { e.key.get_unchecked(x as usize) };
                            if k.0 == w0 && k.1 == w1 && k.2 == l as u32 && (l <= 16 || { let st = e.start[x as usize] as usize; c[st..st + l] == c[es..ee] }) { break x; }
                            s = (s + 1) & (TB - 1);
                        };
                        let r = unsafe { rows.get_unchecked_mut(hour) };
                        if v < LAT_ESC as u64 && id < 2047 { r.push(id << 21 | (five as u32) << 20 | v as u32); } else { r.side.push((id as u64) << 33 | v << 1 | five as u64); }
                    }
                    blk += 64;
                }
                (e, rows)
            })
        }).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // global ids in byte order of the endpoint text
    // distinct names through a hash map, one sort of the 1,842, then each thread's ids mapped
    let name = |e: &Eps, x: usize| -> &'static [u8] { &b[e.start[x] as usize..(e.start[x] + e.len[x]) as usize] };
    let mut uniq: std::collections::HashMap<&[u8], u32> = std::collections::HashMap::with_capacity(4096);
    for (e, _) in &parts { for x in 0..e.len.len() { uniq.entry(name(e, x)).or_insert(0); } }
    let mut all: Vec<&[u8]> = uniq.keys().copied().collect();
    all.sort_unstable();
    for (r, k) in all.iter().enumerate() { *uniq.get_mut(k).unwrap() = r as u32; }
    let ne = all.len();
    let maps: Vec<Vec<u32>> = parts.iter().map(|(e, _)| (0..e.len.len()).map(|x| uniq[name(e, x)]).collect()).collect();
    let parts: Vec<std::sync::Mutex<Vec<Hr>>> = parts.into_iter().map(|(_, r)| std::sync::Mutex::new(r)).collect();
    let _ = &parts;
    let rows_of = |pi: usize, h: usize| std::mem::replace(&mut parts[pi].lock().unwrap()[h], Hr { blocks: Vec::new(), side: Vec::new() });

    let next = std::sync::atomic::AtomicUsize::new(0);
    // hours get written in order as soon as they and every hour before them are done, so
    // only a few finished hours ever wait in memory
    std::fs::create_dir_all(&out).ok();
    let file = std::fs::File::create(format!("{out}/summary.txt")).unwrap();
    let wr: std::sync::Mutex<(usize, Vec<Option<Vec<u8>>>, &std::fs::File)> = std::sync::Mutex::new((0, (0..HOURS).map(|_| None).collect(), &file));
    std::thread::scope(|sc| {
        for _ in 0..nt {
            sc.spawn(|| {
                let mut cnt = vec![0u32; ne + 1];
                let mut buf: Vec<u32> = Vec::new();
                let mut errv = vec![0u64; ne];
                loop {
                    let h = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if h >= HOURS { break; }
                    let date = ["2026-10-30", "2026-10-31", "2026-11-01", "2026-11-02"][(h + 18) / 24];
                    let hh = (h + 18) % 24;
                    let rs: Vec<Hr> = (0..maps.len()).map(|pi| rows_of(pi, h)).collect();
                    // counting sort by global endpoint id, keeping latency << 1 | is_5xx
                    cnt.fill(0);
                    for (pi, r) in rs.iter().enumerate() {
                        let m = &maps[pi];
                        for b in &r.blocks { for &x in b { cnt[m[(x >> 21) as usize] as usize + 1] += 1; } }
                        for &x in &r.side { cnt[m[(x >> 33) as usize] as usize + 1] += 1; }
                    }
                    for g in 0..ne { cnt[g + 1] += cnt[g]; }
                    let total = cnt[ne] as usize;
                    buf.clear(); buf.resize(total, 0);
                    let mut cur = cnt.clone();
                    for (pi, r) in rs.iter().enumerate() {
                        let m = &maps[pi];
                        for b in &r.blocks { for &x in b {
                            let g = m[(x >> 21) as usize] as usize;
                            buf[cur[g] as usize] = x & LAT_ESC; errv[g] += (x >> 20 & 1) as u64; cur[g] += 1;
                        } }
                        for &x in &r.side { let g = m[(x >> 33) as usize] as usize; buf[cur[g] as usize] = (x >> 1) as u32; errv[g] += x & 1; cur[g] += 1; }
                    }
                    drop(rs); // the blocks are their own mappings, so this hands the pages back while the output grows in the page cache
                    let mut o = Vec::with_capacity(ne * 48);
                    let hdr = format!("{date}T{hh:02}:00:00Z ").into_bytes();
                    for g in 0..ne {
                        let (s, t) = (cnt[g] as usize, cnt[g + 1] as usize);
                        if s == t { continue; }
                        let grp = &mut buf[s..t];
                        let n = grp.len();
                        let errs = std::mem::take(&mut errv[g]);
                        // nearest rank percentiles; groups average about 25 rows, so a plain sort
                        grp.sort_unstable();
                        let pc = |p: usize| grp[(p * n + 99) / 100 - 1] as u64;
                        o.extend_from_slice(&hdr);
                        o.extend_from_slice(all[g]);
                        for x in [n as u64, pc(50), pc(95), pc(99), errs, errs * 1_000_000 / n as u64] { o.push(b' '); num(&mut o, x); }
                        o.push(b'\n');
                    }
                    let mut w = wr.lock().unwrap();
                    w.1[h] = Some(o);
                    while w.0 < HOURS {
                        let k = w.0;
                        let Some(x) = w.1[k].take() else { break };
                        (&*w.2).write_all(&x).unwrap();
                        w.0 += 1;
                    }
                }
            });
        }
    });
    std::process::exit(0);
}

// decimal digits of x, two at a time
#[inline]
fn num(o: &mut Vec<u8>, mut x: u64) {
    const D: &[u8; 200] = b"00010203040506070809101112131415161718192021222324252627282930313233343536373839404142434445464748495051525354555657585960616263646566676869707172737475767778798081828384858687888990919293949596979899";
    let mut t = [0u8; 20];
    let mut i = 20;
    while x >= 100 { let r = (x % 100) as usize; x /= 100; i -= 2; t[i..i + 2].copy_from_slice(&D[2 * r..2 * r + 2]); }
    if x >= 10 { i -= 2; t[i..i + 2].copy_from_slice(&D[2 * x as usize..2 * x as usize + 2]); } else { i -= 1; t[i] = b'0' + x as u8; }
    o.extend_from_slice(&t[i..]);
}

// LFs in c[at .. at + 64]
#[inline(always)]
fn lf64(c: &[u8], at: usize) -> u64 {
    use std::arch::x86_64::*;
    unsafe {
        let p = c.as_ptr().add(at) as *const __m256i;
        let lf = _mm256_set1_epi8(b'\n' as i8);
        _mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_loadu_si256(p), lf)) as u32 as u64 | (_mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_loadu_si256(p.add(1)), lf)) as u32 as u64) << 32
    }
}

// bit i set when c[at + i] is a space
#[inline(always)]
fn sp16(c: &[u8], at: usize) -> u32 {
    use std::arch::x86_64::*;
    unsafe { _mm_movemask_epi8(_mm_cmpeq_epi8(_mm_loadu_si128(c.as_ptr().add(at) as *const __m128i), _mm_set1_epi8(b' ' as i8))) as u32 }
}



// read-only mmap of the input; page cache pages are not charged to us
fn map(path: &str) -> &'static [u8] {
    use std::os::unix::io::AsRawFd;
    extern "C" { fn mmap(a: *mut u8, l: usize, p: i32, f: i32, fd: i32, o: i64) -> *mut u8; }
    let f = std::fs::File::open(path).unwrap();
    let len = f.metadata().unwrap().len() as usize;
    if len == 0 { return &[]; }
    let flags = 2; // MAP_PRIVATE
    unsafe { std::slice::from_raw_parts(mmap(std::ptr::null_mut(), len, 1, flags, f.as_raw_fd(), 0), len) }
}
