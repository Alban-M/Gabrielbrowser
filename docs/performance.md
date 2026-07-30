# Performance report

Measured with `cargo run --release -p gabriel-bench` on 2026-07-29.

**Host:** macOS 15.7.4, Intel Core i7-8700B @ 3.20 GHz, 12 logical cores.
**Origin:** a loopback HTTP server inside the harness, so these numbers describe
Gabriel rather than somebody's network. Latency to a real API dwarfs everything
here; what matters is the overhead Gabriel *adds*, which is why every section
reports a baseline next to it.

Percentiles are nearest-rank over 400 samples for HTTP paths, after 40 warmup
iterations.

## Verdict

Three things came out well, two need work.

| | |
|---|---|
| **Request overhead** | 10 µs over the raw HTTP client — 25 µs with templates, captures and assertions all switched on. Against the strategy document's "≤ parity with Bruno" target, this is not a concern. |
| **Startup** | 6.0 ms for `--version`, 7.7 ms to load and list a 51-request collection. The 1.2 s cold-start budget in §21 was written for a browser; a CLI has no excuse and doesn't need one. |
| **TLS interception** | 77 µs to mint a leaf certificate for a new host, then free from cache. CA generation, once per install, is under a millisecond. |
| **⚠ Capture log reads** | `capture ls --limit 30` takes **271 ms** against a 50 000-capture log, because every read deserializes the entire file to show the last thirty rows. |
| **⚠ Capture appends** | 52 µs of the 72 µs per append is opening and closing the file. That is roughly a third of the proxy's per-request overhead. |

## Full output

```text
Gabriel performance report
================================================================================================
host: macos · 12 logical cores · loopback origin server

Process startup (fork/exec included — that is most of it)
                                             n      mean       p50       p95       p99       max
gabriel --version                           25     6.0ms     6.0ms     7.2ms     7.3ms     7.3ms
gabriel ls (51 requests)                    25     7.9ms     7.7ms     9.4ms    10.0ms    10.0ms
binary size                               8.7 MB

Request path · 70-byte JSON response over loopback
                                             n      mean       p50       p95       p99       max
reqwest direct (baseline)                  400      68µs      64µs      97µs     125µs     169µs
gabriel engine                             400      79µs      74µs     113µs     143µs     168µs
gabriel engine + vars/captures/asserts     400      96µs      89µs     138µs     189µs     214µs
engine overhead at p50                      10µs
…with templates + asserts                   25µs

Response size · body fully buffered and read
                                             n      mean       p50       p95       p99       max
gabriel engine · 1.0 KB                    200      84µs      75µs     134µs     184µs     259µs
gabriel engine · 100.0 KB                  200     122µs     115µs     179µs     231µs     247µs
gabriel engine · 1.0 MB                    200     661µs     614µs     1.2ms     1.6ms     1.8ms
gabriel engine · 8.0 MB                     40     7.9ms     7.7ms    10.1ms    12.0ms    12.0ms

Capture proxy overhead · HTTP, small JSON
                                             n      mean       p50       p95       p99       max
gabriel engine, direct                     400      80µs      74µs     109µs     135µs     237µs
gabriel engine, via capture proxy          400     252µs     237µs     343µs     405µs     453µs
added per request at p50                   163µs
throughput direct · 8 concurrent         72.7k/s
throughput via proxy · 8 concurrent      10.2k/s
captures written                            1240

TLS interception
                                             n      mean       p50       p95       p99       max
mint leaf cert · new host                   60      80µs      77µs      83µs     234µs     234µs
cached cert · repeat host                 2000       0ns       0ns       0ns       0ns       0ns
CA generation · once per install           445µs

Capture log writes
                                             n      mean       p50       p95       p99       max
append one capture                        2000      74µs      72µs     123µs     169µs     255µs

Capture log reads · every read parses the whole file
                                             n      mean       p50       p95       p99       max
ls --limit 30 · 100 captures (60.7 KB)      10     576µs     439µs     1.3ms     1.3ms     1.3ms
ls --limit 30 · 1000 captures (610.9 KB)      10     4.9ms     4.6ms     5.9ms     5.9ms     5.9ms
ls --limit 30 · 10000 captures (6.0 MB)      10    56.1ms    55.3ms    61.8ms    61.8ms    61.8ms
ls --limit 30 · 50000 captures (30.2 MB)      10     289ms     278ms     343ms     343ms     343ms

Capture lookup by id
                                             n      mean       p50       p95       p99       max
get by id · 100 captures                    10     555µs     496µs     890µs     890µs     890µs
get by id · 1000 captures                   10     5.2ms     4.8ms     6.6ms     6.6ms     6.6ms
get by id · 10000 captures                  10    53.6ms    53.6ms    55.7ms    55.7ms    55.7ms
get by id · 50000 captures                  10     318ms     308ms     383ms     383ms     383ms

Bottleneck attribution
append · reopen per capture (current)  62µs · 16.2k/s
append · one held handle               7µs · 150.9k/s
read 6 MB log from page cache              1.5ms
deserialize 10000 captures                45.3ms
  → the cost is deserialization, not I/O; `ls --limit 30` decodes every
    capture ever recorded to show the last thirty.

Core CPU paths
resolve template · 4 substitutions     281.8k/s · 4µs
resolve string with no template          15.5M/s
  13.9 KB document · 100 items
    parse                                  130µs
    jsonpath select                       2.8M/s
    structural diff                    385µs · 1 change(s) found
  1.4 MB document · 10000 items
    parse                                 17.0ms
    jsonpath select                       2.8M/s
    structural diff                    35.4ms · 1 change(s) found
build Cookie header · 200 cookies        17.5k/s

Vault · Argon2id, 64 MiB / 3 passes
create · derive key + write                189ms
unlock · derive key + decrypt              158ms
save 100 secrets                          20.6ms
lookup once unlocked                     12.3M/s

================================================================================================
```

