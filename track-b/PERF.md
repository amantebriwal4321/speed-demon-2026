# Track B: performance log

Step 1 local numbers came from an 8 thread Linux box. T and M from step 2 on are the board's own numbers, sha256 matching the published `f86973c2...`.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 56.35 | 2,200 | 0 |
| 1 | Rust, queries first, index only the query terms, positional postings, parallel build and parallel queries | run-a80f93dff992 | 0.798 | 989 | 46.13 |
| 2 | token stream of term ids for phrases instead of (doc, pos) postings, packed u128 token table, queries run per part, mmap | run-52fac7961fbc | 0.631 | 366 | 52.19 |
| 3 | doc lists by counting sort over the stream, SSE2 tokenizer, better hash mixing, only count, sum and top 10 kept per query | run-fd411d459da0 | 0.417 | 289 | 56.88 |
| 4 | u16 token stream: phrase terms get the low ids, other query terms go to a per doc presence list | run-9a693a205195 | 0.428 | 213 | 57.35 |
| 5 | BOOL on bitsets: ready made bitsets for terms in more than 1 doc in 32, a scratch bitset for the rest | run-3672a75d2a36 | 0.365 | 229 | 58.03 |
| 6 | rounds 6-18: dense terms straight into bitsets, u16 part-local lists, 8-byte probe slots | run-9b9f8c901f87 | 0.308 | 96 | 62.82 |
| 7 | round 20: phrase check anchored on the rarest word, popcount sums | run-96a07446a51b | 0.249 | 97 | 64.94 |
| 8 | round 29: the fill in two waves | run-abf05797d461 | 0.243 | 76 | 66.05 |
| 9 | rounds 30-32: common-term cutoff 1 in 24 (and waves) | run-14c0632534ec | 0.251 | 67 | 66.25 |
| 10 | round 32 again (two waves, cutoff 1 in 24) | run-ff5ca3c4b651 | 0.245 | 70 | 66.34 |
| 11 | rounds 36-42: query terms numbered in a second pass, answers in runs, 1 MB blocks, docs' page tables dropped by the answering threads, glibc linked statically | run-a46587694315 | 0.220 | 67 | 67.42 |
| 12 | round 45: query temporaries dropped before the scan | run-84d08843293a | 0.2212 | 66.0 | 67.46 |
| 13 | rounds 51-56: tokens a window at a time, each part filled by two threads, less before the scan | run-8f37cd767722 | 0.2127 | 65.9 | 67.84 |

## Step 1: why it got faster

- Queries are read first, so the index only holds the terms some query actually uses. Every other token is one hash miss and gets skipped.
- Tokenizing is a 256 entry lookup table that does the ASCII lowercase and the separator test in one step. Bytes from 0x80 up are separators, so `Zürich` splits into `z` and `rich` for free.
- The corpus is cut into chunks on line boundaries and tokenized on all threads. Each posting is `(doc, position)`, and since chunks are in file order, concatenating them keeps every list sorted.
- BOOL is plain left to right merging of sorted doc lists (AND, OR, NOT), no precedence.
- PHRASE drives from the rarest term in the phrase and checks the other terms by binary search on `(doc, start + offset)`. The reference retokenizes every candidate document, which is where most of its time goes.
- Queries run in parallel through an atomic counter, then get written in input order.

## Round 2

- The first version kept a `(doc, pos)` posting for every phrase term, which for words like `the` meant millions of entries and about 1 GB. Now each thread keeps one u32 term id per token in a stream, and a phrase is checked by intersecting the two rarest doc lists and comparing id windows in the stream.
- Tokens of 16 bytes or less get packed into a u128 and looked up in an open addressing table, so a lookup is one integer compare and no memcmp.
- Each thread's chunk is one doc range, so every query runs per part and the answers just concatenate. No merged posting lists, which saved about 110 MB.
- Tried and dropped: retokenizing candidate docs instead of keeping the stream. It used less memory but cost 300 ms more on the 232k candidate docs.

## Round 3

- Doc lists used to be pushed one by one into 75k growing vectors, and almost every push was a cache miss. Now each thread counts per term, does a prefix sum and fills one flat array. Two sequential passes over the stream beat one random one.
- The tokenizer builds 64 byte masks for "token byte" and "LF" with SSE2 and walks them with trailing_zeros, instead of a branch per byte.
- The hasher's finish now rotates, because hashbrown indexes with the low bits and a multiply leaves those weak.
- A query only prints its count, its sum and its first ten ids, so the per part answers stop getting concatenated.
- Phrase terms get ids first. There are 33k of them, so the stream fits in u16, and the other query terms only need a deduped per doc list to build their doc lists. If a phrase term ever lands past 65534 it gets a marker in the stream and the doc text gets retokenized to confirm. That path was tested by forcing the cutoff to 1000.
- Tried and dropped: merging into one global doc list per term. Same speed, more memory.

## Round 4

- BOOL was about 70% of query time, mostly long OR and AND chains over the common terms. Any term in more than 1 doc in 32 now gets a bitset over all 250k docs up front (31 KB each, about 16 MB in all). An op on such a term is one word wise pass.
- Queries whose lists add up to more than 2 bits per word run on a scratch bitset: OR sets bits, NOT clears them, AND keeps the list's docs whose bit is set. Doc ids come out ascending, so the first ten fall out of the final scan. Short queries keep the sorted list merges.
- Lower cutoffs used more memory and were no faster.

## Round 5

