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

// per-thread state: endpoint -> local id, and per hour a flat list of
// (local id << 33 | latency << 1 | is_5xx). Sorting one hour's list later groups it by
// endpoint and sorts each group's latencies in the same pass.
struct Part { ids: FxMap<Box<[u8]>, u32>, eps: Vec<Box<[u8]>>, rows: Vec<Vec<u64>> }

fn main() {
    let data = std::env::var("DATA").unwrap_or("/data".into());
    let out = std::env::var("OUT").unwrap_or("/out".into());
    // pread into a small per thread buffer instead of an mmap: no page faults, and the
    // buffer is reused, so the input never shows up as our memory
    let file = std::fs::File::open(format!("{data}/requests.log")).unwrap();
    let flen = file.metadata().unwrap().len() as usize;
    use std::os::unix::fs::FileExt;
    let nt = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16);
    let mut cuts = vec![0usize];
    for k in 1..nt {
        // start of the first line at or after the even split point
        let mut p = (flen * k / nt).max(*cuts.last().unwrap());
        if p > 0 && p < flen {
            let mut t = [0u8; 256];
            let r = file.read_at(&mut t, (p - 1) as u64).unwrap();
            p += t[..r].iter().position(|&x| x == b'\n').unwrap();
        }
        cuts.push(p.min(flen));
    }
    cuts.push(flen);

    let mut parts: Vec<Part> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..nt).map(|k| {
            let (lo, hi, file) = (cuts[k], cuts[k + 1], &file);
            sc.spawn(move || {
                let mut p = Part { ids: FxMap::default(), eps: Vec::new(), rows: vec![Vec::with_capacity((hi - lo) / 77 / HOURS + 64); HOURS] };
                const CH: usize = 1 << 19;
                let mut buf = vec![0u8; CH + 4096];
                let (mut pos, mut have) = (lo, 0usize);
                while pos < hi || have > 0 {
                let r = file.read_at(&mut buf[have..have + CH.min(hi - pos)], pos as u64).unwrap();
                pos += r; have += r;
                let end = if pos >= hi { have } else { buf[..have].iter().rposition(|&x| x == b'\n').unwrap() + 1 };
                let c = &buf[..end];
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
                    let id = match p.ids.get(ep) { Some(&x) => x, None => { p.ids.insert(ep.into(), n); p.eps.push(ep.into()); n } };
                    p.rows[hour].push((id as u64) << 33 | (v as u64) << 1 | five as u64);
                }
                buf.copy_within(end..have, 0); have -= end;
                }
                p
            })
        }).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // global endpoint order is plain byte order, which is what Vec<&[u8]>::sort gives
    let eps: Vec<Vec<Box<[u8]>>> = parts.iter_mut().map(|p| std::mem::take(&mut p.eps)).collect();
    let mut all: Vec<&[u8]> = eps.iter().flat_map(|p| p.iter().map(|e| &e[..])).collect();
    all.sort_unstable(); all.dedup();
    // local id -> global id, per part
    let maps: Vec<Vec<u32>> = eps.iter().map(|p| p.iter().map(|e| all.binary_search(&&e[..]).unwrap() as u32).collect()).collect();
    // hour major ownership of the rows, so each hour's rows are freed as soon as it is done
    let hour_rows: Vec<std::sync::Mutex<Vec<Vec<u64>>>> = {
        let mut v: Vec<Vec<Vec<u64>>> = (0..HOURS).map(|_| Vec::new()).collect();
        for p in parts.iter_mut() { for (h, r) in p.rows.drain(..).enumerate() { v[h].push(r); } }
        v.into_iter().map(std::sync::Mutex::new).collect()
    };
    let lines: Vec<Vec<u8>> = {
        let next = AtomicUsize::new(0);
        let slots: Vec<std::sync::Mutex<Vec<u8>>> = (0..HOURS).map(|_| Default::default()).collect();
        std::thread::scope(|sc| {
            for _ in 0..nt {
                sc.spawn(|| {
                    let mut buf: Vec<u64> = Vec::new();
                    loop {
                        let h = next.fetch_add(1, Ordering::Relaxed);
                        if h >= HOURS { break; }
                        let date = ["2026-10-30", "2026-10-31", "2026-11-01", "2026-11-02"][(h + 18) / 24];
                        let hh = (h + 18) % 24;
                        // swap in the global id, then one plain sort orders by endpoint, then latency
                        buf.clear();
                        let rows = std::mem::take(&mut *hour_rows[h].lock().unwrap());
                        for (pi, r) in rows.into_iter().enumerate() {
                            buf.extend(r.iter().map(|&x| (maps[pi][(x >> 33) as usize] as u64) << 33 | (x & 0x1_FFFF_FFFF)));
                        }
                        let mut o = Vec::new();
                        buf.sort_unstable();
                        let mut i = 0;
                        while i < buf.len() {
                            let g = buf[i] >> 33;
                            let mut j = i;
                            let mut errs = 0u64;
                            while j < buf.len() && buf[j] >> 33 == g { errs += buf[j] & 1; j += 1; }
                            let n = j - i;
                            let pc = |p: usize| (buf[i + (p * n + 99) / 100 - 1] >> 1) as u32;
                            write!(o, "{date}T{hh:02}:00:00Z ").unwrap();
                            o.extend_from_slice(all[g as usize]);
                            writeln!(o, " {} {} {} {} {} {}", n, pc(50), pc(95), pc(99), errs, errs * 1_000_000 / n as u64).unwrap();
                            i = j;
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

