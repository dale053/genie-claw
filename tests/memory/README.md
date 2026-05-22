# Memory recall test set

Curated cases that prove household memory recall is reliable and that recall
failures are observable, not silent. Tracks M1 exit criterion from
[issue #111](https://github.com/GeniePod/genie-claw/issues/111).

## Layout

- `cases.toml` — the case definitions. One `[[case]]` table per scenario.
- `expected/ledger.json` — **golden fixture**. The committed file is the
  expected ledger; the runner serializes the in-memory ledger and
  compares it against this file (with CRLF normalised). The runner does
  **not** write into the tracked tree.

The runner lives at `crates/genie-core/tests/memory_recall.rs` and is exercised
by `cargo test -p genie-core --test memory_recall`. Its only filesystem
output is `target/memory-recall-ledger.json`, in Cargo's gitignored build
directory.

## Regenerating the golden ledger

When you intentionally change anything that affects the ledger output —
adding a case, editing a description, changing an `expect.outcome` —
the runner fails with a drift error pointing at the freshly generated
ledger under `target/`. To accept the new ledger:

```sh
cargo test -p genie-core --test memory_recall   # fails with drift hint
cp target/memory-recall-ledger.json tests/memory/expected/ledger.json
cargo test -p genie-core --test memory_recall   # now green
git add tests/memory/expected/ledger.json
```

The two-step ensures every fixture change appears as a reviewable diff
in the PR.

## What a case proves

Each case has a `seed`, a `query`, a `context`, and an `expect`:

| `expect.outcome` | Meaning |
| --- | --- |
| `hit`      | Recall returned at least one entry. If `contains` is set, one entry must contain that substring. |
| `filtered` | The underlying search **did** match rows, but every match was dropped by `assess_memory_read` (scope / sensitivity / spoken_policy). The user gets an empty context, but the miss is logged with `cause="policy_filtered"`. |
| `miss`     | The underlying search returned nothing. The miss is logged with `cause="no_match"`. |

`restart = true` drops and reopens `Memory` against the same SQLite file
between seed and query. That proves the recall path survives a process
restart — the M1 "next session" requirement — without spinning a binary.

## Acceptance gate

The runner asserts ≥ 95 % pass across all cases and then compares the
in-memory ledger against `expected/ledger.json`. The first assertion
catches recall regressions; the second catches subtler ledger drift
(e.g. a case that moved from `hit` to `filtered` while keeping the
overall pass rate above the floor).
