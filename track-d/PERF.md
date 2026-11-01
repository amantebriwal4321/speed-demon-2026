# Track D: performance log

Step 1 local numbers came from an 8 thread Linux box. T and M from step 2 on are the board's own numbers, sha256 matching the published `a23dbdf0...`.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 72.69 | 893 | 0 |
| 1 | Rust, parallel parse, CSR graph, Dijkstra with early exit, queries spread over threads | run-85a7d4602e41 | 1.647 | 338 | 41.03 |
| 2 | mmap, parallel CSR, parallel BFS relabel for locality, bidirectional Dijkstra | run-5b13f5193b36 | 0.295 | 494 | 55.86 |
| 3 | u32 distances with a side map for the few above 2^32 | run-aa04a894c3d5 | 0.306 | 327 | 56.94 |
| 4 | CSR built per node range bucket, no atomics | run-093e22eb3b78 | 0.277 | 314 | 57.96 |
| 5 | per query distances in a small open addressing table instead of a dense array per thread | run-5205ad48a187 | 0.307 | 172 | 59.08 |
| 6 | hash slot is one u64 (node, dist), so a probe is one load | run-9ecfb196b1c8 | 0.304 | 177 | 59.12 |
| 7 | rounds 7-16: radix heaps, BFS relabel on all threads, 4-byte edges | run-1af27b829c3b | 0.236 | 107 | 63.12 |
| 8 | round 17: query tables given back, used-slot cap | run-c3e3ece89411 | 0.235 | 91 | 63.78 |
| 9 | rounds 19-20: no stale check, 1K-slot tables | run-e8a600cefc0f | 0.233 | 92 | 63.85 |
| 10 | rounds 21-22: number parser, radix fill and set_at inlined | run-83d7b5a8003b | 0.230 | 92 | 63.97 |
| 11 | round 23: pushes prefetch the node offsets | run-c29392d5df70 | 0.224 | 90 | 64.35 |
| 12 | rounds 24-25: look-ahead prefetch in the relabel and the BFS | run-d3a4cf6ff601 | 0.220 | 87 | 64.52 |

## Step 1: why it got faster

- Edge lines get parsed on all threads, then packed into a CSR array (offsets plus `(head, weight)` pairs). Self loops are dropped while building, since they can never shorten a path.
- Each query is a Dijkstra with a binary heap that stops as soon as the target is popped. Targets are picked by Dijkstra rank (median rank 512), so most searches settle a tiny part of the graph.
- Every thread owns a distance array and a list of touched nodes, and resets only those after each query instead of clearing 1.7M entries.
- Distances are `u64`, so sums above 2^32 just work. `u64::MAX` marks unreachable and prints as `-1`.
- Query time is now well under parse time, so the parse is the next target.

## Round 2

- Queries were 90% of the time. Bidirectional Dijkstra with the standard stop rule (top forward + top backward >= best) halves the area each search covers.
- Node ids are random, so every dist read was a cache miss. A parallel BFS relabel (each thread grows regions and claims nodes with a CAS) puts graph neighbours next to each other in memory. Which thread wins a node only changes the layout, never an answer.
- Distances are u32 now, with the rare value above 2^32 kept in a side HashMap, which halves the per thread arrays.
- Edges are sorted into buckets by tail node range while parsing, so each thread builds its own slice of the CSR with no atomics.

## Round 3

- Each of the 12 query threads had two dense u32 arrays over 1.74M nodes, about 167 MB in total. A query reaches about 4k nodes on average (78k at most), so each side now uses a 16k slot open addressing table that grows on demand and gets wiped after every query.
- That costs 30 ms on the board, but memory drops from 314 MB to 172 MB, so it comes out ahead on points.
- Tried and dropped: parsing twice to skip the edge buckets (70 ms slower, same peak) and skipping the BFS relabel (much slower queries).

## Round 4

- Each distance table slot now packs node and distance into one u64, so a probe is one load.
- Tried and dropped: a heap of packed u64 (dist, node) keys. It made no measurable difference, because the time goes to the hash probes and the edge reads, not the heap.

