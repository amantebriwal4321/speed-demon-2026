# Track B: performance log

Step 1 local numbers came from an 8 thread Linux box. T and M from step 2 on are the board's own numbers, sha256 matching the published `f86973c2...`.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 56.35 | 2,200 | 0 |
| 1 | Rust, queries first, index only the query terms, positional postings, parallel build and parallel queries | run-a80f93dff992 | 0.798 | 989 | 46.13 |
| 2 | token stream of term ids for phrases instead of (doc, pos) postings, packed u128 token table, queries run per part, mmap | run-52fac7961fbc | 0.631 | 366 | 52.19 |

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

## Next ideas

The doc list pushes are now the main cost. A two pass counting build or bitsets for very common terms should help.
