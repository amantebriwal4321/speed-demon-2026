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

- The binary heaps are now radix heaps. Dijkstra only pushes keys at or above the last one popped, so a key goes in the bucket of the highest bit where it differs from that key, and a pop only redistributes the lowest non-empty bucket. Locally that's about 3% faster.

## Next ideas

A hash based distance map per thread to cut memory, and a faster integer parser.
