# Speed Demon 2026: PERF summary

All four tracks are in Rust with zero crates. Same pattern every time: map the input, split it on a boundary the spec guarantees is safe, do the work on every core without locks, merge once, write once.

Board numbers (seed 1 full data, scoring box), first run vs best run of the current code:

| Track | Reference (s) | First board run | Best board run | Points |
|---|---|---|---|---|
| A Ingest | 32.05 | 0.172 s, 340 MB | 0.0257 s, 20.6 MB (run-bb9921cb6cae) | 51.06 to 80.79 |
| B Index | 56.35 | 0.798 s, 989 MB | 0.2127 s, 66 MB (run-8f37cd767722) | 46.13 to 67.84 |
| C Join | 14.44 | 0.702 s, 517 MB | 0.0375 s, 9 MB (run-d73199192b45) | 28.41 to 76.52 |
| D Traverse | 72.69 | 1.647 s, 338 MB | 0.210 s, 87 MB (run-4b0234285e75) | 41.03 to 65.00 |

What mattered most late on was the operating system, not the inner loops. The scoring box runs two threads per core, so a change that only moves cache misses around often measured faster on a 4-core test box and slower there; what held up was work that stops being done at all. Three of those:
- tearing down a 200+ MB file mapping at exit took milliseconds on one thread after everything else was done; each thread now drops its own share's page tables with MADV_DONTNEED when it is finished with them (C -16%, A -10% on the board);
- many small mmap/munmap calls (A's row blocks, D's growing parse buckets) each took the address-space lock and interrupted every core; A's rows now live in one reserved mapping freed an hour at a time;
- glibc is linked statically, so a run starts without the dynamic loader.
The board's kernel backs anonymous memory with 2 MB pages, which changes both memory and TLB behaviour, so local timing runs set transparent huge pages to `always` as well.

The last night went to different designs rather than tuning, judged on the board against runs of the old code in the same hour. None held up there, and all are out again, each written up in its track's PERF.md:
- B's scan with one 1 MB table of 64-bit fingerprints instead of 4 MB of exact keys (a standalone benchmark showed the lookup's cost is the table's size): -11% scan on one local core, about 6% slower on the board (round 46);
- B taking the docs a part at a time with every thread on each part, so only one part's postings are ever held: peak 56 -> 31 MB locally and time flat, but on the board 66 -> 56 MB and 11% slower (round 49);
- B's phrase check starting where a recorded anchor first shows up: -6.5% locally, but 5 MB and 58 lines more, about +0.2 points on paper, so not tried on the board (round 48);
- A's parse in pieces handed out as threads come free (what helped C): -7% with 12 threads forced locally, no difference on the board (round 35).
All of them were exact on the checks below; what decided was the board, so the code that stays is the code with the most board evidence behind it.

What did hold up on the morning of the last day was finding threads with nothing to do, in B, the weakest of the three tracks that count. Timing each piece of its scan showed the token walk costing more than reading the text, and the fill running in two waves of half the parts, so half the threads sat idle through it:
- each 64-byte window now gives all its token starts and ends at once from two bit masks (round 51);
- each part is filled by two threads, the second filling its half of every list backward from the list's end, so neither needs the other's counts and the waves keep their memory (rounds 53-54);
- less is done on one thread before the scan starts (rounds 55-56).
B went from 0.222-0.230 s on a quiet box to 0.213-0.218 s at the same 66 MB, 67.46 to 67.84 points. In C the same look at where the time goes led to a smaller region lookup (0.65 -> 0.4 MB, round 34): about 3% faster, 76.38 to 76.52.

Every change was checked against the published sha256 on seed 1 over 5 runs, against the Python reference on sample data for seeds 7 and 42, on tiny data, and with 1, 3, 5 and 7 threads; the final versions of all four also match the reference on a full-scale data set generated with a fresh seed (77). Per track steps and reasoning are in each `track-*/PERF.md`.

## Formatting
All four sources are now in rustfmt's default style, the same formatting the board applies before it counts S, so the code reads the way it is measured and S does not change. No code changed: every track is still exact on s7, s42, t5, full, h99 and h3.
