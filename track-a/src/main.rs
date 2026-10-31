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

fn d2(b: &[u8]) -> usize { ((b[0] - b'0') * 10 + (b[1] - b'0')) as usize }

// per-thread state: endpoint -> local id, and for every (local id, hour) its latencies and 5xx count
struct Part<'a> { ids: FxMap<&'a [u8], u32>, eps: Vec<&'a [u8]>, lat: Vec<Vec<u32>>, err: Vec<u32> }

fn main() {
    let data = std::env::var("DATA").unwrap_or("/data".into());
    let out = std::env::var("OUT").unwrap_or("/out".into());
    let b = std::fs::read(format!("{data}/requests.log")).unwrap();
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
                    let mut j = i + 28;
                    while c[j] != b' ' { j += 1; } // method
                    j += 1;
                    let es = j;
                    while c[j] != b' ' { j += 1; }
                    let ep = &c[es..j];
                    let five = c[j + 1] == b'5';
                    j += 5;
                    let mut v = 0u32;
                    while c[j] != b'\n' { v = v * 10 + (c[j] - b'0') as u32; j += 1; }
                    i = j + 1;
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
    std::fs::write(format!("{out}/summary.txt"), lines.concat()).unwrap();
}
