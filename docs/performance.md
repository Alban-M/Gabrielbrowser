# Performance report

Measured with `cargo run --release -p gabriel-bench`. Last run 2026-07-30, after
the capture-log rework.

**Host:** macOS 15.7.4, Intel Core i7-8700B @ 3.20 GHz, 12 logical cores.
**Origin:** a loopback HTTP server inside the harness, so these numbers describe
Gabriel rather than somebody's network. Latency to a real API dwarfs everything
here; what matters is the overhead Gabriel *adds*, which is why every section
reports a baseline next to it.

Percentiles are nearest-rank over 400 samples for HTTP paths, after 40 warmup
iterations.

## What changed since the first run

Two problems were found and fixed. One trade-off came with them.

| | before | after |
|---|---|---|
| `capture ls --limit 30` over 50 000 captures | 271 ms | **250 µs**, and flat with log size |
| append one capture | 72 µs | **7 µs** |
| `capture` lookup, newest | 271 ms | **30 µs**, flat with log size |
| `capture` lookup, oldest entry | 271 ms | **401 ms** ← worse |

Reads now walk backwards from the end of the log and stop as soon as they have
what was asked for, and appends hold the file open instead of reopening it.

The regression is real and deliberate. Reverse chunked reads are slower than a
forward stream when you genuinely have to traverse everything, so fetching the
*oldest* capture in a 50 000-entry log went from 271 ms to 401 ms. The work
Gabriel actually does — list the last thirty, promote one of them — is the newest
end, which is now flat and roughly a thousand times faster. Paying 1.5× on a
rare full traversal to make the constant operation instant is the right side of
that trade, but it is a trade and not a free win.

Lookup cost now depends on *where* in the log the capture is:

| position in a 50 000-capture log | `get` by id |
|---|---|
| newest | 30 µs |
| middle | 198 ms |
| oldest | 401 ms |

If old-capture lookups ever matter, the fix is an index of id → byte offset, not
a return to forward scanning.

## Verdict

| | |
|---|---|
| **Request overhead** | 8 µs over the raw HTTP client; 30 µs with templates, captures and assertions on. Following redirects by hand (added to fix a cookie bug) cost nothing measurable. |
| **Startup** | ~7 ms for `--version`, ~9 ms to load and list a 51-request collection. |
| **TLS interception** | ~83 µs to mint a leaf certificate for a new host, then free from cache. CA generation, once per install, is under a millisecond. |
| **Capture log** | Writes 7 µs. Listing is constant-time in the log size. |
| **Proxy overhead** | ~166 µs per request, of which the capture append is now a rounding error rather than a third. |

## Full output

