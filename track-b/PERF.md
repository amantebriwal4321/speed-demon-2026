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
