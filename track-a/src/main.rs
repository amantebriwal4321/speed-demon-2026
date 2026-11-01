use std::io::Write;

const HOURS: usize = 72; // 2026-10-30T18 .. 2026-11-02T17

#[inline(always)]
fn ld8(b: &[u8], i: usize) -> u64 {
    unsafe { (b.as_ptr().add(i) as *const u64).read_unaligned() }
}

// A row is local id << 21 | is_5xx << 20 | latency; latencies from 2^20 - 1 up (about 1%) and
// ids past 2046 go to a thread's `side` rows as full u64 rows instead.
const LAT_ESC: u32 = (1 << 20) - 1;

// Every row lives in one mapping reserved up front: for each hour a region per thread, the
// hour's regions side by side. A region holds as many rows as its thread's share of the file
// can have lines, so a thread appends to its own regions with no growth or bounds check, and
// untouched pages cost nothing. Once an hour's rows are gathered, one madvise hands all its
// pages back, instead of an unmap per block, each of which interrupted every core.
struct Rows {
    base: *mut u32,
    cap: usize,
    nt: usize,
}
unsafe impl Sync for Rows {}
extern "C" {
    fn madvise(a: *mut u8, l: usize, advice: i32) -> i32;
}
impl Rows {
    fn new(nt: usize, rows: usize) -> Rows {
        extern "C" {
            fn mmap(a: *mut u8, l: usize, p: i32, f: i32, fd: i32, o: i64) -> *mut u8;
        }
        let cap = rows.next_multiple_of(1024); // whole 4 KB pages, so freeing one hour never touches the next
        let len = HOURS * nt * cap * 4;
        let base = unsafe { mmap(std::ptr::null_mut(), len, 3, 0x4022, -1, 0) }; // read/write, private anonymous, no reserve
        unsafe { madvise(base, len, 15) }; // MADV_NOHUGEPAGE: a 2 MB page would span regions
        Rows {
            base: base as *mut u32,
            cap,
            nt,
        }
    }
    fn region(&self, h: usize, k: usize) -> *mut u32 {
        unsafe { self.base.add((h * self.nt + k) * self.cap) }
    }
    fn free(&self, h: usize) {
        unsafe { madvise(self.region(h, 0) as *mut u8, self.nt * self.cap * 4, 4) };
    } // MADV_DONTNEED
}

// a thread's endpoints: hash slots, and per id its text's place in `text` (a copy, so a long
// name is compared in cache rather than against its first line somewhere in the file)
struct Eps {
    slot: Vec<u32>,
    start: Vec<u32>,
    len: Vec<u32>,
    key: Vec<(u64, u64, u32)>,
    text: Vec<u8>,
}
const TB: usize = 1 << 13;