- Indexing was about 85% of the run (0.39 s of 0.46 s locally on 4 cores), with 26M doc postings for 75,747 terms and 40,350 phrase pairs.
- Each short token's table slot is now prefetched and the lookup happens one token later, so the 4 MB key table miss overlaps the scan of the next token. Locally that's about 8% faster.
- queries.txt is mapped instead of read into the heap.
- Board: 0.358 s, 258 MB, 57.37 points (run-d3fbb7f46f7f), up from 0.379 s and 56.93.
- The pair blocks, which set the peak, now hold u16 entries. Term ids below 0xFFFE take one slot, larger ones take three (an escape and two halves). The common terms come early in the queries, so they get small ids. The local peak went from about 186 MB to 155 MB at the same time. Board: 0.357 s, 215 MB, 58.12 points, a new best, rank 43 to 40 of 81 (run-071271108d80).
- Tried and dropped: terms found in more than 1 doc in 16 within the first 1024 docs going straight into a local bitset instead of the pair blocks. The peak went from 186 MB to 157 MB, but the run was 7% slower and the code longer. With the board's weights (T^0.5, M^0.2, S^0.15) that comes out slightly negative.
- Tried and dropped: filling the dense term bitsets straight from the blocks. The peak is in the blocks, not the lists, so memory didn't move.

## Round 6

- A 32 KB filter of query term hashes sits in front of the 4 MB term table, so a token that can't be a query term skips the prefetch and the probe. Locally that's about 2% faster, but on the board it ran 0.379 s against 0.355 to 0.357 s for the version before it (run-bdb28e0d53a4), so it was reverted. An earlier 128 KB term bitmap came out in 9007210, and this one is a quarter of the size.

## Round 8 (rewrite attempts in a separate folder, none swapped in)

- 79% of document tokens are query terms, so filters and caches that skip tokens can't save much.
- Tried: a direct table for tokens of 3 bytes or less (30% of tokens), skipping the hash. Slower (median 0.320 to 0.333 s, then 0.349 to 0.367 s).
- Tried: dense query terms going straight into bitsets at fill time, so their lists are never stored. Every thread then has to keep its blocks until the global counts are known, so the peak rose from 142 to 163 MB and the median from 0.349 to 0.375 s.
- Tried: two role bits per term (starts or ends a phrase pair) in front of the pair hash. No faster. Taking the pair work out entirely, which is wrong but shows the cost, saves 30 to 55 ms of the roughly 200 ms index build.

## Next ideas

Tokenizing is the largest piece again, then the phrase checks.

## Round 9: phrase-pair lists dropped
Phrase queries now AND the plain term lists and verify each candidate on the document text:
SSE2 scan for the first word's first byte (case folded), then exact whole-token match through
the lowercase table. Words over 16 bytes keep the old tokenizing check. No pair table is built.
Local (full, medians of 4 runs x2): 0.331 s -> 0.343 s (+3.5%), anon peak 147 MB -> 120 MB (-18%).
By the T^0.5 M^0.2 score that is about +0.5 points net. Exact on s7, s42, t5, full.

## x86-64-v3 build
Same target-cpu config as A and D. Exact on s7, s42, t5, full. Local medians about 1-3% faster.

## Round 10: dense terms straight to bitsets (not kept)
Index build split in two: scan, then a global count picks the dense terms (over 1 doc in 32),
and the fill writes those into shared atomic bitsets instead of lists (10.1M of 22.2M postings).
Exact after fixing an overflow in the big-query test. But the peak stayed at 122 MB (it sits in
the scan phase, where the u16 blocks hold every posting) and medians went 0.305 s -> 0.312 s.
Kept in /workspace/b-rewrite only. To cut the peak, the scan itself has to stop emitting dense postings.

## Round 11: common terms skip the lists in the first pass
A 256 KB sample of docs.txt picks the terms in over 1 doc in 48 (a margin under the 1 in 32
line); the last line gives the max doc id, so their bitsets exist before the scan. Their table
value becomes 1<<31 | slot, so the scan ORs a bit into a local 64-doc word per term, flushed
with one fetch_or per word, and never writes a posting or touches last[] for them. Any choice of
terms is exact, the sample only steers memory. Exact on s7, s42, t5, full.
Local: anon peak 120 MB -> 103 MB, medians 0.304 s vs 0.307 s (within noise).
Atomic fetch_or per posting was +16%, and keeping the bslot side lookup was +5%.

## Round 12: less code (S axis)
S is scored against the reference's 91 lines at exponent 0.15, so dead code costs points.
Removed the disabled phrase-pair table and its scan branch, and the tokenizing phrase check:
phrases now keep their words as byte slices and every phrase goes through the text scan, long
words included. rustfmt'd code lines 943 -> 827. Exact on s7, s42, t5, full; medians unchanged.

## Round 13: std HashMap
The term maps are only used while reading queries and for tokens over 16 bytes, so the custom Fx hasher went. rustfmt lines 827 -> 800. Exact on s7, s42, t5, full; medians 0.303 vs 0.299 s.

## Round 14: pack() for the sample and the table keys
rustfmt lines 800 -> 789. Exact on s7, s42, t5, full; medians the same within noise.

## Round 15: u16 doc ids in the lists
Lists hold doc ids local to their part as u16 (the part adds lo when it hands ids out); a thread closes its part and starts a new one every 65,536 docs, so any thread count or doc count works. Exact on s7, s42, t5, full, and also with parts forced to close every 1,024 docs. Local anon peak 103 -> 82 MB; medians the same (0.300/0.315 vs 0.292/0.314 s).

## Round 16: scalar phrase scan (not kept)
Replacing the SSE2 first-byte scan in has2 with a byte loop cuts 6 lines but is 8-11% slower in three paired runs. Reverted.

## Round 17: query terms straight into the scan table
Reading the 50,000 queries took 20-36 ms: a HashMap of 86k distinct words hit with 300k lookups, then rebuilt into the u128 table. Terms now go straight into the open-addressing u128 table (grown at half full, same final size), long ones into a small map, so there is no second build. Exact on s7, s42, t5, full; paired medians 0.329/0.326/0.306 -> 0.305/0.296/0.285 s, peak 82 -> 79 MB.

