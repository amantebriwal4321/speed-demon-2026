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
| 6 | rounds 6-17 (see below): hours in parallel, written in order, SWAR fields | run-3ca301542316 | 0.0366 | 21 | 77.46 |
| 7 | rounds 18-21: endpoint text in a per-thread copy, percentiles by selection | run-d9f8db937084 | 0.0360 | 22 | 77.49 |

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
- glibc kept every 16 KB row block in its thread arenas. With M_MMAP_THRESHOLD just under 16 KB each block is its own mapping, and each hour's blocks are dropped as soon as the hour is done. The local anonymous peak went from 25 MB to 17 MB. Board: 0.0465 s, 23 MB (from 33), 74.30 points, a new best, rank 49 to 47 of 106 (run-9039a2342ca7).
- Tried and dropped: MADV_POPULATE_READ on each thread's range before parsing. About 10 ms slower locally.

## Round 6 (looked into, not built)

- The idea was a streaming hour by hour pass that only keeps the current hour's rows. A proper count killed it. 405,921 of the 3.1M lines sit behind the newest hour seen so far, not 491. The 491 was the number of places where one line's hour is lower than the line just before it. The spec says why: shard 0 uploads each hour 5 to 10 minutes after it ends, and no bound on lateness is promised.
- So no hour can be finished early and still be exact. Every finished hour would have to wait for the end of the file, or be recomputed when a late line turns up. The output text waiting to be written (10 MB) would take the place of the rows (12 MB), so memory would barely move, and the run would get slower and the code longer.

## Round 7

- Profiling the parse showed the endpoint lookup took up to a third of it. Most endpoints are over 16 bytes, so every hit compared against the first copy somewhere in the 240 MB file, which was a cache miss nearly every line. Each thread now keeps a copy of its endpoint names in a small arena (about 75 KB) and compares against that. Board: 0.0476 s against 0.0465 to 0.0470 s before, and 6 more lines (run-14a3cacb173a), so it was reverted. The revert then ran 0.0486 s (run-beb660fac341), and C's unchanged code ran anywhere from 0.069 to 0.091 s on the board. A single board run is noisier than this change, so the arena goes back in. It's faster locally, and its one board run beat the revert's.

## Round 8

- Row blocks went from 16 KB to 64 KB. Since round 5 each block is its own mapping, so this cuts the mmap and munmap calls by 4x. A last block's untouched pages are never resident, so memory doesn't grow. Locally the median went from about 0.049 s to 0.045 s.

## Round 9

- Mapping endpoint names to global ids was a serial 4 ms: a sort of all threads' name slices (about 15k) and a binary search for each one. Now the distinct names go through a hash map, only the 1,842 get sorted, and each thread's ids are hash lookups. That step dropped to about 2.8 ms, and two rounds of 25 alternating runs gave 0.052 to 0.049 s and 0.051 to 0.048 s.

## Next ideas

Radix sort per hour, and a faster integer formatter for the output.

## Round 13: less code
Endpoint bytes are compared in the mmap instead of a copied arena (arena, aoff and eq gone); the POP mmap switch is gone. rustfmt lines 420 -> 397. Exact on s7, s42, t3, t5, full; medians 0.0425 -> 0.0400 s, peak 15 MB both.

## Round 14: plain sort for percentiles
Groups average about 25 rows (3.1M rows, 127k output lines), so three select_nth passes buy nothing over one sort_unstable. rustfmt lines 397 -> 369. Exact on s7, s42, t3, t5, full; medians equal across four paired runs.

## Round 15: write outside the lock
The thread that flushed finished hours used to hold the lock through write_all, so every other thread waited to drop its hour off (3-4 ms per thread). Now one thread at a time is the writer: it takes the ready run of hours under the lock, writes them unlocked, then checks again. Exact on s7, s42, t3, t5, full; three paired medians 0.0524/0.0553/0.0507 -> 0.0504/0.0546/0.0494 s (box slower than earlier today), peak 15 MB.

