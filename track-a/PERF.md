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
| 8 | rounds 23-28: endpoint compare inline, lines written in place, ids through one table, all rows in one reserved mapping, each thread drops its share's page tables | run-68b956798dc8 | 0.0269 | 22 | 79.99 |
| 9 | rounds 29 and 32: glibc linked statically, p50 selected before p95 and p99 | run-0c2cc394e96e | 0.0255 | 22 | 80.61 |

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
Also tried and dropped: keeping the top n - k(95) rows of each small group in one pass (p95 and p99 come from them) and selecting only the median. Exact, but 31 alternating runs at 4 and 12 threads were within noise (+3%, -1%): the median selection is most of the cost.

## Round 23: endpoint text compared inline
Most endpoints are over 16 bytes, so after the first and last 8 bytes matched, every line still compared its endpoint with the thread's copy through a `memcmp` call: 5 to 7% of CPU samples on 4 cores, plus the call. eq() now compares up to 64 bytes as two 32 byte AVX2 compares and a mask of the first l bits; the text copy always ends in 64 zero bytes so it can be read that far from any start, and a line within 64 bytes of the end of the file, or a longer endpoint, falls back to the slice compare. 21 alternating runs: 98.0 -> 92.1 ms (-6%), CPU 295 -> 283 ms; peak 16-17 MB both. Exact on s7, s42, t5, full, h99, h3.

## Round 24: output lines written in place
Timers inside the hour phase (CPU summed over 4 threads): formatting the 127,000 output lines cost about 30 ms, more than the selections. Each line was eight `extend_from_slice` calls (the hour, the endpoint, six numbers from a scratch buffer), each a capacity check and a `memcpy` call for a few bytes. line() now reserves room for the whole line once and writes it through a pointer: the 21-byte hour, the endpoint, and each number's digits straight into place from the last one back, two at a time, its length from `ilog10`. s7: 150.6M -> 138.2M instructions (-8%); 21 alternating runs: 92.9 -> 90.7 ms, CPU 270 -> 264 ms. Exact on s7, s42, t5, full, h99, h3.