## Round 18: 8-byte probe slots
The scan probes slots of 32 hash bits over the value (8 bytes, table half the size of the u128 keys); keys[j] is read only when the hash bits match. vals is freed once the slots exist. Exact on s7, s42, t5, full; peak 79 -> 80 MB; faster in 5 of 6 later paired runs, e.g. 0.292/0.294/0.285 -> 0.275/0.280/0.273 s.

## Round 19: one access per short-token lookup, 2 MB pages, one line per term
Profiled on 4 cores (perf, software clock): the scan was 55% of CPU, and knocking pieces out showed tokenizing alone at about half of it, lookups and postings the rest. The hottest loads were the second access into the 4 MB `keys` table, the stall at the slot prefetch (TLB), `last[t]`, the `lock or` that flushes the common-term words, and `cnt[t]`.
- `pack` loads 16 bytes at once and ORs in 0x20 (token bytes are letters and digits, and bit 5 lowercases both), instead of a variable-length copy and a per-byte case fix.
- A slot is 16 bytes: the token's low 8 bytes, then its value and a 32-bit tag of its high 8 bytes. Tokens of 8 bytes or less (most of them) are matched with one access; longer ones also check `hi[j]`. The table is asked for 2 MB pages (MADV_HUGEPAGE) before it is filled.
- A term's last doc and list length sit side by side in one `[u32; 2]`.
- Common-term words between a thread's first and last word hold only its docs, so they are written with a plain store; only the first and last words use `fetch_or`.
Exact on s7, s42, t5, full. Paired, 11 alternating runs: 0.625 -> 0.579 s (-7%), CPU 2.20 -> 2.00 s, cgroup peak 79 -> 80 MB. rustfmt lines 857 -> 866.
Board: run-a96a453972ab, 0.319 s, 95.3 MB, 62.44 points. That run was slow on every track (unchanged C went 0.058 -> 0.068 s), so it says little.

## Round 20: phrases anchored on their rarest word
214,226 candidate docs went through the phrase check, 19,332 matched, and it scanned 438 MB of doc text looking for the first byte of the phrase's first word (often `t`, `a` or `o`), calling the matcher at each hit. The check now looks for the first two bytes of the phrase word with the shortest doc list (ties: the longest word), case folded, 32 bytes a step with AVX2, then matches the words after it forward and the words before it backward, all as whole tokens. Paired, 11 alternating runs: 0.574 -> 0.494 s (-14%).
The count and sum of a big BOOL answer came from 8 byte-table lookups per bitset word, in a loop whose counter the compiler kept on the stack. Bit k of a position is set exactly on the positions in a fixed mask, so a word's positions sum to six masked popcounts shifted by k. 0.532 -> 0.514 s (-3%), CPU -5%.
Exact on s7, s42, t5, full. Both together against round 19: 0.609 -> 0.566 s in one paired set of 11 (the box is noisy), peak 80 MB. rustfmt lines 866 -> 901.
Board: run-96a07446a51b, 0.2487 s, 97.2 MB, 64.94 points, a new best, rank 34 -> 28 of 85.

## Round 21: term ids by frequency, tables split by token length, u16 counters
- Term ids are renumbered after the sample, most docs first. The common terms are then ids 0..nd, so the scan tests `t < nd` instead of a flag bit and a slot table, and the next most frequent terms' counters sit together. -1.5% wall, -3.5% CPU.
- 20% of doc tokens are 9 to 16 bytes, and each of those paid a second miss on the high-half check. Short tokens (8 bytes or less, 49,402 of the 75,747 query terms) now go to a table of 16-byte slots [bytes, value], longer ones to a table of 32-byte slots [low, high, value, 0]. Every lookup is one access, the tables are 2 MB each instead of 4 + 2 MB, and they are page aligned (from mmap, so no 32-byte slot straddles a cache line) and on 2 MB pages. -1% wall, -3% CPU, peak -2 MB.
- A part closes at 65,535 docs instead of 65,536, so a term's last doc and its count both fit in u16: the per-thread counters go from 690 KB to 345 KB. -5%. Checked with parts closing every 1,000 docs and with 12 threads forced, since with 4 threads no thread here reaches a second part.
Exact on s7, s42, t5, full. Against round 20, 11 alternating runs: 0.561 -> 0.526 s (-6%), CPU 1.87 -> 1.75 s, peak 80 -> 77 MB. rustfmt lines 901 -> 932.
Board: run-fbcf3441c1ac, 0.2499 s, 92.5 MB, 64.99 points, rank 27 of 85; later runs of the same code: 0.3168 s (run-1c84a6dbc125, a slow run on every track) and 0.2492 s, 91.0 MB, 65.09 points (run-b006af567da4).

## Round 22: tries that lost (not kept)
- A ring of 2 to 8 pending lookups instead of one, flushed at each doc end: depth 4 vs 1 moved the scan -9 ms but the total not at all, and depth 8 was slower. Profiling a tokenize-only build (lookups and postings cut out: scan 135 ms of 250 locally) shows a fifth of it stalled on the prefetches themselves.
- Query terms got their ids in a second pass that prefetches the table 8 words ahead: -2.5 ms of the 35 ms parse. Stubbing the term lookup out leaves 13.5 ms, so the parse is allocations, splitting and page faults more than table misses. A table sized up front: no change.
- Dense-term cutoff at 1 doc in 24 or 16 instead of 48: peak 76-78 MB either way, time within noise. A memory trace (cgroup usage every 2 ms) puts the peak in the fill, when every part's lists are allocated while all the blocks are still alive: 49 MB at the end of the scan, 71-78 MB in the fill, 50 MB after.
- The fill in two waves of half the parts: peak 75-78 -> 71-72 MB with 12 threads, but the fill took 24 -> 40 ms locally.

