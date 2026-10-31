use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::io::Write;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};

fn num(b: &[u8], i: &mut usize) -> u32 {
    let mut v = 0u32;
    while b[*i] >= b'0' { v = v * 10 + (b[*i] - b'0') as u32; *i += 1; }
    *i += 1; // skip the separator
    v
}

struct Ptr(*mut u64);
unsafe impl Sync for Ptr {}
struct Ptr2(*mut (u32, u32));
unsafe impl Sync for Ptr2 {}

fn cuts(b: &[u8], nt: usize) -> Vec<usize> {
    let mut c = vec![0usize];
    for k in 1..nt {
        let mut p = (b.len() * k / nt).max(*c.last().unwrap());
        while p < b.len() && p > 0 && b[p - 1] != b'\n' { p += 1; }
        c.push(p);
    }
    c.push(b.len());
    c
}

fn main() {
    let data = std::env::var("DATA").unwrap_or("/data".into());
    let out = std::env::var("OUT").unwrap_or("/out".into());
    let g = map(&format!("{data}/graph.txt"));
    let mut i = 0;
    let n = num(g, &mut i) as usize;
    let _m = num(g, &mut i);
    let body = &g[i..];
    let nt = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16);
    let ct = cuts(body, nt);

    // pass 1, in parallel: parse edges into per thread lists and count out degrees
    let deg: Vec<AtomicU32> = (0..n + 1).map(|_| AtomicU32::new(0)).collect();
    let parts: Vec<Vec<[u32; 3]>> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..nt).map(|k| {
            let (c, deg) = (&body[ct[k]..ct[k + 1]], &deg);
            sc.spawn(move || {
                let mut v = Vec::with_capacity(c.len() / 15);
                let mut j = 0;
                while j < c.len() {
                    let (a, b, w) = (num(c, &mut j), num(c, &mut j), num(c, &mut j));
                    if a != b { deg[a as usize].fetch_add(1, Relaxed); v.push([a, b, w]); } // self loops never help
                }
                v
            })
        }).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut off = vec![0u32; n + 1];
    for k in 0..n { off[k + 1] = off[k] + deg[k].load(Relaxed); }
    let cur: Vec<AtomicU32> = off[..n].iter().map(|&x| AtomicU32::new(x)).collect();
    let total = off[n] as usize;
    let mut adj: Vec<u64> = vec![0; total];
    let ap = Ptr(adj.as_mut_ptr());
    std::thread::scope(|sc| {
        for p in &parts {
            let (cur, ap) = (&cur, &ap);
            // every slot index comes from a unique fetch_add, so no two writes alias
            sc.spawn(move || for e in p {
                let s = cur[e[0] as usize].fetch_add(1, Relaxed);
                unsafe { *ap.0.add(s as usize) = (e[1] as u64) << 32 | e[2] as u64 };
            });
        }
    });
    drop(parts);

    // relabel nodes in BFS order so graph neighbours sit close in memory. Each thread grows
    // its own BFS regions and claims nodes with a CAS; the final order is the threads' lists
    // concatenated. Which thread wins a node only changes the layout, never an answer.
    let owner: Vec<AtomicU32> = (0..n).map(|_| AtomicU32::new(u32::MAX)).collect();
    let lists: Vec<Vec<u32>> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..nt).map(|k| {
            let (owner, off, adj) = (&owner, &off, &adj);
            sc.spawn(move || {
                let mut ord: Vec<u32> = Vec::with_capacity(n / nt + 1024);
                let claim = |x: usize| owner[x].compare_exchange(u32::MAX, k as u32, Relaxed, Relaxed).is_ok();
                for r in k * n / nt..(k + 1) * n / nt {
                    if !claim(r) { continue; }
                    let mut h = ord.len();
                    ord.push(r as u32);
                    while h < ord.len() {
                        let u = ord[h] as usize; h += 1;
                        for e in &adj[off[u] as usize..off[u + 1] as usize] {
                            let v = (e >> 32) as usize;
                            if owner[v].load(Relaxed) == u32::MAX && claim(v) { ord.push(v as u32); }
                        }
                    }
                }
                ord
            })
        }).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    drop(owner);
    let order: Vec<u32> = lists.concat();
    let mut newid = vec![0u32; n];
    for (k, &u) in order.iter().enumerate() { newid[u as usize] = k as u32; }
    let mut noff = vec![0u32; n + 1];
    for (k, &u) in order.iter().enumerate() { noff[k + 1] = noff[k] + off[u as usize + 1] - off[u as usize]; }
    let mut nadj: Vec<(u32, u32)> = Vec::with_capacity(total);
    unsafe { nadj.set_len(total) }; // every slot is written below
    {
        let chunk = (n + nt - 1) / nt;
        let mut rest: &mut [(u32, u32)] = &mut nadj;
        let mut slices = Vec::new();
        for k in 0..nt {
            let (lo, hi) = ((k * chunk).min(n), ((k + 1) * chunk).min(n));
            let (a, b) = rest.split_at_mut((noff[hi] - noff[lo]) as usize);
            slices.push((lo, hi, a)); rest = b;
        }
        std::thread::scope(|sc| {
            for (lo, hi, s) in slices {
                let (order, off, adj, newid) = (&order, &off, &adj, &newid);
                sc.spawn(move || {
                    let mut p = 0;
                    for k in lo..hi {
                        let u = order[k] as usize;
                        for e in &adj[off[u] as usize..off[u + 1] as usize] {
                            let x = *e;
                            s[p] = (newid[(x >> 32) as usize], x as u32); p += 1;
                        }
                    }
                });
            }
        });
    }
    drop(adj);
    let (off, adj) = (noff, nadj);
    // reverse graph for the backward search
    let rdeg: Vec<AtomicU32> = (0..n).map(|_| AtomicU32::new(0)).collect();
    let per = (total + nt - 1) / nt;
    std::thread::scope(|sc| {
        for k in 0..nt {
            let (adj, rdeg) = (&adj, &rdeg);
            sc.spawn(move || for e in &adj[(k * per).min(total)..((k + 1) * per).min(total)] { rdeg[e.0 as usize].fetch_add(1, Relaxed); });
        }
    });
    let mut roff = vec![0u32; n + 1];
    for k in 0..n { roff[k + 1] = roff[k] + rdeg[k].load(Relaxed); }
    let rcur: Vec<AtomicU32> = roff[..n].iter().map(|&x| AtomicU32::new(x)).collect();
    let mut radj: Vec<(u32, u32)> = Vec::with_capacity(total);
    unsafe { radj.set_len(total) };
    let rp = Ptr2(radj.as_mut_ptr());
    std::thread::scope(|sc| {
        let chunk = (n + nt - 1) / nt;
        for k in 0..nt {
            let (adj, off, rcur, rp) = (&adj, &off, &rcur, &rp);
            sc.spawn(move || for u in (k * chunk).min(n)..((k + 1) * chunk).min(n) {
                for &(v, w) in &adj[off[u] as usize..off[u + 1] as usize] {
                    let s = rcur[v as usize].fetch_add(1, Relaxed);
                    unsafe { *rp.0.add(s as usize) = (u as u32, w) };
                }
            });
        }
    });

    let q = std::fs::read(format!("{data}/queries.txt")).unwrap();
    let mut qs = Vec::new();
    let mut j = 0;
    while j < q.len() { let s = num(&q, &mut j); let t = num(&q, &mut j); qs.push((newid[s as usize], newid[t as usize])); }

    let ans: Vec<AtomicU64> = (0..qs.len()).map(|_| AtomicU64::new(0)).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|sc| {
        for _ in 0..nt {
            sc.spawn(|| {
                // bidirectional Dijkstra: side 0 runs forward from s, side 1 backward from t
                let mut dist = [vec![u64::MAX; n], vec![u64::MAX; n]];
                let mut touched: [Vec<u32>; 2] = [Vec::new(), Vec::new()];
                let mut heap: [BinaryHeap<Reverse<(u64, u32)>>; 2] = [BinaryHeap::new(), BinaryHeap::new()];
                let g = [(&off, &adj), (&roff, &radj)];
                loop {
                    let k = next.fetch_add(1, Relaxed);
                    if k >= qs.len() { break; }
                    let (s, t) = qs[k];
                    let mut best = u64::MAX;
                    if s == t { best = 0; } else {
                        for (side, x) in [(0, s), (1, t)] {
                            dist[side][x as usize] = 0; touched[side].push(x); heap[side].push(Reverse((0, x)));
                        }
                        loop {
                            let tf = heap[0].peek().map(|r| r.0 .0);
                            let tb = heap[1].peek().map(|r| r.0 .0);
                            let (Some(tf), Some(tb)) = (tf, tb) else { break };
                            if tf.saturating_add(tb) >= best { break; }
                            let side = if heap[0].len() <= heap[1].len() { 0 } else { 1 };
                            let Reverse((d, u)) = heap[side].pop().unwrap();
                            if d > dist[side][u as usize] { continue; }
                            let (o, a) = g[side];
                            let [df, db] = &mut dist;
                            let (mine, other) = if side == 0 { (df, &*db) } else { (db, &*df) };
                            for &(v, w) in &a[o[u as usize] as usize..o[u as usize + 1] as usize] {
                                let nd = d + w as u64;
                                let dv = &mut mine[v as usize];
                                if nd < *dv {
                                    if *dv == u64::MAX { touched[side].push(v); }
                                    *dv = nd;
                                    heap[side].push(Reverse((nd, v)));
                                    let ov = other[v as usize];
                                    if ov != u64::MAX && nd + ov < best { best = nd + ov; }
                                }
                            }
                        }
                        for side in 0..2 {
                            for &x in &touched[side] { dist[side][x as usize] = u64::MAX; }
                            touched[side].clear(); heap[side].clear();
                        }
                    }
                    let res = best;
                    ans[k].store(res, Relaxed);
                }
            });
        }
    });
    let mut o = Vec::with_capacity(qs.len() * 12);
    for a in &ans {
        let v = a.load(Relaxed);
        if v == u64::MAX { o.extend_from_slice(b"-1\n"); } else { writeln!(o, "{v}").unwrap(); }
    }
    std::fs::create_dir_all(&out).ok();
    std::fs::write(format!("{out}/distances.txt"), o).unwrap();
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