## Round 25: endpoint ids merged through one cheap table
Between the parse and the hours, one thread gives the endpoints global ids. That was a std HashMap keyed by the text (SipHash over every name, about 46,000 times: an entry per thread's name, then a lookup per name again for the maps), which grows with the thread count: 1.0 ms at 4 threads, 2.2-3.8 ms with 12 forced, all of it serial. The threads' names now go into one open-addressing table hashed on the keys the threads already have (first and last 8 bytes, length) and compared on the text; each thread's ids come out of the same pass, and are mapped to the sorted rank afterwards. With 12 threads forced: 2.5-3.8 -> 1.1-1.4 ms. 21 alternating runs at 4 threads: 84.9 -> 83.7 ms; peak 16 MB both. Exact on s7, s42, t5, full, h99, h3.

## Round 26: all rows in one reserved mapping
Timers inside the hour phase put the unmapping of each hour's row blocks at 20-26 ms of CPU on 4 cores: about 1,000 blocks of 64 KB (one partial block per thread and hour, and the full ones), each its own mmap during the parse, which takes the address-space lock and stalls the other threads' page faults, and its own munmap after, which interrupts every core the process runs on.
The rows now live in one mapping reserved up front (MAP_NORESERVE, no huge pages): for each hour a region per thread, the hour's regions side by side, each region rounded to whole pages and big enough for every line of its thread's share of the file (a line is at least 40 bytes). Untouched pages cost nothing, a thread appends to its own regions with no block bookkeeping, the hour phase reads each thread's rows as one slice, and when an hour's rows are gathered one MADV_DONTNEED over the hour's regions hands all its pages back: 72 calls instead of about 2,000. The first try freed a range that did not end on a page boundary, which zeroed the first rows of the next hour (s7 and h3 failed); regions are whole pages now.
21 alternating runs at 4 threads: 85.2 -> 70.0 ms (-18%), CPU 261 -> 215 ms; with 12 threads forced, 15 runs: 103.9 -> 77.8 ms (-25%). Peak 16 MB at 4 threads either way, 19 -> 18 MB at 12. Exact on s7, s42, t5, full, h99, h3, also with 12 threads.

## Round 27: an hour's text sized up front
strace after round 26: 77 mremap calls, one for nearly every hour, as its output buffer outgrew its first 88 KB and glibc moved the mapping (a move rewrites page tables and flushes the TLB on every core). The buffer now starts at the most one hour can hold (every endpoint, six numbers of up to 20 digits: about 220 KB), so it never moves; with buffers over 16 KB mapped on their own, pages never written cost nothing. mremap 77 -> 6. 21 alternating runs: 68.0 -> 62.2 ms (-9%), CPU 212 -> 200 ms; peak 16 MB both. Exact on s7, s42, t5, full, h99, h3.
Board: round 26 alone run-6efec75da0d8 0.0413 s (A's slow runs come in about one in five on unchanged code), then rounds 26 + 27 run-138e996faf95 0.0312 s, 22.3 MB, 78.43 points, a new best (from 77.49), rank 45 -> 41 of 128.

## Round 28: each thread drops its share's page tables (board experiment)
C's round 24 on A: once a thread has parsed its share of the log it never reads it again (endpoint text is copied), so it drops the share's page table entries with MADV_DONTNEED (a read lock, like a page fault, so the other threads keep faulting in), and the exit no longer tears down a 239 MB mapping on one thread. An munmap version lost to the address-space lock (C round 20, and here while the row blocks were still mmaps); since round 26 there are none left to contend with. 21 alternating runs: 73.6 -> 70.0 ms at 4 threads, 88.6 -> 87.9 ms with 12 forced. Exact on s7, s42, t5, full, h99, h3.

## Round 29: glibc linked statically
`-C target-feature=+crt-static` in .cargo/config.toml: the binary is static-pie, so a run starts without the dynamic loader mapping libc and resolving its symbols. A trivial program goes from 2.27 to 1.77 ms exec to exit (40 alternating runs); on A, 21 alternating runs 67.8 -> 66.3 ms. Peak memory the same or 1 MB lower. The build image (rust:1-slim-trixie) has libc.a. Exact on s7, s42, t5, full, h99, h3.

## Round 30: two small cuts in the hour and parse loops
- An hour's gather buffer was cleared to its new length every hour (`clear` then `resize`: a 170 KB memset, 1.4% of the instructions on s7), but the scatter writes every slot below the hour's row count, so it now only grows.
- A thread's region of each hour is looked up from a 72-entry array made once, instead of computed with a multiply on every line.
s7: 134.5M -> 131.9M instructions; 31 alternating runs: 60.0 -> 59.2 ms. Exact on s7, s42, t5, full, h99, h3.
Reverted (round 31): on the board round 30 ran 0.0292, 0.0261, 0.0336, 0.0298 and 0.0335 s, against six runs of round 29 between 0.0260 and 0.0269 s, while the other tracks' times in the same runs stayed put. The likely cause is the gather buffer: cleared, it was streamed into cache before the scatter's random writes; left as it was, those writes miss. Back to round 29.

## Round 32: p50 selected first
A group's percentiles came from three selections: p99 on the group, then p95 among the rows below it, then p50 among the rows below that. Each of those ran over nearly the whole group (99% and 95% of it), so about 2.9 n compares. Now p50 is selected first, then p95 among the rows above it (half the group) and p99 among the rows above that (5%): about 1.55 n, the same values. Groups over 128 rows hold 55% of the rows, so the selections are mostly long ones. THP `always`, 31 alternating runs: 57.1 -> 55.7 ms and 57.7 -> 55.3 ms at 4 threads, 70.0 -> 67.6 ms with 12 forced. Exact on s7, s42, t5, full, h99, h3.

## Round 33: the threads' endpoint tables freed before the hours
The global name list was slices into each thread's endpoint table (hash slots, keys and text, about 170 KB a thread), so all twelve tables stayed through the hours, where the peak is. The 1,842 names are copied out now (about 74 KB) and the tables, the merge's own slots and its first-seen list are dropped before the hours start. THP `always`, 12 threads forced, five runs each: peak 18 MB every time against 16-17 MB. Exact on s7, s42, t5, full, h99, h3.
Board: run with rounds 32-33, 0.0259 s at 20.5 MB (from 22.0-22.4 MB), 80.72 points, a new best.
Since: run-bb9921cb6cae 0.0257 s, 20.6 MB, 80.79 points, a new best on the same code.

## Round 34: each thread's share mapped in one call (not kept)
perf puts about a fifth of the local CPU time in the kernel, much of it page faults on the input (each maps 16 pages around the one touched). Each parse thread now asked for its whole share's page table entries first, with madvise(MADV_POPULATE_READ). Exact on s7, full, h3, but slower over 41 alternating runs: 53.1 -> 57.6 ms at 4 threads, 65.7 -> 71.8 ms with 12 forced. A thread can no longer parse while the rest of its share is being mapped, and the faults it saves were cheap ones. Not kept.

## Round 35: pieces handed out as threads come free (board experiment)
Each thread parsed one fixed twelfth of the file, so the parse ended when the slowest thread did. On the board two threads share each core, and a thread whose neighbour is busier runs slower; track C gained from the same change (its round 26). The file is now cut into four pieces per thread, and each thread takes the next piece from an atomic counter. A thread's rows still go to its own regions, which now have room for twice its share of the file's lines; a row that would not fit goes to its side rows (a path forced on for testing with regions of 100 rows: exact on s7 and full). Each piece's page tables are dropped when it is done. 31 alternating runs: 53.1 -> 54.2 ms at 4 threads (no SMT, so nothing to balance), 68.0 -> 63.3 ms with 12 threads forced on 4 cores (-7%); peak 16-17 MB either way. 4 more counted lines. Exact on s7, s42, t5, full, h99, h3 and the fresh full-scale seed 77, also with 1, 3, 5, 7 and 16 threads.
Board: run-867136d1be06 0.0262 s, run-0bacecee5c4b 0.0294 s, run-739f5d1b1a55 0.0263 s, at 20.6-21.1 MB, against 0.0257-0.0261 s for round 33 on most runs; but track C, unchanged, ran 0.0445-0.0467 s in the same runs against its usual 0.0388 s, so the box was slow. The board also counts S at 498 for this version where rustfmt and scc here give 491 (both gave 487 before).

## Round 36: round 35 out again (board A/B)
Round 33's fixed shares back, to compare with round 35 on runs of the same hour.