## Round 23: less at the fill peak
- The two token tables are mmap'd, so nothing freed them; they are unmapped (a Drop on Probe) right after the scan, before the fill.
- The fill used a copy of each part's offsets as cursors (300 KB a part). It now moves the offsets themselves from each term's start to its end, which is the next term's start, and shifts them back by one.
With 12 threads forced, the cgroup peak goes from 77-78 MB to 66-70 MB, and a 2 ms memory trace now tops out at 66 MB in the fill (61 MB at the end of the scan, 57-59 MB in the queries) where it had a spike to 78 MB. Exact on s7, s42, t5, full; 11 paired runs 0.514 -> 0.503 s (noise).

## Round 24: query parse on all threads (not kept)
The serial query parse is about 35 ms locally, half of it a cache miss per word in the 4 MB key table. Parsing line-aligned shares of queries.txt on all threads, then numbering the words in hash buckets (each thread sorts and dedups one bucket; id = bucket base + rank) and building the scan tables straight from the buckets, took the phase from 35 to 23 ms with exact output at 4 and 12 threads. But the 12-thread peak rose by about 5 MB (query objects allocated across twelve malloc arenas); malloc_trim did not help, and one arena for all threads (M_ARENA_MAX 1) made the query phase 110 ms slower through lock contention. About -5% T against +6% M on the board is no gain, for 20 more lines, so it stays out.

## Round 25: query parse on all threads, again
Round 24's parallel query parse lost only on memory: its big temporaries (each thread's words, each bucket's sorted keys) were freed into twelve malloc arenas and stayed there. With the mmap threshold at 64 KB while the queries are read and numbered, and back to 1 MB before the scan, they go back to the kernel: 12-thread peaks 72/69/75 MB against 72/73/68 MB before. The phase goes 37 -> 25 ms on 4 cores (11 alternating runs), more with 12 threads, and it is serial time on the board, where round 20's query-side cut showed up larger than locally. Exact on s7, s42, t5, full, also with 12 threads forced.

## Round 26: zeroed tables from calloc
The 19 MB of common-term bitsets (and the word ranks) were built by collecting `AtomicU64::new(0)`, which writes every byte on the main thread before the scan starts. They now come from `alloc_zeroed`: calloc takes fresh zero pages from the kernel, nothing is written up front, and the pages fault in on the scan threads that set the bits. Scan -4.5 ms, total -8 ms over 11 alternating runs. Exact on s7, s42, t5, full. (The same in D made its BFS 22 ms slower: there each slot is read before its CAS, so every page faults twice, once for the shared zero page and again on the write. D keeps its loop.)

