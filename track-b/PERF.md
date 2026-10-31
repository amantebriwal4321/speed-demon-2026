# Track B: performance log

Step 1 local numbers came from an 8 thread Linux box. T and M from step 2 on are the board's own numbers, sha256 matching the published `f86973c2...`.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 56.35 | 2,200 | 0 |
| 1 | Rust, queries first, index only the query terms, positional postings, parallel build and parallel queries | run-a80f93dff992 | 0.798 | 989 | 46.13 |
| 2 | token stream of term ids for phrases instead of (doc, pos) postings, packed u128 token table, queries run per part, mmap | run-52fac7961fbc | 0.631 | 366 | 52.19 |
| 3 | doc lists by counting sort over the stream, SSE2 tokenizer, better hash mixing, only count, sum and top 10 kept per query | run-fd411d459da0 | 0.417 | 289 | 56.88 |
| 4 | u16 token stream: phrase terms get the low ids, other query terms go to a per doc presence list | run-9a693a205195 | 0.428 | 213 | 57.35 |

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

## Next ideas

The query phase is now the biggest piece, mostly long OR and AND chains over the common terms. Bitsets for the most frequent terms are next.
