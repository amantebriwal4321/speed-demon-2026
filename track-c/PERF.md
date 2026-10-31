# Track C: performance log

Local numbers are from an 8 thread Linux box, seed 1 full data, best of 5 runs, sha256 matching the published `d8088b1c...`.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 14.44 | 367 | 0 |
| 1 | Rust, hand written CSV state machine, file split on CRLF, per thread aggregates | run-67fbf32e31ef | 0.702 | 517 | 28.41 |

## Why it got faster

- The spec promises a CR only ever shows up as the first byte of a CRLF terminator, never inside quotes. So any position right after `\r\n` is a safe record boundary, and the file can be split for threads without a sequential quote scan.
- The parser is a tiny state machine over bytes. Quoted fields are scanned for the next `"`, and only fields that really contain `""` get an owned unescaped copy. Everything else is a borrowed slice into the file buffer.
- A BOM is checked only at a record start, which is the only place it's legal.
- The region lookup is a flat array indexed by region id, so the join is one array read.
- Distinct names go into a `HashSet<(country, name)>` per thread with a small Fx style hasher, then the sets get unioned. Money stays integer cents the whole way.

## Next ideas

Most of the remaining time is the distinct name sets. Interning names to ids first would make the merge a lot cheaper.
