# Speed Demon 2026: PERF summary

All four tracks are in Rust with zero crates. Same pattern every time: mmap the input, split it on a boundary the spec guarantees is safe, do the work on every core without locks, merge once, write once.

Board numbers (seed 1 full data, scoring box), first run vs latest:

| Track | Reference (s) | First board run | Latest board run | Points |
|---|---|---|---|---|
| A Ingest | 32.05 | 0.172 s, 340 MB | 0.065 s, 34 MB (run-f9113349ae56) | 51.06 to 70.86 |
| B Index | 56.35 | 0.798 s, 989 MB | 0.632 s, 364 MB (run-0e405c66c37e) | 46.13 to 52.21 |
| C Join | 14.44 | 0.702 s, 517 MB | 0.098 s, 20 MB (run-d49a0459162a) | 28.41 to 63.09 |
| D Traverse | 72.69 | 1.647 s, 338 MB | 0.274 s, 314 MB (run-e875a2282c02) | 41.03 to 58.06 |

Every change was checked against the published sha256 on seed 1 over 5 runs, against the Python reference on sample data for seeds 7 and 42, on tiny data, and with 1, 3, 5 and 7 threads. Per track steps and reasoning are in each `track-*/PERF.md`.