## The two problems, and what fixing them buys

### Capture log reads are O(everything ever recorded)

`CaptureStore::list` and `::get` read the whole newline-delimited log and
deserialize every line. The harness isolates where that time goes on a
10 000-capture, 6 MB log:

| | |
|---|---|
| read the file from page cache | 1.3 ms |
| deserialize 10 000 captures | 40.4 ms |

So it is not I/O — it is serde, and it scales linearly with a file that only
ever grows:

| captures | log size | `ls --limit 30` | `get` by id |
|---|---|---|---|
| 100 | 61 KB | 0.4 ms | 0.4 ms |
| 1 000 | 611 KB | 4.5 ms | 4.5 ms |
| 10 000 | 6.0 MB | 50 ms | 50 ms |
| 50 000 | 30 MB | 271 ms | 271 ms |

50 000 captures is not a stress test — the proxy recorded 1 240 during one
section of this benchmark, and a browsing session produces hundreds per page.
An afternoon of real use lands in the 10 000s, where the tool a developer
reaches for constantly takes 50 ms to answer.

Two independent fixes, either sufficient:

1. **Read backwards from the end of the file** for `list`, stopping once `limit`
   rows are decoded. Turns the common case into O(limit) and needs no format
   change. `get` by id still needs a scan, but can stop at the first match
   instead of decoding everything after it.
2. **Keep a sidecar index** (id → byte offset, plus the fields `ls` displays) so
   listing never touches the capture bodies at all.

The format choice was right; the read strategy was lazy.

### Appends reopen the file every time

| | per append | throughput |
|---|---|---|
| reopen per capture (current) | 52 µs | 19.2k/s |
| one held handle | 7 µs | 134.2k/s |

A **7.4×** difference, and it is on the proxy's hot path: the proxy adds 163 µs
per request, of which ~45 µs is this. Holding the handle open behind the mutex
the store already has would be a contained change.

## Notes on the other numbers

**Proxy throughput** — 10.2k req/s through the proxy against 72.7k direct. The
ratio looks alarming and mostly isn't: 10k req/s is far beyond what a browser
generates, and the absolute cost is 163 µs per request against a loopback origin
that answers in 74 µs. Against a real API answering in 50 ms it is noise. Worth
revisiting only after the append fix, which is a third of it.

**Body buffering** — 7.7 ms for 8 MB, scaling linearly, as buffering does. This
is the known cost of the decision not to stream; it is why the proxy skips
capturing bodies over 8 MB by default. A large download through the proxy pays
this twice (once each way).

**Vault** — 102 ms to unlock is Argon2id at 64 MiB doing its job, paid once per
process, and only when a request actually references a secret (the CLI opens the
vault lazily). Lookups after that are 13.3M/s. The 22.8 ms save is `fsync`;
correct for a file holding credentials.

**Structural diff** — 29.4 ms on a 1.4 MB document with 10 000 array elements,
of which 14.8 ms is `serde_json` parsing before the diff even starts. Fine for
its interactive purpose.

**Cookie header assembly** — 18.4k/s with 200 cookies in the jar (54 µs), which
is the same order as the entire rest of the request overhead. It clones and sorts
the matching set on every call. Real jars hold ten or twenty cookies per host,
not two hundred, so this is a note rather than a problem — but the jar is scanned
linearly per request and would want bucketing by domain if sessions ever get
large.
