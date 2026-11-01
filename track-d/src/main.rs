use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};

#[inline(always)]
fn num(b: &[u8], i: &mut usize) -> u32 {
    // eight bytes at once: the first byte below '0' ends the number, then SWAR the digits
    if *i + 8 <= b.len() {
        let x = u64::from_le_bytes(b[*i..*i + 8].try_into().unwrap());
        let d = x.wrapping_sub(0x3030303030303030);
        let stop = (d | (d.wrapping_add(0x7676767676767676))) & 0x8080808080808080; // byte < '0' or > '9'
        if stop != 0 {
            let n = (stop.trailing_zeros() / 8) as usize;
            if n > 0 {
                let x = (d << (8 * (8 - n))) & 0x0F0F0F0F0F0F0F0F;
                let x = (x * 10 + (x >> 8)) & 0x00FF00FF00FF00FF;
                let x = (x * 100 + (x >> 16)) & 0x0000FFFF0000FFFF;
                let x = (x * 10000 + (x >> 32)) & 0xFFFFFFFF;
                *i += n + 1;
                return x as u32;
            }
        }
    }
    let mut v = 0u32;
    while b[*i] >= b'0' {
        v = v * 10 + (b[*i] - b'0') as u32;
        *i += 1;
    }
    *i += 1; // skip the separator
    v
}

// distances of the nodes a search has reached, in a small open addressing table that starts
// at 1K slots (8 KB) and grows: half the searches pop under 130 nodes, and two such tables a
// thread, next to a hyperthread's, stay in L1 and L2. Values are u32, with the rare one above
// 2^32 - 3 kept in a side map.
const SPILL: u32 = u32::MAX - 1;
// slot = node << 32 | dist, all ones when empty: key and value share one load
struct Dist {
    t: Vec<u64>,
    len: usize,
    big: std::collections::HashMap<u32, u64>,
    used: Vec<u32>,
}
const E: u64 = u64::MAX;
impl Dist {
    fn new() -> Self {
        Dist {
            t: vec![E; 1 << 10],
            len: 0,
            big: Default::default(),
            used: Vec::new(),
        }
    }
    #[inline]
    fn slot(&self, v: u32) -> usize {
        let m = self.t.len() - 1;
        let mut i = (v.wrapping_mul(0x9E3779B1) as usize >> 4) & m;
        loop {
            let k = self.t[i];
            if k == E || (k >> 32) as u32 == v {
                return i;
            }
            i = (i + 1) & m;
        }
    }
    #[inline]
    fn get(&self, v: u32) -> u64 {
        let k = self.t[self.slot(v)];
        if k == E {
            return u64::MAX;
        }
        match k as u32 {
            SPILL => self.big[&v],
            x => x as u64,
        }
    }
    // set with the slot already found by slot(v)
    #[inline(always)]
    fn set_at(&mut self, mut i: usize, v: u32, d: u64) {
        if self.t[i] == E {
            if (self.len + 1) * 2 > self.t.len() {
                self.grow();
                i = self.slot(v);
            }
            self.len += 1;
            self.track(i);
        }
        let x = if d < SPILL as u64 {
            d as u32
        } else {
            self.big.insert(v, d);
            SPILL
        };
        self.t[i] = (v as u64) << 32 | x as u64;
    }
    fn grow(&mut self) {
        let c = self.t.len() * 2;
        let old = std::mem::replace(&mut self.t, vec![E; c]);
        self.used.clear();
        for k in old {
            if k != E {
                let i = self.slot((k >> 32) as u32);
                self.t[i] = k;
                self.track(i);
            }
        }
    }
    // the filled slots, kept only while few enough that clearing them one by one beats a fill
    fn track(&mut self, i: usize) {
        if self.used.len() * 16 < self.t.len() {
            self.used.push(i as u32);
        }
    }
    // most searches touch a few thousand slots of a table grown by an earlier big one
    fn clear(&mut self) {
        // the few searches that grow a table past 64K slots give it back afterwards
        if self.t.len() > 1 << 16 {
            *self = Dist::new();
            return;
        }
        if (self.used.len() + 1) * 16 < self.t.len() {
            for &i in &self.used {
                self.t[i as usize] = E;
            }
        } else {
            self.t.fill(E);
        }
        self.used.clear();
        self.len = 0;
        self.big.clear();
    }
}