fn main() {
    let data = std::env::var("DATA").unwrap_or("/data".into());
    let out = std::env::var("OUT").unwrap_or("/out".into());
    // buffers of 16 KB and up get their own mmap, so a written hour's text really leaves the process
    extern "C" {
        fn mallopt(p: i32, v: i32) -> i32;
    }
    unsafe { mallopt(-3, 16 * 1024 - 64) }; // M_MMAP_THRESHOLD
    let b = map(&format!("{data}/requests.log"));
    // method length from its first two bytes
    let mut mlen = vec![0u8; 1 << 16];
    for m in ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"] {
        let m = m.as_bytes();
        mlen[(m[0] as usize) << 8 | m[1] as usize] = m.len() as u8;
    }
    let mlen = &mlen;
    let nt = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(32);
    let mut cuts = vec![0usize];
    for k in 1..nt {
        let mut p = (b.len() * k / nt).max(*cuts.last().unwrap());
        while p < b.len() && p > 0 && b[p - 1] != b'\n' {
            p += 1;
        }
        cuts.push(p);
    }
    cuts.push(b.len());
    // a line is at least 40 bytes
    let rows = Rows::new(
        nt,
        cuts.windows(2).map(|w| w[1] - w[0]).max().unwrap() / 40 + 1,
    );
    let rows = &rows;

    // per thread: endpoint table, its row count in each hour, and its side rows per hour
    let parts: Vec<(Eps, [usize; HOURS], Vec<Vec<u64>>)> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..nt)
            .map(|k| {
                let (lo, hi) = (cuts[k], cuts[k + 1]);
                sc.spawn(move || {
                    let mut e = Eps {
                        slot: vec![u32::MAX; TB],
                        start: Vec::new(),
                        len: Vec::new(),
                        key: Vec::new(),
                        text: vec![0; 64],
                    };
                    let (mut n, mut side) = ([0usize; HOURS], vec![Vec::new(); HOURS]);
                    // walk the LF bits of 64 byte blocks. Each line is parsed from its own two ends,
                    // so finding the next line never waits on parsing this one.
                    let mut start = lo;
                    let mut blk = lo;
                    while blk < hi {
                        let mut m = if blk + 64 <= b.len() {
                            lf64(b, blk)
                        } else {
                            let mut t = [0u8; 64];
                            t[..b.len() - blk].copy_from_slice(&b[blk..]);
                            lf64(&t, 0)
                        };
                        if blk + 64 > hi {
                            m &= (1u64 << (hi - blk)) - 1;
                        }
                        while m != 0 {
                            let lf = blk + m.trailing_zeros() as usize;
                            m &= m - 1;
                            let j = start;
                            start = lf + 1;
                            let c = b;
                            // 2026-MM-DDTHH: day offset from Oct 30, then the hour
                            let ts = ld8(c, j + 5); // "MM-DDTHH" at +5 .. +12, little endian
                            let m1 = (ts >> 8) as u8; // second month digit: '0' for Oct (10), '1' for Nov (11)
                            let dd = (((ts >> 24) as u8 - b'0') * 10 + ((ts >> 32) as u8 - b'0'))
                                as usize;
                            let hh = (((ts >> 48) as u8 - b'0') * 10 + ((ts >> 56) as u8 - b'0'))
                                as usize;
                            let day = if m1 == b'0' { dd - 30 } else { dd + 1 };
                            let hour = day * 24 + hh - 18;
                            let ml = unsafe {
                                *mlen.get_unchecked((c[j + 28] as usize) << 8 | c[j + 29] as usize)
                            } as usize;
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
                                let x = ld8(c, lf - 8)
                                    & 0x0F0F0F0F0F0F0F0F
                                    & (u64::MAX << (8 * (8 - nd)));
                                let x = (x * 10 + (x >> 8)) & 0x00FF00FF00FF00FF;
                                let x = (x * 100 + (x >> 16)) & 0x0000FFFF0000FFFF;
                                (x * 10000 + (x >> 32)) & 0xFFFFFFFF
                            } else {
                                let mut v = 0u64;
                                for &d in &c[s4 + 1..lf] {
                                    v = v * 10 + (d - b'0') as u64;
                                }
                                v
                            };
                            let l = ee - es;
                            let (w0, w1) = if l >= 8 {
                                (ld8(c, es), ld8(c, ee - 8))
                            } else {
                                (ld8(c, es) & ((1u64 << (8 * l)) - 1), 0)
                            };
                            let h = ((w0.wrapping_mul(0x9E3779B97F4A7C15)
                                ^ w1.wrapping_mul(0xC2B2AE3D27D4EB4F)
                                ^ (l as u64).wrapping_mul(0x165667B19E3779F9))
                                >> 50) as usize
                                & (TB - 1);
                            let mut s = h;
                            let id = loop {
                                let x = unsafe { *e.slot.get_unchecked(s) };
                                if x == u32::MAX {
                                    let id = e.start.len() as u32;
                                    // the text keeps 64 zero bytes at its end, so eq() can always read 64 bytes from a start
                                    e.text.truncate(e.text.len() - 64);
                                    e.slot[s] = id;
                                    e.start.push(e.text.len() as u32);
                                    e.len.push(l as u32);
                                    e.text.extend_from_slice(&c[es..ee]);
                                    e.text.extend_from_slice(&[0; 64]);
                                    e.key.push((w0, w1, l as u32));
                                    break id;
                                }
                                let k = unsafe { e.key.get_unchecked(x as usize) };
                                if k.0 == w0
                                    && k.1 == w1
                                    && k.2 == l as u32
                                    && (l <= 16
                                        || eq(&e.text, e.start[x as usize] as usize, c, es, l))
                                {
                                    break x;
                                }
                                s = (s + 1) & (TB - 1);
                            };
                            if v < LAT_ESC as u64 && id < 2047 {
                                unsafe {
                                    *rows.region(hour, k).add(n[hour]) =
                                        id << 21 | (five as u32) << 20 | v as u32
                                };
                                n[hour] += 1;
                            } else {
                                side[hour].push((id as u64) << 33 | v << 1 | five as u64);
                            }
                        }
                        blk += 64;
                    }
                    // drop this share's page table entries now, on this thread, rather than all of
                    // them at exit on one; the text is never read again. madvise, unlike munmap, does
                    // not lock out the other threads' page faults. The thread before may read a few
                    // bytes past its end, so a margin stays mapped at the front.
                    release(&b[(lo + 256).min(hi)..hi]);
                    (e, n, side)
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // global ids in byte order of the endpoint text
    // every thread's endpoints go into one table, hashed on the keys the threads already have
    // (first and last 8 bytes, length) and compared on the text; the distinct names are then
    // sorted once and each thread's ids mapped to their rank
    fn name(e: &Eps, x: usize) -> &[u8] {
        &e.text[e.start[x] as usize..(e.start[x] + e.len[x]) as usize]
    }
    let (eps, counts, sides): (Vec<Eps>, Vec<[usize; HOURS]>, Vec<Vec<Vec<u64>>>) = parts
        .into_iter()
        .fold((Vec::new(), Vec::new(), Vec::new()), |mut a, (e, n, s)| {
            a.0.push(e);
            a.1.push(n);
            a.2.push(s);
            a
        });
    let cap = (2 * eps.iter().map(|e| e.len.len()).sum::<usize>())
        .next_power_of_two()
        .max(16);
    let mut slot = vec![u32::MAX; cap];
    let mut first: Vec<(usize, usize)> = Vec::new(); // (thread, its id) of each distinct name
    let mut maps: Vec<Vec<u32>> = eps
        .iter()
        .enumerate()
        .map(|(pi, e)| {
            (0..e.len.len())
                .map(|x| {
                    let (w0, w1, l) = e.key[x];
                    let mut s = (w0.wrapping_mul(0x9E3779B97F4A7C15)
                        ^ w1.wrapping_mul(0xC2B2AE3D27D4EB4F)
                        ^ (l as u64).wrapping_mul(0x165667B19E3779F9))
                        as usize
                        >> 7
                        & (cap - 1);
                    loop {
                        let g = slot[s];
                        if g == u32::MAX {
                            let n = first.len() as u32;
                            slot[s] = n;
                            first.push((pi, x));
                            break n;
                        }
                        let (qi, y) = first[g as usize];
                        if eps[qi].key[y] == e.key[x] && name(&eps[qi], y) == name(e, x) {
                            break g;
                        }
                        s = (s + 1) & (cap - 1);
                    }
                })
                .collect()
        })
        .collect();
    let mut order: Vec<u32> = (0..first.len() as u32).collect();
    order.sort_unstable_by_key(|&g| name(&eps[first[g as usize].0], first[g as usize].1));
    let all: Vec<&[u8]> = order
        .iter()
        .map(|&g| name(&eps[first[g as usize].0], first[g as usize].1))
        .collect();
    let mut rank = vec![0u32; first.len()];
    for (r, &g) in order.iter().enumerate() {
        rank[g as usize] = r as u32;
    }
    for m in maps.iter_mut() {
        for g in m.iter_mut() {
            *g = rank[*g as usize];
        }
    }
    let ne = all.len();
    // the most text one hour can have: every endpoint, with six numbers of up to 20 digits.
    // An hour's buffer gets that up front, so it never moves; untouched pages cost nothing
    let most: usize = all.iter().map(|e| 21 + e.len() + 6 * 21 + 1).sum();
    // thread pi's rows of hour h, and its side rows
    let rows_of = |pi: usize, h: usize| {
        (
            unsafe { std::slice::from_raw_parts(rows.region(h, pi), counts[pi][h]) },
            &sides[pi][h],
        )
    };

    let next = std::sync::atomic::AtomicUsize::new(0);
    // hours get written in order as soon as they and every hour before them are done, so
    // only a few finished hours ever wait in memory
    std::fs::create_dir_all(&out).ok();
    let file = std::fs::File::create(format!("{out}/summary.txt")).unwrap();
    let wr: std::sync::Mutex<(usize, Vec<Option<Vec<u8>>>, bool)> =
        std::sync::Mutex::new((0, (0..HOURS).map(|_| None).collect(), false));
    std::thread::scope(|sc| {
        for _ in 0..nt {
            sc.spawn(|| {
                let mut cnt = vec![0u32; ne + 1];
                let mut buf: Vec<u32> = Vec::new();
                let mut errv = vec![0u64; ne];
                loop {
                    let h = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if h >= HOURS {
                        break;
                    }
                    let date =
                        ["2026-10-30", "2026-10-31", "2026-11-01", "2026-11-02"][(h + 18) / 24];
                    let hh = (h + 18) % 24;
                    let rs: Vec<_> = (0..maps.len()).map(|pi| rows_of(pi, h)).collect();
                    // counting sort by global endpoint id, keeping latency << 1 | is_5xx
                    cnt.fill(0);
                    for (pi, (r, side)) in rs.iter().enumerate() {
                        let m = &maps[pi];
                        for &x in r.iter() {
                            cnt[m[(x >> 21) as usize] as usize + 1] += 1;
                        }
                        for &x in side.iter() {
                            cnt[m[(x >> 33) as usize] as usize + 1] += 1;
                        }
                    }
                    for g in 0..ne {
                        cnt[g + 1] += cnt[g];
                    }
                    let total = cnt[ne] as usize;
                    buf.clear();
                    buf.resize(total, 0);
                    let mut cur = cnt.clone();
                    for (pi, (r, side)) in rs.iter().enumerate() {
                        let m = &maps[pi];
                        for &x in r.iter() {
                            let g = m[(x >> 21) as usize] as usize;
                            buf[cur[g] as usize] = x & LAT_ESC;
                            errv[g] += (x >> 20 & 1) as u64;
                            cur[g] += 1;
                        }
                        for &x in side.iter() {
                            let g = m[(x >> 33) as usize] as usize;
                            buf[cur[g] as usize] = (x >> 1) as u32;
                            errv[g] += x & 1;
                            cur[g] += 1;
                        }
                    }
                    drop(rs);
                    rows.free(h); // the hour's pages go back while the output grows in the page cache
                    let mut o = Vec::with_capacity(most);
                    let hdr: [u8; 21] = format!("{date}T{hh:02}:00:00Z ")
                        .into_bytes()
                        .try_into()
                        .unwrap();
                    for g in 0..ne {
                        let (s, t) = (cnt[g] as usize, cnt[g + 1] as usize);
                        if s == t {
                            continue;
                        }
                        let grp = &mut buf[s..t];
                        let n = grp.len();
                        let errs = std::mem::take(&mut errv[g]);
                        // nearest rank percentiles: p50 is selected first, then p95 among the rows
                        // above it and p99 among the rows above that, about 1.55 n compares in all
                        // (p99 first and the others below it went over nearly all rows three times)
                        let k = |p: usize| (p * n + 99) / 100 - 1;
                        grp.select_nth_unstable(k(50));
                        if k(95) > k(50) {
                            grp[k(50) + 1..].select_nth_unstable(k(95) - k(50) - 1);
                        }
                        if k(99) > k(95) {
                            grp[k(95) + 1..].select_nth_unstable(k(99) - k(95) - 1);
                        }
                        let pc = |p: usize| grp[k(p)] as u64;
                        line(
                            &mut o,
                            &hdr,
                            all[g],
                            [
                                n as u64,
                                pc(50),
                                pc(95),
                                pc(99),
                                errs,
                                errs * 1_000_000 / n as u64,
                            ],
                        );
                    }
                    // one thread at a time writes, outside the lock, so the others only drop their hour off
                    let mut w = wr.lock().unwrap();
                    w.1[h] = Some(o);
                    if w.2 {
                        continue;
                    }
                    w.2 = true;
                    loop {
                        let mut ready = Vec::new();
                        while w.0 < HOURS {
                            let k = w.0;
                            let Some(x) = w.1[k].take() else { break };
                            ready.push(x);
                            w.0 += 1;
                        }
                        if ready.is_empty() {
                            w.2 = false;
                            break;
                        }
                        drop(w);
                        for x in ready {
                            (&file).write_all(&x).unwrap();
                        }
                        w = wr.lock().unwrap();
                    }
                }
            });
        }
    });
    std::process::exit(0);
}

// one output line, written straight into o's spare room after one reserve: the hour, the
// endpoint, then six numbers, each a space and its decimal digits
fn line(o: &mut Vec<u8>, hdr: &[u8; 21], ep: &[u8], nums: [u64; 6]) {
    const D: &[u8; 200] = b"00010203040506070809101112131415161718192021222324252627282930313233343536373839404142434445464748495051525354555657585960616263646566676869707172737475767778798081828384858687888990919293949596979899";
    o.reserve(hdr.len() + ep.len() + 6 * 21 + 1);
    unsafe {
        let mut p = o.as_mut_ptr().add(o.len());
        std::ptr::copy_nonoverlapping(hdr.as_ptr(), p, hdr.len());
        p = p.add(hdr.len());
        std::ptr::copy_nonoverlapping(ep.as_ptr(), p, ep.len());
        p = p.add(ep.len());
        for mut x in nums {
            *p = b' ';
            // the digits go in from the last one back, two at a time
            let n = x.checked_ilog10().unwrap_or(0) as usize + 1;
            let mut q = p.add(n + 1);
            while x >= 100 {
                q = q.sub(2);
                std::ptr::copy_nonoverlapping(D.as_ptr().add(2 * (x % 100) as usize), q, 2);
                x /= 100;
            }
            if x >= 10 {
                std::ptr::copy_nonoverlapping(D.as_ptr().add(2 * x as usize), q.sub(2), 2);
            } else {
                *q.sub(1) = b'0' + x as u8;
            }
            p = p.add(n + 1);
        }
        *p = b'\n';
        o.set_len(p.add(1).offset_from(o.as_ptr()) as usize);
    }
}

// are x[i..i + l] and y[j..j + l] equal (l > 0)? Two 32 byte compares when 64 bytes can be read from
// both starts, a slice compare otherwise
#[inline(always)]
fn eq(x: &[u8], i: usize, y: &[u8], j: usize, l: usize) -> bool {
    use std::arch::x86_64::*;
    if l <= 64 && i + 64 <= x.len() && j + 64 <= y.len() {
        let same = |k: usize| unsafe {
            _mm256_movemask_epi8(_mm256_cmpeq_epi8(
                _mm256_loadu_si256(x.as_ptr().add(i + k) as *const __m256i),
                _mm256_loadu_si256(y.as_ptr().add(j + k) as *const __m256i),
            )) as u32 as u64
        };
        let want = u64::MAX >> (64 - l);
        return (same(0) | same(32) << 32) & want == want;
    }
    x[i..i + l] == y[j..j + l]
}

// LFs in c[at .. at + 64]
#[inline(always)]
fn lf64(c: &[u8], at: usize) -> u64 {
    use std::arch::x86_64::*;
    unsafe {
        let p = c.as_ptr().add(at) as *const __m256i;
        let lf = _mm256_set1_epi8(b'\n' as i8);
        _mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_loadu_si256(p), lf)) as u32 as u64
            | (_mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_loadu_si256(p.add(1)), lf)) as u32
                as u64)
                << 32
    }
}

