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
- Board: 0.0692 s, 12 MB, 68.59 points, rank 43 of 89 (run-ec706f86af24).
- Tried and dropped: keeping 2, 4 or 8 records in flight. No better than 1 locally.

## Round 8 (measured, not built)

- Giving each thread its own countries would make the seen bitsets private. As an upper bound I swapped the atomic OR for a plain load and store, which is wrong but free. It was only 2 to 3% faster locally. Splitting the countries across threads would need a second pass or a 13 MB record buffer, which costs more than that, so it stays as it is.

## Round 9

- id, region_id and the amount are read with an 8 byte SWAR digit parser instead of a byte loop. The amount is digits, a dot and exactly two digits, so its cents are read directly. Local medians over 25 alternating runs went from 0.069 to 0.062 s and from 0.064 to 0.059 s.

## Next ideas

The per country bitsets of names seen are the next cost: one atomic OR per record into 8 MB.

## Round 10: dynamic chunks (reverted)
Eight pieces per thread pulled from an atomic counter instead of one fixed slice each: medians
0.0538 s both ways, peak unchanged. The fixed split is already balanced, so it stays.

## x86-64-v3 build
Same target-cpu config as A and D. Exact on s7, s42, t5, full. Local medians about 1-3% faster.

## Round 11: less code
The Hasher impl became one fx() function, the 8-byte tail of find() went (position() covers it), and the POP mmap switch went. rustfmt lines 373 -> 337. Exact on s7, s42, t5, full; medians the same.

## Round 12: more records in flight
Profiled on 4 cores: the hottest load in the record loop (37% of it) was `lookup[rid]`, a random read into the 2 MB region table that was never prefetched, and the next was the atomic OR into the 4 MB of seen bits (22%), prefetched only one record ahead.
- A parsed record now prefetches its region entry and its name slot and waits one record for its country; it then gets its aggregate and a prefetch of its seen word, and waits four more records for the name insert and the OR. Depth 4 beat 1, 2, 8 and 16 for the inserts; waiting more than one record for the country was slower. A two-queue version was 3-8% slower than an Option for the first wait.
- The BOM test at each record start was a `starts_with`, which compiled to a `memcmp` call (7% of samples); it is three byte compares now.
- Tried and dropped: the three tables on 2 MB pages (-4% time but the peak went 9 -> 13 MB, a loss with M^0.2), and MADV_POPULATE_READ on each thread's part of the file (page faults are 12% of CPU, but it moved -1.7%, inside the noise).
- A record is a one-record `Agg`, merged with the same `merge` the totals use.
Exact on s7, s42, t5, full. Against round 11, 15 alternating runs: 0.149 -> 0.119 s (-20%), CPU 0.49 -> 0.39 s, peak 9 MB both. rustfmt lines 361 -> 390.
Board: run-960c1d720e2e, 0.0985 s (a slow run for every track: unchanged B went 0.250 -> 0.317 s), then run-501ebf874bb0, 0.0607 s, 12.7 MB, 70.06 points. With 12 threads the gain seen on 4 cores does not show; the old code ran 0.058 to 0.068 s.
Reverted in round 13: a third board sample, run-7073cb070c9d at 0.0601 s, matched the old code (0.058 to 0.068 s, median about 0.0595). The board runs two hyperthreads per core, which already overlaps these misses; on 4 cores without SMT the software prefetching was the only overlap there was. Not worth 24 lines of S.

## Round 13: region codes become ranks in place
The region table held codes in one 2 MB array and ranks in a second one, both alive to the end. The ranks now overwrite the codes in place. With 12 threads forced, the cgroup peak goes from 10 to 8 MB. Exact on s7, s42, t5, full.

## Round 14: zeroed tables from calloc
The name table and the seen bits (5 MB) were filled with zeros on the main thread before the parse; they now come from `alloc_zeroed`, so the kernel's zero pages fault in on the threads that use them. 21 alternating runs: -1% at 4 threads, -3% with 12 threads forced. Exact on s7, s42, t5, full; peak 8 MB both.
Reverted: board run-f613fe81dbf3 measured 0.0836 s and 20.7 MB for rounds 13 + 14. Not reproduced locally (8-10 MB, even with THP `always`), but round 14 is the one that changed how the big tables get their pages, so it goes; round 13 stays.
Round 13 alone on the board: run-2c942fe50715, 0.0592 s, 10.9 MB (from 12.8), 71.16 points, a new best, rank 36 -> 33 of 94.

## Round 15: region lookup as a rank query
The region table was 2 MB indexed by region id (ids up to 999,999, 200,000 present), read once per record at random. It is now a bitset of the ids present, with the count of ids before each 64-bit word stored next to the word, and the countries in id order (regions.csv lists ids ascending): a lookup is a bit test plus a popcount, in 0.6 MB that stays in cache. 12-thread peak 8 -> 7 MB; 21 alternating runs: 0.153 -> 0.140 s at 4 threads, 0.156 -> 0.147 s with 12. Exact on s7, s42, t5, full.

## Round 16: read the seen bit before setting it
Every record did an atomic OR on its (country, name) bit. With 12 threads the lines of the popular countries move between cores on every OR, even when the bit is already set, which it is for every repeat of a pair. The bit is now read first and only ORed when clear, so a repeat leaves the line shared. Locally (4 cores, no SMT) this is neutral, 21 alternating runs at 4 and at 12 threads within 1-4%; it is aimed at the board's 6 cores. Exact on s7, s42, t5, full.
