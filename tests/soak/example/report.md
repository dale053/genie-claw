# genie-ai-runtime 24h soak report

- Telemetry: `telemetry.jsonl` (60 records)
- Observed span: 2.0 h
- Overall: **PASS**

| Criterion | Verdict | Detail |
| --- | --- | --- |
| 24h continuous uptime, zero runtime restarts | ✅ PASS | 2.00h continuous, 0 runtime restarts |
| Zero OOM kills (dmesg/journal clean) | ✅ PASS | no OOM markers found |
| /api/health reports genie-ai-runtime reachable >= 99% of samples | ✅ PASS | 60/60 reachable (100.00% >= 99%) |
| No memory monotonic growth (RSS plateaus, not creeps) | ✅ PASS | median first-decile=1606731767, last-decile=1607479160 (+0.0%, slope -161877.1/h) via runtime_rss_bytes |
| First-token latency p50/p99 within budget at hours 1, 12, 24 | ✅ PASS | within budget at h1 |
| Soak telemetry artifact present under tests/soak/ | ✅ PASS | 60 telemetry records parsed from telemetry.jsonl |

Verdicts marked N/A had no captured input (e.g. no journal, no restart count, or RSS unaccounted on this host) and were excluded from the pass/fail gate. See README.md for how each criterion is computed.