// bit i set when c[at + i] is a space
#[inline(always)]
fn sp16(c: &[u8], at: usize) -> u32 {
    use std::arch::x86_64::*;
    unsafe {
        _mm_movemask_epi8(_mm_cmpeq_epi8(
            _mm_loadu_si128(c.as_ptr().add(at) as *const __m128i),
            _mm_set1_epi8(b' ' as i8),
        )) as u32
    }
}

// MADV_DONTNEED on the whole pages inside `b`: the file's pages stay in the page cache, only
// this process's mapping of them goes
fn release(b: &[u8]) {
    let (a, e) = (
        (b.as_ptr() as usize + 4095) & !4095,
        (b.as_ptr() as usize + b.len()) & !4095,
    );
    if a < e {
        unsafe { madvise(a as *mut u8, e - a, 4) };
    }
}

// read-only mmap of the input; page cache pages are not charged to us
fn map(path: &str) -> &'static [u8] {
    use std::os::unix::io::AsRawFd;
    extern "C" {
        fn mmap(a: *mut u8, l: usize, p: i32, f: i32, fd: i32, o: i64) -> *mut u8;
    }
    let f = std::fs::File::open(path).unwrap();
    let len = f.metadata().unwrap().len() as usize;
    if len == 0 {
        return &[];
    }
    let flags = 2; // MAP_PRIVATE
    unsafe {
        std::slice::from_raw_parts(
            mmap(std::ptr::null_mut(), len, 1, flags, f.as_raw_fd(), 0),
            len,
        )
    }
}
