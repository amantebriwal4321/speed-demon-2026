# Speed Demon 2026 entry

Solo entry for [Speed Demon 2026](https://speeddemon.dev), all four tracks, Rust with no external crates. Started from the [starter template](https://github.com/hackathon-raptors/speed-demon-starter).

Each `track-*` folder builds offline with its own Dockerfile and reads `/data`, writes one file to `/out`. For a local run without Docker:

```
cd track-a && cargo build --release
DATA=path/to/data OUT=/tmp/out ./target/release/solution
```

See `PERF.md` for timings and the why behind them (each track's own `PERF.md` has every round, kept or not), and `RUNS.md` for the board's runs one by one, which shows how much the same code varies from run to run. MIT licensed.
