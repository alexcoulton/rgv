# Remote range benchmark

This deterministic loopback benchmark has two gates. The range-layer gate places 20 ms of latency
on every request, injects one transient 503 response, reads 600 KiB, then rereads cached data. It
allows three requests and 786,433 successful response bytes. The production alignment gate opens
the generated indexed BAM through the same backend used by the desktop, queries the 151,259-record
deep locus, and repeats it warm. It allows 12 requests, 15 MiB, and 1 second while requiring the
warm query to add no requests.

```sh
cargo run --release -p rgv-remote-range-bench -- \
  --output benchmark-results/remote-range-latest.json
```

The JSON separates the range-only wall time from the total run and records retries, cache hits,
misses, evictions, cancellations, requests, and successful response bytes. The alignment section
records the same counters for the first deep-locus query and proves that an identical warm query
adds no network requests. Remote BAM, CRAM/CRAI, and FASTA/FAI all use the observable bounded
pure-Rust range layer; focused qualification tests cover their format-specific semantics and index
naming matrix.
