# Speed Demon 2026: PERF summary

All four tracks are in Rust with zero crates. Same pattern every time: read the whole input once, split it on a boundary that the spec guarantees is safe, do the work on every core without locks, merge once, write once.

Local timings, 8 thread Linux box, seed 1 full datasets, best of 5 runs. Every run matched the published sha256. The solutions also match the Python reference byte for byte on sample data for seeds 7 and 42.

| Track | Reference (s) | Local best (s) | Local median (s) | Peak RSS |
|---|---|---|---|---|
| A Ingest | 32.05 | 0.23 | 0.25 | ~300 MB |
| B Index | 56.35 | 0.81 | 0.88 | ~920 MB |
| C Join | 14.44 | 0.93 | 0.96 | ~480 MB |
| D Traverse | 72.69 | 1.32 | 1.40 | ~260 MB |

The reference times come from the organisers' scoring box and mine come from a different machine, so treat the ratio as rough until the board runs land. Per track details and reasoning are in each `track-*/PERF.md`.
