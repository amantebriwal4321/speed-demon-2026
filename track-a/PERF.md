# Track A: performance log

Local numbers are from an 8 thread Linux box, seed 1 full data, best of 5 runs, sha256 matching the published `495ca096...`. Board run IDs get filled in once the Board app is on the repo.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 32.05 | | 0 |
| 1 | Rust, whole file read once, chunked on newlines, one thread per chunk | run-8eeb9befcebc | 0.172 | 340 | 51.06 |

## Why it got faster

- The reference spends most of its time in `datetime.strptime`. The span is a fixed 72 hours, so the hour index is just month, day and hour digits read straight from fixed byte offsets in the timestamp. No date parsing at all.
- Status only matters for 5xx, so it's one byte compare on the first digit.
- Each thread keeps its own endpoint to id map and a flat `Vec<Vec<u32>>` of latencies per (endpoint, hour). No locks while parsing.
- Disorder in the file doesn't matter because nothing assumes order. Groups are merged after parsing, hours are handed out to threads, and each group is sorted once with `sort_unstable` to read the nearest rank values.
- Endpoints get sorted as raw byte slices, which is exactly the order the spec wants.

## Next ideas

Radix or select based percentiles instead of a full sort, and mmap to skip the read copy.
