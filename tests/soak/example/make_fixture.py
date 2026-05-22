#!/usr/bin/env python3
"""Generate the synthetic example soak fixture used by analyze_soak.py --self-check.

This is NOT a real 24h Jetson run. It is a deterministic, compressed stand-in
(60 ticks at 120s spacing, ~2h of synthetic wall time) that demonstrates the
telemetry schema and lets CI exercise the analyzer without hardware — the span
deliberately reaches past the hour-1 latency window so the percentile path is
covered. A real run is produced by genie-soak.sh on a Jetson; see ../README.md.

Run from this directory: `python3 make_fixture.py`
"""

import json
import math
import os

HERE = os.path.dirname(os.path.abspath(__file__))
TICKS = 60
INTERVAL_S = 120
BASE_RSS = 1_600_000_000  # ~1.49 GiB, plateaus (small bounded jitter, no creep)


def main() -> None:
    records = []
    for tick in range(1, TICKS + 1):
        elapsed = tick * INTERVAL_S
        # Bounded oscillation around a flat baseline — a plateau, not a creep.
        jitter = int(8_000_000 * math.sin(tick / 4.0))
        first_token = round(420.0 + 60.0 * math.sin(tick / 3.0), 1)
        total = round(1700.0 + 250.0 * math.sin(tick / 5.0), 1)
        records.append(
            {
                "ts": round(1_716_200_000.0 + elapsed, 3),
                "iso": "2026-05-20T12:00:00Z",
                "elapsed_s": float(elapsed),
                "tick": tick,
                "conversation_id": f"example-soak-{tick}",
                "request": {
                    "ok": True,
                    "status": 200,
                    "first_token_ms": first_token,
                    "total_ms": total,
                    "tokens": 40 + (tick % 20),
                    "error": None,
                },
                "health": {
                    "ok": True,
                    "http_status": 200,
                    "status": "ok",
                    "llm": "connected",
                    "llm_backend": "genie-ai-runtime",
                    "mem_available_mb": 1850 - (tick % 7),
                    "error": None,
                },
                "runtime_rss_bytes": BASE_RSS + jitter,
            }
        )

    with open(os.path.join(HERE, "telemetry.jsonl"), "w", encoding="utf-8") as handle:
        for record in records:
            handle.write(json.dumps(record) + "\n")

    journal = [
        "2026-05-20T12:00:01Z geniepod systemd[1]: Started GeniePod AI Runtime.",
        "2026-05-20T12:00:02Z geniepod jetson-llm-server[812]: model loaded, ctx=4096",
        "2026-05-20T12:15:00Z geniepod jetson-llm-server[812]: served 30 requests, kv ok",
        "2026-05-20T12:30:00Z geniepod jetson-llm-server[812]: served 60 requests, kv ok",
    ]
    with open(os.path.join(HERE, "journal.txt"), "w", encoding="utf-8") as handle:
        handle.write("\n".join(journal) + "\n")

    print(f"wrote {len(records)} telemetry records and a clean journal to {HERE}")


if __name__ == "__main__":
    main()