struct Ptr2(*mut u32);
unsafe impl Sync for Ptr2 {}

fn cuts(b: &[u8], nt: usize) -> Vec<usize> {
    let mut c = vec![0usize];
    for k in 1..nt {
        let mut p = (b.len() * k / nt).max(*c.last().unwrap());
        while p < b.len() && p > 0 && b[p - 1] != b'\n' {
            p += 1;
        }
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
    let nt = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(16);
    let ct = cuts(body, nt);

    // parse in parallel, sorting each edge into a bucket by its tail's node range; then each
    // bucket's thread builds the CSR for its own node range, with no atomics
    // bucket of node a is a * nt / n, so bucket b holds nodes ceil(b * n / nt) .. ceil((b + 1) * n / nt)
    // an edge is one u32: head in the low vb bits, weight above it. A weight that does not fit
    // (0.07% of them) is stored as all ones there, with the true one in `big` by (tail, head);
    // parallel big edges keep the smallest, the only one a shortest path can use
    let vb = 32 - (n.max(2) as u32 - 1).leading_zeros();
    let (vm, esc) = ((1u32 << vb) - 1, u32::MAX >> vb);
    let pk = |v: u32, w: u32| w.min(esc) << vb | v;
    let range = |b: usize| ((b * n + nt - 1) / nt, ((b + 1) * n + nt - 1) / nt);
    let parts: Vec<(Vec<Vec<u64>>, HashMap<(u32, u32), u32>)> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..nt)
            .map(|k| {
                let c = &body[ct[k]..ct[k + 1]];
                sc.spawn(move || {
                    // each bucket sized for its share of the lines up front (vec![v; nt] would clone
                    // the first, and a clone of an empty Vec has no capacity)
                    let mut v: Vec<Vec<u64>> = (0..nt)
                        .map(|_| Vec::with_capacity(c.len() / 15 / nt + 16))
                        .collect();
                    let mut big: HashMap<(u32, u32), u32> = HashMap::new();
                    let mut j = 0;
                    while j < c.len() {
                        let (a, b, w) = (num(c, &mut j), num(c, &mut j), num(c, &mut j));
                        if a == b {
                            continue;
                        } // self loops never help
                        v[(a as u64 * nt as u64 / n as u64) as usize]
                            .push((a as u64) << 32 | pk(b, w) as u64);
                        if w >= esc {
                            let x = big.entry((a, b)).or_insert(w);
                            *x = (*x).min(w);
                        }
                    }
                    // drop this share's page table entries now, on this thread, rather than all of
                    // them at exit on one; the text is never read again. madvise, unlike munmap, does
                    // not lock out the other threads' page faults. The thread before may read a few
                    // bytes past its end, so a margin stays mapped at the front.
                    release(&c[256.min(c.len())..]);
                    (v, big)
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let sizes: Vec<usize> = (0..nt)
        .map(|b| parts.iter().map(|p| p.0[b].len()).sum())
        .collect();
    let mut big: HashMap<(u32, u32), u32> = HashMap::new();
    for (_, m) in &parts {
        for (&k, &w) in m {
            let x = big.entry(k).or_insert(w);
            *x = (*x).min(w);
        }
    }
    let total: usize = sizes.iter().sum();
    let mut off = vec![0u32; n + 1];
    let mut adj: Vec<u32> = vec![0; total];
    {
        let (mut orest, mut arest): (&mut [u32], &mut [u32]) = (&mut off[..n], &mut adj);
        let mut jobs = Vec::new();
        let mut base = 0u32;
        for b in 0..nt {
            let (lo, hi) = range(b);
            let (o, r) = orest.split_at_mut(hi - lo);
            orest = r;
            let (a, r) = arest.split_at_mut(sizes[b]);
            arest = r;
            jobs.push((b, lo, o, a, base));
            base += sizes[b] as u32;
        }
        std::thread::scope(|sc| {
            for (b, lo, o, a, base) in jobs {
                let parts = &parts;
                sc.spawn(move || {
                    for p in parts {
                        for e in &p.0[b] {
                            o[(e >> 32) as usize - lo] += 1;
                        }
                    }
                    let mut acc = 0u32;
                    for x in o.iter_mut() {
                        let d = *x;
                        *x = acc;
                        acc += d;
                    }
                    let mut cur: Vec<u32> = o.to_vec();
                    for p in parts {
                        for e in &p.0[b] {
                            let s = &mut cur[(e >> 32) as usize - lo];
                            a[*s as usize] = *e as u32;
                            *s += 1;
                        }
                    }
                    for x in o.iter_mut() {
                        *x += base;
                    }
                });
            }
        });
    }
    off[n] = total as u32;
    drop(parts);

    // relabel nodes in BFS order so graph neighbours sit close in memory. Each thread grows
    // its own BFS regions and claims nodes with a CAS; the final order is the threads' lists
    // concatenated. Which thread wins a node only changes the layout, never an answer.
    let owner: Vec<AtomicU32> = (0..n).map(|_| AtomicU32::new(u32::MAX)).collect();
    let lists: Vec<Vec<u32>> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..nt)
            .map(|k| {
                let (owner, off, adj) = (&owner, &off, &adj);
                sc.spawn(move || {
                    let mut ord: Vec<u32> = Vec::with_capacity(n / nt + 1024);
                    let claim = |x: usize| {
                        owner[x]
                            .compare_exchange(u32::MAX, k as u32, Relaxed, Relaxed)
                            .is_ok()
                    };
                    for r in k * n / nt..(k + 1) * n / nt {
                        if !claim(r) {
                            continue;
                        }
                        let mut h = ord.len();
                        ord.push(r as u32);
                        while h < ord.len() {
                            if let Some(&w) = ord.get(h + 16) {
                                pf(off, w as usize);
                            }
                            if let Some(&w) = ord.get(h + 8) {
                                pf(adj, off[w as usize] as usize);
                            }
                            let u = ord[h] as usize;
                            h += 1;
                            for e in &adj[off[u] as usize..off[u + 1] as usize] {
                                let v = (e & vm) as usize;
                                if owner[v].load(Relaxed) == u32::MAX && claim(v) {
                                    ord.push(v as u32);
                                }
                            }
                        }
                    }
                    ord
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    drop(owner);
    let order: Vec<u32> = lists.concat();
    drop(lists);
    // newid and the degrees in the new order on all threads (both are random reads), then one
    // sequential prefix sum
    let newid: Vec<AtomicU32> = (0..n).map(|_| AtomicU32::new(0)).collect();
    let mut noff = vec![0u32; n + 1];
    std::thread::scope(|sc| {
        let per = n.div_ceil(nt).max(1);
        for (c, (os, ds)) in order.chunks(per).zip(noff[1..].chunks_mut(per)).enumerate() {
            let (newid, off) = (&newid, &off);
            sc.spawn(move || {
                for (i, (&u, d)) in os.iter().zip(ds.iter_mut()).enumerate() {
                    if let Some(&w) = os.get(i + 16) {
                        pf(off, w as usize);
                        pf(newid, w as usize);
                    }
                    newid[u as usize].store((c * per + i) as u32, Relaxed);
                    *d = off[u as usize + 1] - off[u as usize];
                }
            });
        }
    });
    for k in 0..n {
        noff[k + 1] += noff[k];
    }
    let newid: Vec<u32> = newid.into_iter().map(|x| x.into_inner()).collect();
    let mut nadj: Vec<u32> = Vec::with_capacity(total);
    unsafe { nadj.set_len(total) }; // every slot is written below
    {
        let chunk = (n + nt - 1) / nt;
        let mut rest: &mut [u32] = &mut nadj;
        let mut slices = Vec::new();
        for k in 0..nt {
            let (lo, hi) = ((k * chunk).min(n), ((k + 1) * chunk).min(n));
            let (a, b) = rest.split_at_mut((noff[hi] - noff[lo]) as usize);
            slices.push((lo, hi, a));
            rest = b;
        }
        std::thread::scope(|sc| {
            for (lo, hi, s) in slices {
                let (order, off, adj, newid) = (&order, &off, &adj, &newid);
                sc.spawn(move || {
                    let mut p = 0;
                    for k in lo..hi {
                        let u = order[k] as usize;
                        if let Some(&w) = order.get(k + 16) {
                            pf(off, w as usize);
                        }
                        if let Some(&w) = order.get(k + 8) {
                            pf(adj, off[w as usize] as usize);
                        }
                        for e in &adj[off[u] as usize..off[u + 1] as usize] {
                            let x = *e;
                            s[p] = x & !vm | newid[(x & vm) as usize];
                            p += 1;
                        }
                    }
                });
            }
        });
    }
    drop(adj);
    drop(off);
    drop(order);
    let (off, adj) = (noff, nadj);
    // reverse graph for the backward search
    let rdeg: Vec<AtomicU32> = (0..n).map(|_| AtomicU32::new(0)).collect();
    let per = (total + nt - 1) / nt;
    std::thread::scope(|sc| {
        for k in 0..nt {
            let (adj, rdeg) = (&adj, &rdeg);
            sc.spawn(move || {
                for e in &adj[(k * per).min(total)..((k + 1) * per).min(total)] {
                    rdeg[(e & vm) as usize].fetch_add(1, Relaxed);
                }
            });
        }
    });
    let mut roff = vec![0u32; n + 1];
    for k in 0..n {
        roff[k + 1] = roff[k] + rdeg[k].load(Relaxed);
    }
    drop(rdeg);
    let rcur: Vec<AtomicU32> = roff[..n].iter().map(|&x| AtomicU32::new(x)).collect();
    let mut radj: Vec<u32> = Vec::with_capacity(total);
    unsafe { radj.set_len(total) };
    let rp = Ptr2(radj.as_mut_ptr());
    std::thread::scope(|sc| {
        let chunk = (n + nt - 1) / nt;
        for k in 0..nt {
            let (adj, off, rcur, rp) = (&adj, &off, &rcur, &rp);
            sc.spawn(move || {
                for u in (k * chunk).min(n)..((k + 1) * chunk).min(n) {
                    for &x in &adj[off[u] as usize..off[u + 1] as usize] {
                        let s = rcur[(x & vm) as usize].fetch_add(1, Relaxed);
                        unsafe { *rp.0.add(s as usize) = x & !vm | u as u32 };
                    }
                }
            });
        }
    });
    drop(rcur);
    // from here on a table of 128 KB or more gets its own mapping, so the old table a search
    // outgrows goes back to the kernel instead of sitting unused in a thread's malloc arena
    extern "C" {
        fn mallopt(p: i32, v: i32) -> i32;
        fn malloc_trim(pad: usize) -> i32;
    }
    unsafe {
        mallopt(-3, 1 << 17);
        malloc_trim(0)
    }; // M_MMAP_THRESHOLD; free arena memory back too
    let q = map(&format!("{data}/queries.txt"));
    let mut qs = Vec::new();
    let mut j = 0;
    while j < q.len() {
        let s = num(q, &mut j);
        let t = num(q, &mut j);
        qs.push((newid[s as usize], newid[t as usize]));
    }
    let big: HashMap<(u32, u32), u32> = big
        .into_iter()
        .map(|((a, b), w)| ((newid[a as usize], newid[b as usize]), w))
        .collect();
    drop(newid);
    let ans: Vec<AtomicU64> = (0..qs.len()).map(|_| AtomicU64::new(0)).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|sc| {
        for _ in 0..nt {
            sc.spawn(|| {
                // bidirectional Dijkstra: side 0 runs forward from s, side 1 backward from t
                let mut dist = [Dist::new(), Dist::new()];
                let mut heap = [Radix::new(), Radix::new()];
                let g = [(&off, &adj), (&roff, &radj)];
                loop {
                    let k = next.fetch_add(1, Relaxed);
                    if k >= qs.len() {
                        break;
                    }
                    let (s, t) = qs[k];
                    let mut best = u64::MAX;
                    if s == t {
                        best = 0;
                    } else {
                        for (side, x) in [(0, s), (1, t)] {
                            let i = dist[side].slot(x);
                            dist[side].set_at(i, x, 0);
                            heap[side].push(0, x);
                        }
                        loop {
                            let tf = heap[0].min();
                            let tb = heap[1].min();
                            let (Some(tf), Some(tb)) = (tf, tb) else {
                                break;
                            };
                            if tf.saturating_add(tb) >= best {
                                break;
                            }
                            let side = if heap[0].len() <= heap[1].len() { 0 } else { 1 };
                            let lim = if side == 0 { tb } else { tf };
                            let (d, u) = heap[side].pop();
                            let (o, a) = g[side];
                            let [df, db] = &mut dist;
                            let (mine, other) = if side == 0 { (df, &*db) } else { (db, &*df) };
                            for &x in &a[o[u as usize] as usize..o[u as usize + 1] as usize] {
                                let v = x & vm;
                                let w = if x >> vb != esc {
                                    x >> vb
                                } else {
                                    big[&if side == 0 { (u, v) } else { (v, u) }]
                                };
                                let nd = d + w as u64;
                                let i = mine.slot(v);
                                let k = mine.t[i];
                                let dv = if k == E {
                                    u64::MAX
                                } else if k as u32 == SPILL {
                                    mine.big[&v]
                                } else {
                                    k as u32 as u64
                                };
                                if nd < dv {
                                    mine.set_at(i, v, nd);
                                    let ov = other.get(v);
                                    if ov != u64::MAX && nd + ov < best {
                                        best = nd + ov;
                                    }
                                    // anything left from v to the other end costs at least the other frontier
                                    if nd + lim < best {
                                        heap[side].push(nd, v);
                                        pf(o, v as usize); // its offsets are read when it is popped
                                    }
                                }
                            }
                        }
                        for side in 0..2 {
                            dist[side].clear();
                            heap[side].clear();
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
        if v == u64::MAX {
            o.extend_from_slice(b"-1\n");
        } else {
            writeln!(o, "{v}").unwrap();
        }
    }
    std::fs::create_dir_all(&out).ok();
    std::fs::write(format!("{out}/distances.txt"), o).unwrap();
    std::process::exit(0);
}

// start a cache miss early, into L2: the build walks nodes in an order it knows ahead
#[inline(always)]
fn pf<T>(x: &[T], i: usize) {
    if i < x.len() {
        unsafe {
            std::arch::x86_64::_mm_prefetch(
                x.as_ptr().add(i) as *const i8,
                std::arch::x86_64::_MM_HINT_T1,
            )
        }
    }
}

// MADV_DONTNEED on the whole pages inside `b`: the file's pages stay in the page cache, only
// this process's mapping of them goes
fn release(b: &[u8]) {
    extern "C" {
        fn madvise(a: *const u8, l: usize, advice: i32) -> i32;
    }
    let (a, e) = (
        (b.as_ptr() as usize + 4095) & !4095,
        (b.as_ptr() as usize + b.len()) & !4095,
    );
    if a < e {
        unsafe { madvise(a as *const u8, e - a, 4) };
    }
}

// read-only mmap of an input file, so nothing is copied and nothing is charged to our heap
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

// radix heap: Dijkstra only ever pushes keys at or above the last one popped, so a key goes in
// the bucket of the highest bit where it differs from that last key, and a pop only
// redistributes the lowest non-empty bucket
struct Radix {
    last: u64,
    len: usize,
    b: Vec<Vec<(u64, u32)>>,
}
impl Radix {
    fn new() -> Self {
        Radix {
            last: 0,
            len: 0,
            b: (0..65).map(|_| Vec::new()).collect(),
        }
    }
    #[inline]
    fn idx(&self, k: u64) -> usize {
        64 - (k ^ self.last).leading_zeros() as usize
    }
    #[inline]
    fn push(&mut self, k: u64, v: u32) {
        let i = self.idx(k);
        self.b[i].push((k, v));
        self.len += 1;
    }
    fn len(&self) -> usize {
        self.len
    }
    // make bucket 0 (keys equal to last) non-empty, if anything is left
    #[inline(always)]
    fn fill(&mut self) {
        if !self.b[0].is_empty() || self.len == 0 {
            return;
        }
        let i = (1..65).find(|&i| !self.b[i].is_empty()).unwrap();
        let v = std::mem::take(&mut self.b[i]);
        self.last = v.iter().map(|x| x.0).min().unwrap();
        for &(k, x) in &v {
            let j = self.idx(k);
            self.b[j].push((k, x));
        }
        let mut v = v;
        v.clear();
        self.b[i] = v;
    }
    #[inline]
    fn min(&mut self) -> Option<u64> {
        self.fill();
        if self.len == 0 {
            None
        } else {
            Some(self.last)
        }
    }
    #[inline]
    fn pop(&mut self) -> (u64, u32) {
        self.fill();
        self.len -= 1;
        self.b[0].pop().unwrap()
    }
    fn clear(&mut self) {
        for x in &mut self.b {
            x.clear();
            x.shrink_to(1 << 12);
        }
        self.len = 0;
        self.last = 0;
    }
}
