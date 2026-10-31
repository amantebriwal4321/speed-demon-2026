use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

fn num(b: &[u8], i: &mut usize) -> u64 {
    let mut v = 0u64;
    while *i < b.len() && b[*i] >= b'0' {
        v = v * 10 + (b[*i] - b'0') as u64;
        *i += 1;
    }
    *i += 1; // skip the separator
    v
}

fn threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16)
}

fn main() {
    let data = std::env::var("DATA").unwrap_or("/data".into());
    let out = std::env::var("OUT").unwrap_or("/out".into());
    let g = std::fs::read(format!("{data}/graph.txt")).unwrap();
    let mut i = 0;
    let n = num(&g, &mut i) as usize;
    let _m = num(&g, &mut i);
    let body = &g[i..];
    let nt = threads();

    // split the edge lines into nt chunks on line boundaries and parse them in parallel
    let mut cuts = vec![0usize];
    for k in 1..nt {
        let mut p = body.len() * k / nt;
        while p < body.len() && body[p - 1] != b'\n' { p += 1; }
        cuts.push(p.max(*cuts.last().unwrap()));
    }
    cuts.push(body.len());
    let parts: Vec<Vec<(u32, u32, u32)>> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..nt).map(|k| {
            let c = &body[cuts[k]..cuts[k + 1]];
            sc.spawn(move || {
                let mut v = Vec::with_capacity(c.len() / 16);
                let mut j = 0;
                while j < c.len() {
                    let u = num(c, &mut j) as u32;
                    let w = num(c, &mut j) as u32;
                    let x = num(c, &mut j) as u32;
                    v.push((u, w, x));
                }
                v
            })
        }).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // CSR adjacency, self-loops dropped (they never shorten a path)
    let mut off = vec![0u32; n + 1];
    for p in &parts { for &(u, v, _) in p { if u != v { off[u as usize + 1] += 1; } } }
    for k in 0..n { off[k + 1] += off[k]; }
    let total = off[n] as usize;
    let mut fill = off.clone();
    let mut adj = vec![(0u32, 0u32); total];
    for p in &parts {
        for &(u, v, w) in p {
            if u != v {
                let s = &mut fill[u as usize];
                adj[*s as usize] = (v, w);
                *s += 1;
            }
        }
    }
    drop(parts);

    let q = std::fs::read(format!("{data}/queries.txt")).unwrap();
    let mut qs = Vec::new();
    let mut j = 0;
    while j < q.len() { let s = num(&q, &mut j) as u32; let t = num(&q, &mut j) as u32; qs.push((s, t)); }

    let ans: Vec<std::sync::atomic::AtomicU64> = (0..qs.len()).map(|_| Default::default()).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|sc| {
        for _ in 0..nt {
            sc.spawn(|| {
                let mut dist = vec![u64::MAX; n];
                let mut touched: Vec<u32> = Vec::new();
                let mut heap: BinaryHeap<Reverse<(u64, u32)>> = BinaryHeap::new();
                loop {
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    if k >= qs.len() { break; }
                    let (s, t) = qs[k];
                    let mut res = u64::MAX;
                    if s == t { res = 0; } else {
                        dist[s as usize] = 0; touched.push(s);
                        heap.push(Reverse((0, s)));
                        while let Some(Reverse((d, u))) = heap.pop() {
                            if d > dist[u as usize] { continue; }
                            if u == t { res = d; break; }
                            let (a, b) = (off[u as usize] as usize, off[u as usize + 1] as usize);
                            for &(v, w) in &adj[a..b] {
                                let nd = d + w as u64;
                                let dv = &mut dist[v as usize];
                                if nd < *dv {
                                    if *dv == u64::MAX { touched.push(v); }
                                    *dv = nd;
                                    heap.push(Reverse((nd, v)));
                                }
                            }
                        }
                        for &x in &touched { dist[x as usize] = u64::MAX; }
                        touched.clear(); heap.clear();
                    }
                    ans[k].store(res, Ordering::Relaxed);
                }
            });
        }
    });

    let mut o = Vec::with_capacity(qs.len() * 12);
    for a in &ans {
        let v = a.load(Ordering::Relaxed);
        if v == u64::MAX { o.extend_from_slice(b"-1\n"); } else { writeln!(o, "{v}").unwrap(); }
    }
    std::fs::create_dir_all(&out).ok();
    std::fs::write(format!("{out}/distances.txt"), o).unwrap();
}
