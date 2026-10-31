# Track D: performance log

Local numbers are from an 8 thread Linux box, seed 1 full data, best of 5 runs, sha256 matching the published `a23dbdf0...`.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 72.69 | 893 | 0 |
| 1 | Rust, parallel parse, CSR graph, Dijkstra with early exit, queries spread over threads | run-85a7d4602e41 | 1.647 | 338 | 41.03 |

## Why it got faster

- Edge lines get parsed on all threads, then packed into a CSR array (offsets plus `(head, weight)` pairs). Self loops are dropped while building, since they can never shorten a path.
- Each query is a Dijkstra with a binary heap that stops as soon as the target is popped. Targets are picked by Dijkstra rank (median rank 512), so most searches settle a tiny part of the graph.
- Every thread owns a distance array and a list of touched nodes, and resets only those after each query instead of clearing 1.7M entries.
- Distances are `u64`, so sums above 2^32 just work. `u64::MAX` marks unreachable and prints as `-1`.
- Query time is now well under parse time, so the parse is the next target.

## Next ideas

Bidirectional search, and a faster integer parser over the 92 MB graph file.