## Round 5

- Profile on 8 threads: parse 23 ms, CSR 13 ms, BFS relabel 48 ms, reverse graph 17 ms, queries 120 ms. The queries settle 7.1M nodes in all, about 3.5k per query.
- Peak memory was the query phase holding arrays it no longer needed: the old offsets, the BFS lists and order, the reverse degree and cursor arrays, and newid after the queries are mapped. They are dropped as soon as they're used, and queries.txt is mapped. The local peak went from 143 MB to 116 MB.
- An improving edge used to probe the distance table twice, once to read and once to write. It now reuses the slot from the read. Locally that's about 7% faster.
- Board: 0.298 s, 144 MB, 60.08 points, a new best, rank 45 to 40 of 93 (run-0ba83db1c675).
- Tried and dropped: pruning labels with nd + the other side's heap top >= best. It's correct, but best is found late, so it saved nothing.
- Tried and dropped: wiping only the touched slots after each query instead of the whole table. No difference.

## Round 6

- The binary heaps are now radix heaps. Dijkstra only pushes keys at or above the last one popped, so a key goes in the bucket of the highest bit where it differs from that key, and a pop only redistributes the lowest non-empty bucket. Locally that's about 3% faster. Board: 0.280 s, 149 MB, 60.19 points, a new best (run-6e38f286db80).

## Round 7

- The integer parser reads 8 bytes at once, finds the first byte that isn't a digit, and converts the digits with SWAR. Fields of 9 or more digits fall back to the byte loop. Locally that's about 2% faster.

## Round 8

- Building newid and the degrees in the new order was one serial loop of 1.7M random reads inside the relabel. It now runs on all threads, with only the prefix sum left serial. Local median over 25 alternating runs went from 0.247 s to 0.236 s.
- Built for x86-64-v3 (AVX2, BMI, POPCNT) like track A, through `.cargo/config.toml`. Two rounds of 25 alternating runs gave 0.237 to 0.229 s and 0.220 to 0.209 s. The same flag on B and C showed nothing beyond the noise, so they stay as they are.

## Next ideas

A hash based distance map per thread to cut memory, and a faster integer parser.