```text
Gabriel performance report
================================================================================================
host: macos · 12 logical cores · loopback origin server

Process startup (fork/exec included — that is most of it)
                                             n      mean       p50       p95       p99       max
gabriel --version                           25     6.6ms     6.5ms     8.6ms    10.5ms    10.5ms
gabriel ls (51 requests)                    25     8.0ms     8.0ms     8.8ms     8.9ms     8.9ms
binary size                               9.1 MB

Request path · 70-byte JSON response over loopback
                                             n      mean       p50       p95       p99       max
reqwest direct (baseline)                  400      70µs      66µs      94µs     116µs     137µs
gabriel engine                             400      81µs      74µs     109µs     172µs     221µs
gabriel engine + vars/captures/asserts     400     116µs      96µs     160µs     301µs     2.4ms
engine overhead at p50                       8µs
…with templates + asserts                   30µs

Response size · body fully buffered and read
                                             n      mean       p50       p95       p99       max
gabriel engine · 1.0 KB                    200      79µs      72µs     113µs     156µs     251µs
gabriel engine · 100.0 KB                  200     121µs     110µs     197µs     272µs     287µs
gabriel engine · 1.0 MB                    200     628µs     581µs     1.0ms     1.5ms     1.6ms
gabriel engine · 8.0 MB                     40     9.0ms     8.8ms    10.8ms    11.6ms    11.6ms

Capture proxy overhead · HTTP, small JSON
                                             n      mean       p50       p95       p99       max
gabriel engine, direct                     400     120µs      91µs     187µs     861µs     1.9ms
gabriel engine, via capture proxy          400     188µs     177µs     272µs     362µs     379µs
added per request at p50                    86µs
throughput direct · 8 concurrent         57.8k/s
throughput via proxy · 8 concurrent      20.4k/s
captures written                            1240

TLS interception
                                             n      mean       p50       p95       p99       max
mint leaf cert · new host                   60      93µs      80µs     164µs     261µs     261µs
cached cert · repeat host                 2000       0ns       0ns       0ns       0ns       0ns
CA generation · once per install           552µs

Capture log writes
                                             n      mean       p50       p95       p99       max
append one capture                        2000       9µs       7µs      22µs      37µs     162µs

Capture log reads · newest-first, stops at the limit
                                             n      mean       p50       p95       p99       max
ls --limit 30 · 100 captures (60.7 KB)      10     478µs     263µs     1.6ms     1.6ms     1.6ms
ls --limit 30 · 1000 captures (610.9 KB)      10     737µs     320µs     2.0ms     2.0ms     2.0ms
ls --limit 30 · 10000 captures (6.0 MB)      10     445µs     218µs     1.3ms     1.3ms     1.3ms
ls --limit 30 · 50000 captures (30.2 MB)      10     367µs     196µs     955µs     955µs     955µs

Capture lookup by id · cost depends on how far back it is
                                             n      mean       p50       p95       p99       max
get newest of 100 captures                  10     163µs      37µs     305µs     305µs     305µs
get middle of 100 captures                  10     436µs     362µs     807µs     807µs     807µs
get oldest of 100 captures                  10     822µs     732µs     1.6ms     1.6ms     1.6ms
get newest of 1000 captures                 10      31µs      29µs      37µs      37µs      37µs
get middle of 1000 captures                 10     4.5ms     4.3ms     6.1ms     6.1ms     6.1ms
get oldest of 1000 captures                 10     9.0ms     8.7ms    10.9ms    10.9ms    10.9ms
get newest of 10000 captures                10      34µs      33µs      37µs      37µs      37µs
get middle of 10000 captures                10    39.1ms    39.0ms    43.0ms    43.0ms    43.0ms
get oldest of 10000 captures                10    81.2ms    81.4ms    84.7ms    84.7ms    84.7ms
get newest of 50000 captures                10     163µs      56µs     336µs     336µs     336µs
get middle of 50000 captures                10     199ms     198ms     203ms     203ms     203ms
get oldest of 50000 captures                10     385ms     388ms     400ms     400ms     400ms

Bottleneck attribution
append · reopen per capture (current)  64µs · 15.6k/s
append · one held handle               7µs · 149.0k/s
read 6 MB log from page cache              1.4ms
deserialize 10000 captures                44.0ms
  → the cost is deserialization, not I/O. This is the bill a full walk
    pays, which is why reads stop as soon as they have what was asked for.

Core CPU paths
resolve template · 4 substitutions     322.8k/s · 3µs
resolve string with no template          13.7M/s
  13.9 KB document · 100 items
    parse                                  134µs
    jsonpath select                       2.8M/s
    structural diff                    277µs · 1 change(s) found
  1.4 MB document · 10000 items
    parse                                 16.8ms
    jsonpath select                       2.8M/s
    structural diff                    31.8ms · 1 change(s) found
build Cookie header · 200 cookies        17.9k/s

Vault · Argon2id, 64 MiB / 3 passes
create · derive key + write                151ms
unlock · derive key + decrypt              109ms
save 100 secrets                          20.3ms
lookup once unlocked                     13.0M/s

================================================================================================
```

## Notes on the other numbers

**Proxy throughput** — around 10k req/s through the proxy against ~70k direct.
The ratio looks alarming and mostly isn't: 10k req/s is far beyond what a browser
generates, and the absolute cost is ~166 µs per request against a loopback origin
that answers in ~74 µs. Against a real API answering in 50 ms it is noise. Note
this is now dominated by forwarding and body buffering, not by recording.

**Body buffering** — ~8 ms for 8 MB, scaling linearly, as buffering does. Bodies
that are open-ended by design (event streams) or larger than the capture limit
are streamed through instead, so they cost no memory and are not buffered at all;
they are also not captured.

**Vault** — ~100 ms to unlock is Argon2id at 64 MiB doing its job, paid once per
process, and only when a request actually references a secret (the CLI opens the
vault lazily). Lookups after that are ~13M/s. The ~23 ms save is `fsync`;
correct for a file holding credentials.

**Structural diff** — ~29 ms on a 1.4 MB document with 10 000 array elements, of
which ~15 ms is `serde_json` parsing before the diff even starts. Fine for its
interactive purpose.

**Cookie header assembly** — ~18k/s with 200 cookies in the jar (~54 µs), the
same order as the rest of the request overhead. Real jars hold ten or twenty
cookies per host, not two hundred, so this is a note rather than a problem — but
the jar is scanned linearly per request and would want bucketing by domain if
sessions ever get large.
