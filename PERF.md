# Speed Demon 2026: PERF summary

All four tracks are in Rust with zero crates. Same pattern every time: map the input, split it on a boundary the spec guarantees is safe, do the work on every core without locks, merge once, write once.

Board numbers (seed 1 full data, scoring box), first run vs best run of the current code:

| Track | Reference (s) | First board run | Best board run | Points |
|---|---|---|---|---|
| A Ingest | 32.05 | 0.172 s, 340 MB | 0.0255 s, 22 MB (run-0c2cc394e96e) | 51.06 to 80.61 |
| B Index | 56.35 | 0.798 s, 989 MB | 0.220 s, 67 MB (run-a46587694315) | 46.13 to 67.42 |
| C Join | 14.44 | 0.702 s, 517 MB | 0.0385 s, 9 MB (run-7e61367059c9) | 28.41 to 76.26 |
| D Traverse | 72.69 | 1.647 s, 338 MB | 0.210 s, 87 MB (run-4b0234285e75) | 41.03 to 65.00 |

What mattered most late on was the operating system, not the inner loops. The scoring box runs two threads per core, so a change that only moves cache misses around often measured faster on a 4-core test box and slower there; what held up was work that stops being done at all. Three of those:
- tearing down a 200+ MB file mapping at exit took milliseconds on one thread after everything else was done; each thread now drops its own share's page tables with MADV_DONTNEED when it is finished with them (C -16%, A -10% on the board);
- many small mmap/munmap calls (A's row blocks, D's growing parse buckets) each took the address-space lock and interrupted every core; A's rows now live in one reserved mapping freed an hour at a time;
- glibc is linked statically, so a run starts without the dynamic loader.
The board's kernel backs anonymous memory with 2 MB pages, which changes both memory and TLB behaviour, so local timing runs set transparent huge pages to `always` as well.

Every change was checked against the published sha256 on seed 1 over 5 runs, against the Python reference on sample data for seeds 7 and 42, on tiny data, and with 1, 3, 5 and 7 threads. Per track steps and reasoning are in each `track-*/PERF.md`.

## Formatting
All four sources are now in rustfmt's default style, the same formatting the board applies before it counts S, so the code reads the way it is measured and S does not change. No code changed: every track is still exact on s7, s42, t5, full, h99 and h3.
