# Track A: performance log

Step 1 local numbers came from an 8 thread Linux box. T and M from step 2 on are the board's own numbers, sha256 matching the published `495ca096...`. Board run IDs get filled in once the Board app is on the repo.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 32.05 | | 0 |
| 1 | Rust, whole file read once, chunked on newlines, one thread per chunk | run-8eeb9befcebc | 0.172 | 340 | 51.06 |
| 2 | mmap the log, exit without freeing, parse each line from the back with a SWAR newline search | run-21a0f87472ee | 0.080 | 92 | 64.41 |
| 3 | per hour flat u64 rows (endpoint, latency, 5xx bit) and one plain sort per hour instead of 132k small vectors | run-17ee990429f8 | 0.067 | 52 | 68.86 |
| 4 | free each hour's rows as soon as that hour is written | run-88f1b4564bd4 | 0.065 | 34 | 70.85 |
| 5 | pread into a reused 512 KB buffer per thread instead of the mmap, so no page faults | run-f70f5f3b9d4b | 0.061 | 39 | 70.96 |

## Step 1: why it got faster

- The reference spends most of its time in `datetime.strptime`. The span is a fixed 72 hours, so the hour index is just month, day and hour digits read straight from fixed byte offsets in the timestamp. No date parsing at all.
- Status only matters for 5xx, so it's one byte compare on the first digit.
- Each thread keeps its own endpoint to id map and a flat `Vec<Vec<u32>>` of latencies per (endpoint, hour). No locks while parsing.
- Disorder in the file doesn't matter because nothing assumes order. Groups are merged after parsing, hours are handed out to threads, and each group is sorted once with `sort_unstable` to read the nearest rank values.
- Endpoints get sorted as raw byte slices, which is exactly the order the spec wants.

## Round 2

- mmap instead of reading the file into a Vec. The 240 MB input never lands on our heap, which is most of why memory fell from 340 MB to 34 MB.
- Rows are packed as `endpoint << 33 | latency << 1 | is_5xx`. Sorting an hour's rows with one plain `sort_unstable` groups them by endpoint and sorts each group's latencies in the same pass, and reading the low bit counts the 5xx responses.
- Writing 72 small per hour lists is sequential and cache friendly. The old layout pushed into 132k scattered vectors per thread.

## Round 3

- The mmap version spent a good part of the scan in page faults. Each thread now preads its byte range in 512 KB pieces into one buffer it keeps reusing, and carries the partial last line over to the next read. Endpoint names get copied out the first time a thread sees them, since the buffer gets overwritten.
- Locally the median went from 70 ms to 58 ms. On the board it is 65 to 61 ms, with 5 MB more for the buffers.

## Round 5

- Profile on 8 threads: parse 22 ms, endpoint ids 4 ms, hours and output 11 ms. The board's memory is anonymous memory plus the 10 MB summary.txt in the page cache, so freeing rows while the output grows counts.
- glibc kept every 16 KB row block in its thread arenas. With M_MMAP_THRESHOLD just under 16 KB each block is its own mapping, and each hour's blocks are dropped as soon as the hour is done. The local anonymous peak went from 25 MB to 17 MB.

## Next ideas

Radix sort per hour, and a faster integer formatter for the output.