## Round 16: parser profile and tries (not kept)
Ablations on full: LF walk only 6.5 ms, + field parse ~10 ms, + endpoint lookup ~15 ms (the lookup is the largest parse cost; row pushes are free). Keys inline in the slot table (24 B slots): +1-3%. One-multiply hash: same. Both reverted.

## Round 17: batched endpoint lookups (not kept)
Lines parsed into batches of 16 or 64, lookups run per batch so they can overlap: exact on all sets, but four paired runs each were within noise (0.0466/0.0454/0.0460/0.0465 vs 0.0496/0.0459/0.0453/0.0467 s at 16). The out-of-order core already overlaps them. Reverted.

## Round 18: endpoint names compared in cache
perf on 4 cores put `memcmp` at 8.8% of samples: an endpoint over 16 bytes that matched a slot's hash words was compared byte for byte against its first line, somewhere random in the 239 MB file, so most of those compares missed to memory. Each thread now copies an endpoint's text into its own small arena when it first sees it (1,842 names, about 55 KB), and the compare and the later name sort read from there. Exact on s7, s42, t5, full. Paired, 21 alternating runs, twice: 0.095 -> 0.084 s and 0.094 -> 0.088 s, CPU -5 to -11%; cgroup peak 15-16 MB both. rustfmt lines 385 -> 386.
Board: run-d5ef932cf47c, 0.0379 s, 22.1 MB, 76.97 points; the old code ran 0.0366 to 0.046 s, so one run cannot tell.

## Round 19: row blocks from the malloc heaps during the parse
The local profile was about a quarter kernel time, including TLB-shootdown IPIs. With the mmap threshold at 16 KB, every 64 KB row block was its own mmap (about one per thread and hour, 864 and up) and later its own munmap. An mmap takes the address space lock for writing, which stalls every other thread's page faults, and each munmap interrupts every core the process runs on. Measured with 12 threads forced on 4 cores, 21 alternating runs:
- blocks from the heaps for the whole run: 0.105 -> 0.077 s, but peak 19 -> 25 MB (the hours' buffers and freed blocks pile up across the threads' arenas);
- blocks mapped but never unmapped: only -8%, and peak 28 MB;
- kept: heaps during the parse, then the 16 KB threshold again before the hours, so the hours' buffers are their own mappings: 0.099 -> 0.075 s (-25%), CPU -12%, peak 18-19 -> 22-23 MB.
Exact on s7, s42, t5, full, also with 12 threads.
Reverted (round 20): board run-8b01065c2da1 gave 0.0366 s but 27.9 MB, against 21-22 MB. The board backs heap memory with 2 MB pages (locally with THP `always` the peak goes 18-20 -> 24 MB), and the time gain did not show beyond the run-to-run spread.

## Round 21: percentiles by selection
Phase timers: the hour phase is 31 ms of 71 locally, and perf puts the group sorts at 12% of all CPU. Only three ranks of each group are printed, so p99 is selected first (select_nth_unstable), then p95 among the rows below it, then p50 below that, instead of sorting the group. 21 alternating runs: 0.074 -> 0.072 s at 4 threads, 0.105 -> 0.099 s with 12; CPU -5 to -9%. Exact on s7, s42, t5, full.

## Round 22: output past the page cache (not kept)
The summary (10 MB) sits in the page cache, which counts toward M, and each hour's row blocks are unmapped as it is processed (864 unmaps, each interrupting every core) to make room. Tried: summary.txt opened with O_DIRECT (whole 4 KB pages from an aligned buffer, the last page's padding cut with set_len, plain writes as the fallback) and the blocks kept until exit. Exact on all six sets and 4% faster, but the peak went 18-19 -> 27-29 MB: what the frees offset is mostly the finished hours waiting for an earlier hour to be written, not the page cache. Reverted.
Also tried and dropped: sorting groups of up to 16, 32 or 64 rows instead of selecting (within noise either way).