## Round 27: per-query work out of the per-part loop
With 12 threads there are 12 parts, and every query is answered part by part. A phrase redid its setup in each part: clone, sort and dedup of its terms, the dense/list split, and the anchor choice through `tlen`, which itself sums over all parts, so that was quadratic in the part count. Every BOOL operator also allocated a new list in every part. The setup now runs once per query (the list terms are ANDed in order of their total length instead of each part's), and the list ops write into two buffers each thread reuses. 12 threads forced, 11 alternating runs: query phase 205 -> 182 ms, total 594 -> 557 ms; at 4 threads (4 parts) -3 ms. Exact on s7, s42, t5, full, also with 12 threads.

## Round 28: round 25 out again
Board run-72bb1848af23 (rounds 25-27): 0.2441 s but 92.5 MB, against 86.9 MB before. With transparent huge pages set to `always` locally (the board's kernel evidently backs anonymous memory with 2 MB pages: A's and C's arena and calloc changes in the same run cost 6 to 8 MB there too), the 12-thread peak of round 25 is 83-84 MB against 68-79 MB for round 23; rounds 26 and 27 without 25 are at 69-75 MB. So the parallel query parse is out (its many 64 KB+ temporaries cost whole huge pages), and 26 and 27 stay. Exact on s7, s42, t5, full. Local memory checks from here on run with THP set to `always`.
Rounds 26 + 27 on the board: run-1b96537f4b64 0.2485 s, 86.5 MB; run-0737b5e506ee 0.2452 s, 86.7 MB. No visible change from round 23.

## Round 29: the fill in two waves (board experiment)
The board's peak is the fill: with 12 real cores every scan thread finishes at about the same time, so all 12 parts hold their blocks and their new lists at once (about 61 + 24 MB, close to the board's 86-87 MB). Filling half the parts, then the other half, keeps a quarter of the postings fewer alive at the peak. Locally the spike barely shows (12 threads on 4 cores never overlap fully: peaks 67-72 against 70-74 MB) and the fill time is the same, so this one is judged on the board: it should take about 12 MB off M and add a few ms of fill. Exact on s7, s42, t5, full.
Board: run-abf05797d461, 0.2433 s, 75.9 MB (from 86.7), 66.05 points, a new best, rank 27 -> 24 of 87. No time cost visible.

## Round 30: three waves
At the fill peak, w waves keep about 24/w MB of lists alive on top of the 61 MB the scan ends with. 12 threads forced, three runs each: 2 waves 68-70 MB, 3 waves 66-67, 4 waves 65-67. Three, since each wave fills fewer parts at once. Exact on s7, s42, t5, full.

## Round 31: common-term cutoff at 1 in 24
With the fill spike gone, the peak is where the scan ends: the blocks plus the bitsets. A term near the old cutoff (1 doc in 48 of the sample, about 5,200 docs) costs 31 KB as a bitset but about 14 KB as postings plus a third of its list. 12 threads forced, three runs each: cutoff 48: 66 MB; 32: 61-63; 24: 61-62; 16: 62. Time, 11 alternating runs: 24 beats 48 (0.590 -> 0.557 s) and 32 (0.600 -> 0.568 s); 16 is slower in the queries (+14 ms). Exact on s7, s42, t5, full.
Rounds 30 + 31 on the board: run-14c0632534ec 0.2509 s, 67.1 MB, 66.25 points (a new best, rank 24); run-c5579e17d545 0.2566 s, 67.2 MB. Memory went 75.9 -> 67 MB as expected, but both times are a few percent above round 29's 0.2433 s.

## Round 32: two waves with the 1-in-24 cutoff (board experiment)
To see whether the third wave is what costs the time on 12 real cores, this keeps the 1-in-24 cutoff and goes back to two waves. Exact on s7, s42, t5, full.
Round 32 on the board: run-7cbce8dfc2fe 0.2489 s, 69.7 MB; run-ff5ca3c4b651 0.2453 s, 69.6 MB, 66.34 points, a new best; run-af6ae6c80348 0.2704 s (noise, same code).

## Round 33: mmap threshold 128 KB (board experiment)
Each scan thread keeps about 1 MB of counters, offsets and doc starts below the old 1 MB threshold, so in its malloc arena, and a kernel with THP set to `always` backs an arena heap in 2 MB pages. At 128 KB those buffers get their own small mappings. With THP `always` locally and 12 threads, peaks of 59-63 MB against 63-64 MB; time the same (11 alternating runs at 4 and 12 threads). Exact on s7, s42, t5, full, h99, h3.
Reverted: board run-7757b367dc86 gave 0.2894 s at 67.3 MB. The run before it on unchanged code was also slow (0.2704 s), but many small mappings cost A time on the board through the address-space lock, and 2 MB is not worth that risk.

## Round 34: queries stored flat
Each query was its own Vec (BOOL) or two Vecs (phrase), plus a Vec of words per line while parsing: about 150,000 small allocations on the one thread that reads the queries, before anything else can start. A BOOL query is now a range of (op, term) pairs in one array and a phrase a range of terms in another, with its words in a third; renumbering is two flat loops, and the query phase gets slices. Query parse 40 -> 29 ms over 15 alternating runs; end to end 0.554 -> 0.523 s (13 runs); 12-thread peak 62-63 MB against 63-67 MB. Exact on s7, s42, t5, full, h99, h3.

## Round 35: flat queries read on all threads
Round 25's parallel parse, again on top of round 34: each thread reads a line-aligned share of queries.txt into flat arrays whose terms are word positions; the words then get ids in hash buckets on all threads (each thread sorts and dedups one bucket), and the scan tables are built straight from the buckets, which drops the growable term table. Round 25 lost about 5 MB on the board because twelve threads each allocated thousands of small query vectors in their own arena; flat arrays leave nothing small behind. Query parse 30.5 -> 20.5 ms at 4 threads (11 alternating runs); with 12 threads time-sliced on 4 cores it is flat, as every bucket thread hashes all 300k words (about 1.5 ms each on 12 real cores). 12-thread peaks with THP `always`: 60-70 MB against 63-65. Exact on s7, s42, t5, full, h99, h3, also with 12 threads.
Reverted: board run-6397ea3d3c86 gave 0.2593 s at 78.6 MB, 8 MB above round 34 (run-99ae00b4308c, 0.2522 s, 70.2 MB). Without per-query vectors the parallel parse still leaves its large temporaries (every word, the ranks, the bucket sorts) on the board's 2 MB pages, and the time gain did not show. Round 34's serial flat parse stays.

## Round 36: query terms numbered in a second pass, with prefetch
The query parse is serial time before the scan can start: about 35 ms locally, and most of it was the term table. Each word looked itself up in a 5 MB table the moment it was read, a cache miss after a cache miss, and the table also grew by rehashing. Now the lines are cut into words first (one pass over the bytes, the operator told apart by its first letter), and the table then numbers all words in a second pass, sized once for every word, with each word's slot prefetched 16 words ahead so the misses overlap. Ids come out in the same first-seen order. `words` is sized up front so it gets its own mapping and leaves nothing in the heap; with THP `always` the 4-thread peak is unchanged (57-59 MB both).
Query parse 37.8 -> 17.5 ms (15 alternating runs). Phase medians over 11 alternating runs: queries 33 -> 17 ms, end to end 617 -> 561 ms. Exact on s7, s42, t5, full, h99, h3.

## Round 37: fewer instructions per token in the scan
callgrind on s7 counted about 118 instructions per token in the scan. Three cuts, the scan otherwise unchanged:
- masks() builds the token and LF bitmasks of 64 bytes from two 32 byte AVX2 compares (a byte is a letter or digit when it is at most 25 or 9 above 'a' or '0', tested with an unsigned min) instead of four 16 byte SSE2 ones, and only the last 64 bytes of the file go through a zero padded copy; it used to zero a 64 byte pad on every call.
- key() packs a token of up to 16 bytes from one 16 byte load, lowercased with bit 5 and cut to its length by a sliding byte mask, instead of a copy and a 128-bit shift.
- A token that runs to the end of a 64 byte window is finished in the next window's masks, which the scan goes on with, instead of byte by byte through the lowercase table.
s7: 492M -> 451M instructions (-8%). 15 alternating runs: 651 -> 635 ms median, CPU flat; peak 57-58 MB as before. Exact on s7, s42, t5, full, h99, h3.
Reverted (round 38): board run-714d2f47ce0f 0.2877 s and run-ec28d4f60b9a 0.2919 s, against 0.2287 and 0.2445 s for round 36 just before. Fewer instructions locally, but clearly slower on the scoring box, so the scan goes back to round 36.

## Round 39: answers in runs of 64 queries
Each of the 50,000 answers was its own small Vec (and its own `Vec::with_capacity(10)` for the first ids), allocated on whichever thread answered it, then copied into the output one by one on the main thread. Queries are now handed out 64 at a time and a run's answers go into one buffer, with one first-ids vector per thread: 782 buffers instead of 100,000 small allocations across twelve malloc arenas, and 782 copies at the end. 15 alternating runs: 555 -> 550 ms; peak 57-59 MB as before. Exact on s7, s42, t5, full, h99, h3.

## Round 40: 1 MB posting blocks
A scan thread writes its (doc, term) pairs into blocks of 2M entries, 4 MB each, sized up front. On the board's 12 threads each thread ends with about 2.4 MB in its last, part-filled block, and with transparent huge pages a block of 4 MB is backed in 2 MB pages, so the touched part of the second half costs a whole huge page. Blocks of 512K entries (1 MB) are still over the 1 MB mmap threshold, so each is its own mapping and is handed back when the fill has read it, but too small to hold an aligned 2 MB page. With THP `always` and 12 threads forced, three runs each: 4 MB blocks 63/67/65 MB, 1 MB blocks 62/62/59 MB (512 KB blocks fall into the malloc arenas: 75 MB). 15 alternating runs at 4 threads: 563.7 -> 562.3 ms. Exact on s7, s42, t5, full, h99, h3.
Rounds 39 + 40 on the board: run-ab5c70f27fa2 0.2391 s, 68.4 MB (from 70.4); run-e3845909538f 0.2254 s, 68.6 MB, 67.13 points, a new best (from 66.90).

## Round 41: the docs' page tables dropped by the answering threads
At exit the kernel tore the 254 MB docs mapping down on one thread. The docs are read until the last phrase is checked, so nothing can go during the scan; instead each answering thread, once no queries are left to hand out, drops one share's page table entries with MADV_DONTNEED (the file stays in the page cache; a query still running elsewhere that reads there faults the page back in). 15 alternating runs: 591.4 -> 571.3 ms. Exact on s7, s42, t5, full, h99, h3.

## Round 42: glibc linked statically
`-C target-feature=+crt-static` in .cargo/config.toml: the binary is static-pie, so a run starts without the dynamic loader mapping libc and resolving its symbols. A trivial program goes from 2.27 to 1.77 ms exec to exit (40 alternating runs); on A, 21 alternating runs 67.8 -> 66.3 ms. Peak memory the same or 1 MB lower. The build image (rust:1-slim-trixie) has libc.a. Exact on s7, s42, t5, full, h99, h3.
Reverted (round 44): board run-ca0df2585d96 0.2357 s and run-ee17ad2e8e9f 0.2838 s, against a median of 0.229 s over the nine runs of rounds 36-42. Like round 37, faster locally and slower on the scoring box; the scan goes back to taking its own page faults.

## Round 45: query temporaries dropped before the scan
The query ranges (`qr`, 1.2 MB, its own mapping) and the renumbering's three tables (counts, order, new ids: 300 KB each) lived to the end of the run, through the scan's and the fill's peak, though nothing reads them once the queries are built. They are dropped right there now. With THP `always` and 12 threads forced, four runs each: peak 60-63 MB against 58-61 MB. Exact on s7, s42, t5, full, h99, h3.
Board: 0.2237 s at 66.3 MB (from 67.5-68.7 MB), 67.32 points.

## Round 46: one 1 MB fingerprint table for the scan (board experiment)
A standalone benchmark of the scan's lookup alone (single thread, 6M tokens of the full corpus, keys already packed) puts its cost in the table's size, not in the probing code: one load per token into a table of 32 KB costs 2.6 ns, 512 KB 3.6 ns, 2 MB 6.0 ns and 8 MB 12.2 ns, and prefetching 16 tokens ahead changes little. The two exact-key tables (2 MB for tokens of up to 8 bytes, 2 MB for 9 to 16) came to 15-18 ns a token, part of it the branch between them: a fifth of the tokens are 9 to 12 bytes long. Tried and dropped on the way: buckets of four 12-byte keys (4 MB), a small table of the most common terms in front of the big one (the hit-or-miss branch costs more than it saves), 8-byte (tag, id) slots with the key checked in an id-ordered array (the check is a second, dependent miss), and per-doc batches of lookups.
Now every query term is in one table by a 64-bit hash of its bytes: buckets of one cache line hold eight (44-bit fingerprint, 20-bit id) slots, compared at once with AVX2, about 1 MB for the 75k terms. Terms of 17 to 32 bytes go in the same table, so the separate map lookup in the scan goes too. A token that is not a query term would be taken for one only if it matched a term's bucket and fingerprint, odds of about 2^-56 per pair, about one run in a million over the corpus's ~600k distinct tokens; two query terms that match are caught while building, and the hash then takes another seed. (Track C tells names apart by a 64-bit hash already.)
Scan phase, single thread, 5 alternating runs: 1153 -> 1023 ms (-11%). With 4 threads: scan 262 -> 259 ms, total 492 -> 483 ms (9 runs), so locally the gain mostly goes once every core is busy; the board decides. Peak 56 MB as before; 8 fewer counted lines. Exact on s7, s42, t5, full, h99, h3 and a fresh full-scale seed (77), also with 1, 3, 5, 7 and 12 threads.
Board: run-feaea63b1173 0.2400 s and run-756388053758 0.2421 s, against 0.2336-0.2461 s for the last three runs of round 45 (the box has been slower tonight; round 45's median over 24 runs is 0.228 s). Not settled, so the board gets the two versions alternately.

## Round 47: round 46 out again (board A/B)
Round 45's scan tables back, to compare with round 46 on runs of the same hour.
Board: run-9aaf9a38970b 0.2262 s for round 45 in the same hour as round 46's 0.2400 and 0.2421 s. Round 46 stays out: as with round 37, a scan that measures faster on one local core (here -11%) is no faster, or slower, on the scoring box's twelve threads.

## Round 48: tried and not kept (local only)
- The scan without the per-term "last doc, count" array (`lc`), the fill counting and dropping repeats from the blocks instead (only 0.8% of the listed postings repeat within a doc): scan -5% on one thread, but with 4 threads the scan was flat and the fill's extra pass cost 10-40 ms.
- Phrase anchors with recorded offsets: each phrase's rarest word in the sample is its anchor, the anchors get ids next to each other, and the scan writes where an anchor first shows up in each doc (and whether it shows up again), so the phrase check looks there instead of searching the doc's text (the check read 438 MB of text over 214,000 candidate docs, for 19,000 matches). Query phase 159 -> 116 ms, total 524 -> 489 ms (-6.5%, 15 alternating runs), exact everywhere, but the sample picks anchors with 1.6 million postings between them (the true rarest words would have 835,000): peak 56 -> 61 MB and 58 more counted lines. With the board's weights (T^0.5, M^0.2, S^0.15) that is about +0.2 points, not worth the code.

## Round 49: the docs a part at a time, every thread on each part
Until now each thread scanned its own twelfth of the docs into its own part, so when the scan ended every part's postings were held at once (about 26 MB of blocks, plus the bitsets), and that was the peak: a memory trace with 12 threads tops out at 57-60 MB right at the end of the scan, 46 MB after the fill. Now the docs go a part of about 50,000 docs at a time (5 parts at full scale) and every thread works on each part in turn:
- the part is cut into one piece per thread, and the pieces are scanned as before (each with its own blocks, per-term doc counts, and the common terms' bits in the part's bitsets);
- each piece's counts become where its postings of each term go (term first, then piece, so every list comes out ascending), and every thread fills its own piece's postings into the shared lists; the places never overlap;
- every query is answered on the part, its count, sum and first ids added to what the parts before it gave (parts come in doc order);
- then the part goes, and its text's page tables with it.
Only one part's postings and lists are ever held. Peak with THP `always`: 56 -> 31 MB at 4 threads, 59 -> 36 MB with 12 forced (three runs each). Time over 21 alternating runs at 4 threads: 533.6 -> 533.8 ms (the scan pays about 25 ms for a wait at the end of each part's scan, the fill gains 18 ms from running on every thread instead of in two waves, the queries gain 14 ms from 5 parts instead of 12 and bitsets of a part's size); with 12 threads forced on 4 cores +5%. Parts of 63,000 docs (4 parts): same time, 2-4 MB more; of 32,000 (8 parts): 27 MB but 6% slower. 22 fewer counted lines. Exact on s7, s42, t5, full, h99, h3 and the fresh full-scale seed 77; with 1, 2, 3, 5, 7, 12 and 16 threads on s7, t5 and h3; with parts forced to 1,000 docs (250 parts) and forced too big (a part over 65,534 docs makes the cut double the part count); and on an empty docs.txt.
Board: run-7a7e88468f9a 0.2494 s, 55.6 MB (66.94 points); run-f8baffe6d89b 0.2541 s, 56.1 MB (66.70). Memory went down only 66 -> 56 MB there (36 MB locally with 12 threads: the scoring box charges about 20 MB more for this layout, against 6 MB more for round 45's) and time went up 11%, where locally it was flat at 4 threads and +5% with 12 forced on 4 cores. Each part adds three points where all threads wait for the slowest (five parts: fifteen), and 180 thread starts instead of about 30. About -0.4 points against round 45's usual 67.2.

## Round 50: round 49 out
Back to round 45 (exact on s7, s42, t5, full, h99, h3).

## Round 51: the scan's tokens found a window at a time
Taking the scan apart at 4 threads (each part replaced by nothing in turn): of its 330 ms, finding the tokens and packing them was about 150 ms, the table lookups 100 ms and recording the postings 80 ms. Finding the tokens alone, in a standalone loop over the same file, took about 50 ms, and reading every byte of the mapped file 15 ms, so the walk itself was costing more than the data it reads. It went token by token through the window's bits, with a branch for a token that ran past the window, one for a window with nothing left in it and one for the LF, each taken at random.
Now each 64-byte window gives all its tokens at once: its token bits up to the doc's LF, a token's start where a token bit follows a separator, its end where a separator follows, and the k-th start pairs with the k-th end. A token that reaches the window's end is left for the next window, which starts where that token does; a run of 64 bytes or more (no query term is longer than 32) is skipped byte by byte. 15 alternating runs, twice: 571 -> 558 ms and 611 -> 552 ms, CPU 1892 -> 1809 and 1924 -> 1844 ms (-4%). 2 fewer counted lines. Exact on s7, s42, t5, full, h99, h3, the fresh full-scale seed 77, with 1, 3, 5, 7 and 12 threads, and against the Python reference on a small set with runs of 64 to 130 letters.

## Round 52: the table picked by index (local only, not kept)
A fifth of the tokens are 9 to 16 bytes, so the branch between the two scan tables (8-byte keys and 16-byte keys) looked like a misprediction on many tokens. The tables became one type with the slot width as a field, kept in a two-entry array indexed by `l > 8`, and the key compare masks the high half off for 8-byte slots. Exact on s7, s42, t5, full, h99, h3, but two sets of 15 alternating runs gave +4% and +13% (CPU +4% and +12%): the indexed width costs more per lookup than the branch did. Not kept.

## Round 53: each part filled by two threads
The fill runs in two waves of half the parts each, to keep half the lists off the peak, so during the fill half the threads have nothing to do (6 of 12 on the board, 2 of 4 here); with one wave the fill phase took half the time (74 -> 37 ms at 4 threads) but every part's lists and blocks would be held at once. Now each part is filled by two threads, so the waves keep their memory and use every thread: halfway through a part (by bytes) the scan starts a new block and keeps each term's doc count so far (u16, 150 KB a part), and the second thread fills the postings from that block on, each term's starting at its start plus the first half's count. Both write the same lists at places that never overlap. Fill phase 51 -> 31 ms at 4 threads; end to end, two sets of 15 alternating runs: 544 -> 509 ms and 520 -> 502 ms, CPU the same. With 12 threads forced on 4 cores there are no idle cores to use and it is +3%. Peak with THP `always` and 12 threads: 59-61 MB both. 25 more counted lines. Exact on s7, s42, t5, full, h99, h3, the fresh full-scale seed 77, with 1, 2, 3, 5, 7, 12 and 16 threads (1 to 3 threads close parts at 65,535 docs, before or after their halfway point), against the reference on the long-token set, and on an empty docs.txt.
Board: run-2737d7ea10fa 0.2154 s, 69.3 MB, 67.46 points (A, unchanged, ran slow in the same run: 0.0345 s); run-2571ac333ec6 0.2180 s, 68.6 MB, 67.37 points. The fastest B times so far (round 45: 0.222-0.230 s on a quiet box), but 2.5-3 MB more memory, which takes back most of the time's points. The extra is the halfway counts of every part (12 x 150 KB) held from the scan to the fill, plus the second thread's u32 cursors (6 x 300 KB in a wave).

## Round 54: the second half filled backward, no counts kept
The second thread does not need the first half's counts if it fills each term's postings backward from the term's end: its cursor counts down from the end, and when both threads are done each term's second-half run is turned around (one pass in list order). So the scan keeps only where the second half starts (block, doc), and the fill holds two u16 cursor arrays a part (300 KB) instead of 150 KB of counts per part from the scan on plus 300 KB. Tried on the way: u16 cursors on top of the starts with the halfway counts kept (saves 0.9 MB of round 53's 3.6, same time); splitting each part's fill by term range instead (no counts at all, but both threads read every block and half the postings take a mispredicted branch: fill 58 -> 79 ms, slower than one thread).
Peak with THP `always` and 12 threads, five runs each: round 51 59-60 MB, round 53 60-63 MB, this round 56-62 MB (median 59, as round 51). 15 alternating runs: -5.7% against round 51, +0.6% and -3.5% against round 53. 27 more counted lines than round 51. Exact on s7, s42, t5, full, h99, h3, the fresh full-scale seed 77, with 1, 2, 3, 5, 7, 12 and 16 threads, against the reference on the long-token set, and on an empty docs.txt.
Board: run-511a55a4bfdc 0.2179 s, 66.2 MB, 67.53 points: round 53's time (its third run was run-dcb11107cc67, 0.2131 s, 68.4 MB, 67.63 points, the best B so far) at round 45's memory.

## Round 55: the start before the scan
Everything before the scan runs on one thread: reading the 50,000 queries, the table of their words, the sample and the renumbering, about 26 ms here, which on the board's 12 threads is a bigger share of the run than here. A profile of that part had a quarter of it in page faults and copies of growing vectors, and the word table packed each word with a copy of its own length. Now the query vectors get their room up front (`ops` and `pt` an eighth of the file's bytes, `qr` a sixteenth), and a word is packed from the query text after it, so `pack` reads 16 bytes at once. Start to scan, 21 alternating runs: 31.1 -> 28.8 ms; end to end the 2 ms is under the noise (21 runs: +0.6%). Same line count. Exact on s7, s42, t5, full, h99, h3 and the fresh full-scale seed 77.
Board: run-ccfdff783b09 0.2129 s, 66.0 MB, 67.79 points, a new best; run-aab94224a1c9 0.2127 s, 66.6 MB, 67.77.

## Round 56: the scan tables built from the terms, not the word table
Timed step by step, the start before the scan was: queries 6.9 ms, word table 9.7, sample 3.7, renumbering 2.9, the two scan tables 3.6. The renumbering rewrote every id in the word table (262,144 slots, 5 MB), and each scan table then went through all of it twice (once to count its terms, once to insert them). Now the terms come out of the word table once, with their new ids (75,747 of them), the word table goes right there, and each scan table is built from that list. Start to scan, 21 alternating runs: 31.1 -> 30.4 ms. 9 fewer counted lines. Exact on s7, s42, t5, full, h99, h3, the fresh full-scale seed 77, with 1, 3 and 12 threads, against the reference on the long-token set, and on an empty docs.txt.

## Rounds 57-58: tried and not kept (local only)
- Round 57: each term's list length over all parts summed once after the fill (300 KB), instead of over the parts for every query that asks (with 12 parts that is 12 scattered reads per term, and the phrase setup sorts by it). Query phase with 12 threads forced, 21 alternating runs: 194 -> 198 ms, no gain; those reads were not a cost.
- Round 58: the scan threads finish 100-150 ms apart here (which thread is last changes from run to run, so it is the machine, not the data; on the board a thread sharing its core with a busier one lags the same way), so each thread filled its own parts as soon as its scan was done, with at most half the threads' parts filling at once to keep the waves' memory. With the second-half helper thread: +1.8% and -1.1% over two sets of 15 runs, CPU +2-6% (the helpers make more threads than cores while others still scan); with each part filled by its own thread alone: +11%, CPU +10%. A fill running next to the scan slows the scan more than it saves.
- Round 59: the docs cut into two pieces per thread, handed out to the scan as threads come free (each piece its own part, put back in doc order before the fill, which then fills half the threads' worth of parts at a time), so a slow thread no longer holds up the end of the scan. Two sets of 15 alternating runs: -2.6% and +0.1%. The scan ends more evenly, but twice the parts cost the queries about as much: every query runs once per part.
- Round 60: the table for tokens of up to 8 bytes (2 MB, 16-byte slots) halved: a token of up to 7 bytes fits 42 bits at 6 bits a byte (digits 16-25, letters 33-58, by one PEXT), leaving room for its id in one 8-byte slot, with exact keys still and one load a lookup; 8-byte tokens join the 9-16 byte table. After C's round 34 (a smaller lookup, -3% on the board) this was worth a try, but two sets of 15 alternating runs gave -3.1% and +0.9%, for 14 more lines.

