# Track C: performance log

Step 1 local numbers came from an 8 thread Linux box. T and M from step 2 on are the board's own numbers, sha256 matching the published `d8088b1c...`.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 14.44 | 367 | 0 |
| 1 | Rust, hand written CSV state machine, file split on CRLF, per thread aggregates | run-67fbf32e31ef | 0.702 | 517 | 28.41 |
| 2 | mmap, names interned to ids with per country bitsets instead of one big set of strings | run-b17422467276 | 0.265 | 159 | 43.01 |
| 3 | SWAR field scans, counting sort for country codes, bucketed parallel merge | run-e398408adb14 | 0.181 | 137 | 47.53 |
| 4 | one shared name table filled with CAS, keyed by raw slices, atomic per country bitsets | run-16dc8dca7b0a | 0.098 | 20 | 63.02 |
| 5 | tried a name arena (run-54f93b57d220, 0.097 s but 44 MB, 59.64 points), reverted | run-d49a0459162a | 0.098 | 20 | 63.09 |
| 6 | SSE2 quote and comma scan, 16 bytes per step | run-5e77f9bfa29a | 0.090 | 20 | 63.96 |
| 7 | names told apart by a 64 bit hash, no compare against the file | run-a90ced93ea29 | 0.080 | 20 | 65.03 |

## Step 1: why it got faster

- The spec promises a CR only ever shows up as the first byte of a CRLF terminator, never inside quotes. So any position right after `\r\n` is a safe record boundary, and the file can be split for threads without a sequential quote scan.
- The parser is a tiny state machine over bytes. Quoted fields are scanned for the next `"`, and only fields that really contain `""` get an owned unescaped copy. Everything else is a borrowed slice into the file buffer.
- A BOM is checked only at a record start, which is the only place it's legal.
- The region lookup is a flat array indexed by region id, so the join is one array read.
- Distinct names go into a `HashSet<(country, name)>` per thread with a small Fx style hasher, then the sets get unioned. Money stays integer cents the whole way.

## Round 2

- The distinct names merge was the real cost (560 ms). Names now go into one table shared by all threads, filled with compare and swap. A slot holds a 16 bit hash tag, the length and the offset of the first copy in the mapped file, and the slot index is the name id. Each country sets bits in an atomic bitset, so there is no merge at all and memory stays flat no matter how many threads run.
- Comparing raw slices is exact. The only escape is `""` for `"`, so unescaping is one to one, and an unquoted field can't hold a quote.
- The country codes are three capital letters, so a code is a number below 26^3. Marking which codes appear and walking them in order sorts them with no sort call.
- Tried and reverted: copying names into a small arena so the compare stays in cache. Faster locally, but the board measured 44 MB instead of 20 and the points dropped, so it went back out (run-54f93b57d220, then run-d49a0459162a after the revert).

## Round 4

- The field scans now use SSE2 compares and a movemask, 16 bytes per step, with the SWAR loop kept for the tail.
- Tried and dropped: keeping the bytes of short names next to the hash table so a hit never reads back into the file (run-6e59e72f42c5). 0.085 s, but 28 MB instead of 20 MB, and memory costs more points than that time gains.

## Round 5

- Taking out pieces one at a time showed the name table was about 30% of the run. A hit had to compare the name against its first copy, which sits somewhere random in the 200 MB file, so nearly every record paid a cache miss.
- Names are now told apart by a 64 bit hash of their bytes and length, and the table stores only that. With 100,000 names the odds of a collision are about 1 in 10^9. 0.090 s to 0.080 s, memory flat at 20 MB (run-a90ced93ea29).
- Tried and reverted: a full rewrite on quote, comma and LF bitmasks with prefix xor. Correct, but 0.114 s on the board (run-8e622a5ce441).

## Round 6

- Memory was the cheaper lever: the run is within a few ms of the same time locally either way, but anonymous memory went from 16 MB to 9 MB.
- The name table went from 2^18 to 2^17 slots (100,000 names, so under 0.77 full). That halves the table and the per country seen bitsets, 8 MB down to 4 MB.
- regions.csv is mapped like records.csv instead of read into a 2 MB heap buffer.
- Board: 0.0757 s, 13 MB, 67.67 points, rank 55 to 45 of 88 (run-0278becede36).
- Found a stuck local test run from an earlier session using 7 of 8 cores, which had made the local timings of the last rounds noisy. It's killed.

## Round 7

- The name insert now runs one record late. Each record hashes its name and prefetches its table slot and seen word, then does the insert for the record before it, so those two cache misses overlap the parse of the next record. Local CPU time went from 0.37 s to 0.28 s.
- Tried and dropped: keeping 2, 4 or 8 records in flight. No better than 1 locally.

## Next ideas

The per country bitsets of names seen are the next cost: one atomic OR per record into 8 MB.
