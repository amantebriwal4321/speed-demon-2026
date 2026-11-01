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
