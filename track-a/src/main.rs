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

const HOURS: usize = 72; // 2026-10-30T18 .. 2026-11-02T17

// index of the first LF, eight bytes at a time
fn newline(b: &[u8]) -> usize {
    let mut i = 0;
    while i + 8 <= b.len() {
        let x = u64::from_le_bytes(b[i..i + 8].try_into().unwrap()) ^ 0x0a0a0a0a0a0a0a0a;
        let z = x.wrapping_sub(0x0101010101010101) & !x & 0x8080808080808080;
        if z != 0 { return i + (z.trailing_zeros() / 8) as usize; }
        i += 8;
    }
    i + b[i..].iter().position(|&x| x == b'\n').unwrap()
}

fn d2(b: &[u8]) -> usize { ((b[0] - b'0') * 10 + (b[1] - b'0')) as usize }

// per-thread state: endpoint -> local id, and for every (local id, hour) its latencies and 5xx count
struct Part<'a> { ids: FxMap<&'a [u8], u32>, eps: Vec<&'a [u8]>, lat: Vec<Vec<u32>>, err: Vec<u32> }

fn main() {
    let data = std::env::var("DATA").unwrap_or("/data".into());
    let out = std::env::var("OUT").unwrap_or("/out".into());
    let b = map(&format!("{data}/requests.log"));
    let nt = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16);
    let mut cuts = vec![0usize];
    for k in 1..nt {
        let mut p = (b.len() * k / nt).max(*cuts.last().unwrap());
        while p < b.len() && p > 0 && b[p - 1] != b'\n' { p += 1; }
        cuts.push(p);
    }
    cuts.push(b.len());

    let parts: Vec<Part> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..nt).map(|k| {
            let c = &b[cuts[k]..cuts[k + 1]];
            sc.spawn(move || {
                let mut p = Part { ids: FxMap::default(), eps: Vec::new(), lat: Vec::new(), err: Vec::new() };
                let mut i = 0;
                while i < c.len() {
                    let ts = &c[i..i + 27];
                    // day offset from 2026-10-30: Oct 30 -> 0, Oct 31 -> 1, Nov 1 -> 2, Nov 2 -> 3
                    let day = if ts[6] == b'0' { d2(&ts[8..]) - 30 } else { d2(&ts[8..]) + 1 };
                    let hour = day * 24 + d2(&ts[11..]) - 18;
                    // method length from its first two bytes: GET PUT POST PATCH DELETE HEAD OPTIONS
                    let ml = match (c[i + 28], c[i + 29]) { (b'G', _) | (b'P', b'U') => 3, (b'P', b'O') | (b'H', _) => 4, (b'P', _) => 5, (b'D', _) => 6, _ => 7 };
                    let es = i + 29 + ml;
                    let nl = es + newline(&c[es..]);
                    // walk back over the latency digits; the status is the 3 bytes before them
                    let mut k = nl - 1;
                    let mut v = 0u32;
                    let mut mul = 1u32;
                    while c[k] != b' ' { v += (c[k] - b'0') as u32 * mul; mul = mul.wrapping_mul(10); k -= 1; }
                    let five = c[k - 3] == b'5';
                    let ep = &c[es..k - 4];
                    i = nl + 1;
                    let n = p.ids.len() as u32;
                    let id = *p.ids.entry(ep).or_insert_with(|| n);
                    if id == n {
                        p.eps.push(ep);
                        p.lat.resize_with((n as usize + 1) * HOURS, Vec::new);
                        p.err.resize((n as usize + 1) * HOURS, 0);
                    }
                    let g = id as usize * HOURS + hour;
                    p.lat[g].push(v);
                    p.err[g] += five as u32;
                }
                p
            })
        }).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // global endpoint order is plain byte order, which is what Vec<&[u8]>::sort gives
    let mut all: Vec<&[u8]> = parts.iter().flat_map(|p| p.eps.iter().copied()).collect();
    all.sort_unstable(); all.dedup();
    // local id -> global id, per part
    let maps: Vec<Vec<u32>> = parts.iter().map(|p| p.eps.iter().map(|e| all.binary_search(e).unwrap() as u32).collect()).collect();
    // for each global endpoint, the (part, local id) pairs that hold it
    let mut holders: Vec<Vec<(usize, usize)>> = vec![Vec::new(); all.len()];
    for (pi, m) in maps.iter().enumerate() { for (li, &g) in m.iter().enumerate() { holders[g as usize].push((pi, li)); } }

    let ne = all.len();
    let lines: Vec<Vec<u8>> = {
        let next = AtomicUsize::new(0);
        let slots: Vec<std::sync::Mutex<Vec<u8>>> = (0..HOURS).map(|_| Default::default()).collect();
        std::thread::scope(|sc| {
            for _ in 0..nt {
                sc.spawn(|| {
                    let mut buf: Vec<u32> = Vec::new();
                    loop {
                        let h = next.fetch_add(1, Ordering::Relaxed);
                        if h >= HOURS { break; }
                        let day = (h + 18) / 24;
                        let hh = (h + 18) % 24;
                        let date = ["2026-10-30", "2026-10-31", "2026-11-01", "2026-11-02"][day];
                        let mut o = Vec::new();
                        for g in 0..ne {
                            buf.clear();
                            let mut errs = 0u64;
                            for &(pi, li) in &holders[g] {
                                buf.extend_from_slice(&parts[pi].lat[li * HOURS + h]);
                                errs += parts[pi].err[li * HOURS + h] as u64;
                            }
                            let n = buf.len();
                            if n == 0 { continue; }
                            buf.sort_unstable();
                            let pc = |p: usize| buf[(p * n + 99) / 100 - 1];
                            write!(o, "{date}T{hh:02}:00:00Z ").unwrap();
                            o.extend_from_slice(all[g]);
                            writeln!(o, " {} {} {} {} {} {}", n, pc(50), pc(95), pc(99), errs, errs * 1_000_000 / n as u64).unwrap();
                        }
                        *slots[h].lock().unwrap() = o;
                    }
                });
            }
        });
        slots.into_iter().map(|m| m.into_inner().unwrap()).collect()
    };
    std::fs::create_dir_all(&out).ok();
    let mut f = std::fs::File::create(format!("{out}/summary.txt")).unwrap();
    for l in &lines { f.write_all(l).unwrap(); }
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