## Round 9: prune pushes against the other frontier
A relaxed node is only pushed when nd + (other side's heap min) < best: any rest of the path
from v costs at least that frontier, and if the other side already reached v, best was updated
just before. Exact on s7, s42, t5, full. Local medians 0.2222 s -> 0.2183 s (-1.8%).

## Round 10: drop the unused touched lists
The per-query touched lists were filled but never read (clear() refills the whole table).
Removed: exact on s7, s42, t5, full; medians 0.2075 s vs 0.2068 s, a few lines less code.

## Round 11: less code
Dist::set and the unused new() argument went; the POP switch went. rustfmt lines 515 -> 501. Exact on s7, s42, t5, full. (Dropping the BFS relabel instead would cut ~40 lines but costs +48% time: 0.203 -> 0.301 s.)

## Round 12: clear only the used slots
A table grown by one big search (64K-128K slots) was refilled on every query though most searches use a few thousand. Dist now keeps the slots it filled and clears just those when they are under 1/16 of the table. Exact on s7, s42, t5, full; medians 0.2015 -> 0.1974 s.

## Round 13: 4-byte edges
Every edge (parse buckets, both CSRs) is one u32: head in the low vb = ceil(log2 n) bits (21 here), weight in the rest. Weights that do not fit (3,496 of 5M, 0.07%) store all ones there and the true weight sits in a map by (tail, head); parallel big edges keep the minimum, the only one a shortest path can use. Parse buckets hold tail << 32 | packed (8 bytes, was 12). Exact on s7, s42, t5, full (which include 0 and 2^32-1 weights and parallel edges). Local anon peak 127 -> 89 MB, medians 0.2056/0.1975 -> 0.1888/0.1858 s.

## Round 14: query-phase memory (not kept)
Packed 12-byte radix entries: peak 90 -> 89 MB, time same; the heaps are not the peak. Dist tables grown at 3/4 load instead of 1/2: peak 90 -> 84 MB but medians +2-4%, a wash on the score. Both reverted.

## Round 15: stale heap entries
451,741 of 7,594,508 pops (6%) are stale, so skipping them cannot buy much; not attempted.

## Round 16: query design
Pops per query: median 128, p90 12,150, max 79,548; the longest 10% of queries do 75% of the work. Tried: dense per-thread distance arrays (peak 89 -> 179 MB, no faster), a per-node bit filter before asking the other side (no faster), multiply instead of divide for edge buckets (within noise). A landmark (ALT) lower bound needs a full-graph Dijkstra per landmark and direction, about 180 ms of thread time each at our measured ~105 ns per pop, against ~75 ms of query time it could save; a geometric bound is not valid because of the weight-0 edges. Nothing kept.

## Round 17: query-phase memory with 12 threads
With the thread count forced to 12 (the board's), the local cgroup peak is 98 MB against 78 MB with 4 threads, close to the board's 104 MB. Anonymous memory per phase showed the queries adding 26 MB: each thread's distance tables (up to 1 MB + 512 KB), `used` lists (384 KB) and heap buckets (0.5 MB), plus every table a search outgrew, which glibc keeps in the thread's arena where the next, larger table can't reuse it.
- Before the queries the mmap threshold is fixed at 128 KB (so an outgrown table goes back to the kernel) and malloc_trim(0) hands back what the build freed. Fixing the threshold from the start instead gave 80 MB but cost 5% in the build phases (fresh pages for every big buffer).
- The `used` list stops growing at 1/16 of the table, the point where clearing falls back to a fill anyway.
12-thread peak 98 -> 84 MB; time neutral (per-phase medians over 11-13 alternating runs within +0.4%). Exact on s7, s42, t5, full. rustfmt lines 541 -> 554.
Board: run-c3e3ece89411, 0.2352 s, 91.4 MB (from 102.8 to 107.4 MB), 63.78 points, a new best, rank 40 -> 36 of 95.
Tried and dropped, all exact:
- Dist slot from the BFS-ordered id itself (`v & mask`) for locality: +15%, linear-probe clustering. Hashing groups of 8 ids so a group shares a line: no change.
- Expanding the side with the smaller top key instead of the smaller heap: 3.4x slower (the graph is far from symmetric).
- Shrinking each table back to 16K slots after a big search (with the fixed threshold): 71 MB peak but queries +12%.
Profile of the query loop (perf, 4 cores): about 30% of its samples are misses on `off`/`adj`, 32% on the two distance-table probes, and Radix::fill is 11% of the whole run.

## Round 18: the biggest searches give their tables back
A 2 ms trace of the cgroup usage with 12 threads: 73 MB at the CSR build, then the query phase climbing from 65 to 85 MB as each thread keeps the largest table and heap it ever grew. A table past 64K slots (512 KB) now goes back to the start size after its search, and heap buckets shrink to 4K entries; only the few biggest searches pay to grow again. Shrinking everything past 16K slots (round 17's try) cost 12%; this costs nothing measurable (11 alternating runs: queries 305 -> 298 ms). 12-thread peak 84-85 -> 80-81 MB. Exact on s7, s42, t5, full.

## Round 19: no stale check on pop
Every pop probed its own side's table to skip stale heap entries, but only 6% are stale. A stale pop cannot change anything: its node's smaller entry was popped first and relaxed every edge at that distance, so every relaxation from the stale one fails, and the radix heap only sees keys at or above the last pop. Dropping the check saves a probe on 94% of pops for a few failed relaxations on 6%. Exact on s7, s42, t5, full; queries 278 -> 270 ms over 11 alternating runs.
Tried and dropped: claiming BFS nodes in a 218 KB bitset instead of the 7 MB owner array (the BFS cost is the adjacency reads; no change); an occupancy mask for the radix buckets (fill's cost is redistributing, not finding the bucket; no change).

## Round 20: distance tables start at 1K slots
With 12 threads forced on 4 cores the query phase took 107 ms longer than with 4 threads for the same work, which points at cache footprint: each thread's two tables started at 16K slots (128 KB each), and on the board two hyperthreads share a core's L2. Half of all searches pop fewer than 130 nodes. Starting at 1K slots (8 KB): queries 407 -> 377 ms with 12 threads and 293 -> 283 ms with 4, over 15 alternating runs each; 4K slots was no better than 16K. Peak 82 MB both. Exact on s7, s42, t5, full.
Board for rounds 19 + 20: run-e8a600cefc0f 0.2331 s (63.85 points, a new best), then 0.2585, 0.2353 (run-2faf370cef70), 0.2356 (run-287b797515e9, 63.93 points, a new best), 0.2344 s on the same code: the board's spread on D is about 10%.

## Round 21: the number parser inlined
perf listed `num` as its own function (3.5% of samples): three calls per edge line, 15 million in all, each with a call and spills. `#[inline(always)]`: two sets of 15 alternating runs, 0.611 -> 0.581 s and 0.638 -> 0.597 s, CPU -6%. Exact on s7, s42, t5, full, and on fresh seeds 99 (sample) and 3 (tiny) against the reference.

## Round 22: Radix::fill and Dist::set_at inlined
Both were marked `#[inline]` but stayed out of line (fill was 8.8% of samples under its own name). `#[inline(always)]` on both: three sets of 15 alternating runs, 0.592 -> 0.548, 0.614 -> 0.593 s at 4 threads and 0.693 -> 0.653 s with 12; CPU -4 to -9%. Exact on s7, s42, t5, full, h99, h3.
Rounds 21 + 22 on the board: run-83d7b5a8003b, 0.2298 s, 63.97 points, a new best.

## Round 23: prefetch a pushed node's offsets
Annotating the query loop: the two hottest loads are a popped node's first edge (16% of the loop) and its offsets (11%), then the two distance-table probes. A node is pushed some time before it is popped, so the push now prefetches its offsets into L2. 15 alternating runs: 0.478 -> 0.461 s at 4 threads; with 12 threads 0.586 -> 0.543 and 0.574 -> 0.541 s. A T0 (L1) hint was 4% worse than T1. Exact on s7, s42, t5, full, h99, h3.
Board: run-c29392d5df70, 0.2240 s, 89.5 MB, 64.35 points, a new best, rank 37 -> 33 of 98.

## Round 24: prefetch ahead in the relabel
The newid and relabel passes walk the nodes in BFS order and read the old graph at random: offsets, then the edge list, then newid of every head. The order is known ahead, so each step prefetches (into L2) the offsets 16 nodes ahead and the edge list 8 ahead. Phase times over 13 alternating runs: relabel 49 -> 27 ms, newid 23 -> 20 ms. Tried and dropped: prefetching a node's offsets when the BFS queues it (bfs +3 ms) and look-ahead in the reverse build (+8 ms). End to end, 15 alternating runs: 0.475 -> 0.454 s at 4 threads, 0.528 -> 0.524 s with 12, CPU -3%. Exact on s7, s42, t5, full, h99, h3.

## Round 25: look-ahead in the BFS
The BFS queue is first in, first out, so the nodes it will expand next are known: each step prefetches the offsets of the node 16 places ahead and the edge list of the node 8 ahead. BFS phase 56 -> 46 ms over 13 alternating runs; end to end 0.431 -> 0.414 s at 4 threads, flat with 12 threads time-sliced on 4 cores. The same look-ahead in the CSR scatter saved 1 ms and was dropped. Exact on s7, s42, t5, full, h99, h3.
Rounds 24 + 25 on the board: run-d3a4cf6ff601, 0.2196 s, 87.4 MB, 64.52 points, a new best (rank 33 of 98).
Tried and dropped since: expanding the side that has reached fewer nodes (+4%) or strict alternation (+7%) instead of the smaller heap; prefetching a pushed node's edge list through its offset (within noise); mmap threshold 64 KB for the queries (peak the same).
