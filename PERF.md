# Speed Demon 2026: PERF summary

All four tracks are in Rust with zero crates. Same pattern every time: map or pread the input, split it on a boundary the spec guarantees is safe, do the work on every core without locks, merge once, write once.

Board numbers (seed 1 full data, scoring box), first run vs latest:

| Track | Reference (s) | First board run | Latest board run | Points |
|---|---|---|---|---|
| A Ingest | 32.05 | 0.172 s, 340 MB | 0.061 s, 39 MB (run-f70f5f3b9d4b) | 51.06 to 70.96 |
| B Index | 56.35 | 0.798 s, 989 MB | 0.365 s, 229 MB (run-3672a75d2a36) | 46.13 to 58.03 |
| C Join | 14.44 | 0.702 s, 517 MB | 0.090 s, 20 MB (run-5e77f9bfa29a) | 28.41 to 63.96 |
| D Traverse | 72.69 | 1.647 s, 338 MB | 0.304 s, 177 MB (run-9ecfb196b1c8) | 41.03 to 59.12 |

Every change was checked against the published sha256 on seed 1 over 5 runs, against the Python reference on sample data for seeds 7 and 42, on tiny data, and with 1, 3, 5 and 7 threads. Per track steps and reasoning are in each `track-*/PERF.md`.

## Formatting
All four sources are now in rustfmt's default style, the same formatting the board applies before it counts S, so the code reads the way it is measured and S does not change. No code changed: every track is still exact on s7, s42, t5, full, h99 and h3.
