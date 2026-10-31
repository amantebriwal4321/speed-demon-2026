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

## Next ideas

A hash based distance map per thread to cut memory, and a faster integer parser.
