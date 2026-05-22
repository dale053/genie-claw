// Memory recall test set — M1 exit criterion (issue #111).
//
// Loads `tests/memory/cases.toml` from the workspace root, exercises each
// case against `genie_core::memory::Memory` + `recall::recall_with_context`,
// and asserts the test set passes >= 95%.
//
// The committed `tests/memory/expected/ledger.json` is a **golden fixture**.
// The runner serializes the in-memory ledger and compares it against the
// fixture; a drift means either a real regression or an intentional change
// that needs the fixture regenerated. The runner never writes into the
// tracked tree — the generated ledger goes to `target/memory-recall-ledger.json`
// (Cargo's standard build directory) so a contributor can `cp` it over the
// fixture when the drift is intentional.
//
// Two narrower tests at the bottom of this file pin the structured-log
// behaviour: a no_match recall and a policy_filtered recall must each emit
// a tracing event on target `memory.recall.miss` with the right `cause`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use genie_core::memory::{
    Memory,
    policy::{IdentityConfidence, MemoryReadContext},
    recall::recall_with_context,
};
use serde::{Deserialize, Serialize};
use tracing::field::{Field, Visit};
use tracing::{Level, Subscriber};
use tracing_subscriber::layer::{Context as LayerContext, Layer, SubscriberExt};
use tracing_subscriber::registry::Registry;

const PASS_RATE_FLOOR: f64 = 0.95;
const RECALL_LIMIT: usize = 8;

// ---------------------------------------------------------------------------
// TOML schema.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CaseFile {
    case: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct Case {
    id: String,
    description: String,
    #[serde(default)]
    restart: bool,
    #[serde(default)]
    seed: Vec<SeedEntry>,
    query: String,
    context: ContextSpec,
    expect: ExpectSpec,
}

#[derive(Debug, Deserialize)]
struct SeedEntry {
    kind: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ContextSpec {
    identity_confidence: String,
    explicit_named_person: bool,
    explicit_private_intent: bool,
    shared_space_voice: bool,
}

#[derive(Debug, Deserialize)]
struct ExpectSpec {
    outcome: String,
    #[serde(default)]
    contains: Option<String>,
}

impl ContextSpec {
    fn to_read_context(&self) -> MemoryReadContext {
        let identity_confidence = match self.identity_confidence.to_ascii_lowercase().as_str() {
            "high" => IdentityConfidence::High,
            "medium" => IdentityConfidence::Medium,
            "low" => IdentityConfidence::Low,
            _ => IdentityConfidence::Unknown,
        };
        MemoryReadContext {
            identity_confidence,
            explicit_named_person: self.explicit_named_person,
            explicit_private_intent: self.explicit_private_intent,
            shared_space_voice: self.shared_space_voice,
        }
    }
}

// ---------------------------------------------------------------------------
// Ledger schema (what we write under tests/memory/expected/).
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct LedgerEntry {
    id: String,
    description: String,
    expected_outcome: String,
    actual_outcome: String,
    raw_hits: usize,
    recalled_hits: usize,
    pass: bool,
    note: String,
}

#[derive(Debug, Serialize)]
struct Ledger {
    total: usize,
    passed: usize,
    pass_rate: f64,
    floor: f64,
    entries: Vec<LedgerEntry>,
}

// ---------------------------------------------------------------------------
// Main test: run every case and write the ledger.
// ---------------------------------------------------------------------------

#[test]
fn memory_recall_test_set_meets_95_percent_floor() {
    let cases_path = workspace_root()
        .join("tests")
        .join("memory")
        .join("cases.toml");
    let raw = std::fs::read_to_string(&cases_path)
        .unwrap_or_else(|e| panic!("read {}: {}", cases_path.display(), e));
    let file: CaseFile = toml::from_str(&raw).expect("parse cases.toml");
    assert!(
        file.case.len() >= 20,
        "memory recall test set must have at least 20 cases (have {})",
        file.case.len()
    );

    let mut entries = Vec::with_capacity(file.case.len());
    let mut passed = 0usize;

    for case in &file.case {
        let entry = run_case(case);
        if entry.pass {
            passed += 1;
        }
        entries.push(entry);
    }

    let total = file.case.len();
    let pass_rate = passed as f64 / total as f64;
    let ledger = Ledger {
        total,
        passed,
        pass_rate,
        floor: PASS_RATE_FLOOR,
        entries,
    };

    // Write the generated ledger to Cargo's ignored build directory so a
    // contributor can inspect it without re-running and `cp` it onto the
    // golden fixture when a drift is intentional. This is the only place
    // the runner writes — never into the tracked tree.
    let actual_path = write_debug_ledger(&ledger);
    let actual_text = serde_json::to_string_pretty(&ledger).expect("serialize ledger");

    // Echo failures so the test output points at the right case without
    // requiring a separate cargo invocation.
    let failures: Vec<&LedgerEntry> = ledger.entries.iter().filter(|e| !e.pass).collect();
    if !failures.is_empty() {
        eprintln!("memory recall failures ({}):", failures.len());
        for f in &failures {
            eprintln!(
                "  - {}: expected {}, got {} ({})",
                f.id, f.expected_outcome, f.actual_outcome, f.note
            );
        }
    }

    assert!(
        pass_rate >= PASS_RATE_FLOOR,
        "memory recall pass rate {:.1}% is below the {:.0}% M1 exit floor (passed {}/{})",
        pass_rate * 100.0,
        PASS_RATE_FLOOR * 100.0,
        passed,
        total
    );

    // Integrity check: the in-memory ledger must match the committed
    // golden fixture verbatim. A drift here means either a recall
    // regression that snuck past the pass-rate floor (e.g. a case moved
    // from `hit` -> `filtered`, raw_hits changed), or an intentional
    // change (new case added, description edited) that needs the fixture
    // regenerated.
    let golden_path = workspace_root()
        .join("tests")
        .join("memory")
        .join("expected")
        .join("ledger.json");
    let golden_text = std::fs::read_to_string(&golden_path).unwrap_or_else(|e| {
        panic!(
            "golden fixture {} missing or unreadable ({}). The committed file is the expected ledger; regenerate by copying {} on top of it.",
            golden_path.display(),
            e,
            actual_path.display(),
        )
    });

    if normalize_ledger(&actual_text) != normalize_ledger(&golden_text) {
        panic!(
            "memory recall ledger drifted from {}.\n\
             Generated ledger written to {}.\n\
             If the drift is intentional (e.g. you added a case, edited a description, or changed expected outcomes), regenerate the fixture:\n\
             \n    cp {} {}\n\
             \nand commit the result.",
            golden_path.display(),
            actual_path.display(),
            actual_path.display(),
            golden_path.display(),
        );
    }
}

fn normalize_ledger(text: &str) -> String {
    // Tolerate CRLF in case a Windows checkout normalized line endings,
    // and strip trailing whitespace so an editor-added final newline
    // does not register as a drift.
    text.replace("\r\n", "\n").trim_end().to_string()
}

fn run_case(case: &Case) -> LedgerEntry {
    let db_dir = tempdir(&format!("genie-recall-{}", case.id));
    let db_path = db_dir.join("memory.db");

    {
        let memory = Memory::open(&db_path).expect("open memory");
        for seed in &case.seed {
            memory.store(&seed.kind, &seed.content).expect("seed store");
        }
    } // drop seed-side handle so WAL is flushed before reopen.

    let read_context = case.context.to_read_context();

    let (raw_hits, recalled_hits, has_contains): (usize, usize, bool) = {
        let memory = if case.restart {
            // Reopen the same SQLite file. Proves recall survives a
            // process restart at the storage layer (the M1 "next session"
            // path) without spinning a full binary.
            Memory::open(&db_path).expect("reopen memory")
        } else {
            Memory::open(&db_path).expect("open memory")
        };

        // Capture raw FTS hits separately from policy-filtered recall so
        // we can tell `miss` from `filtered` in the ledger. Both Memory
        // calls touch recall_count, which is fine for these throwaway DBs.
        let raw = memory
            .search(&case.query, RECALL_LIMIT)
            .expect("raw search");
        let raw_hits = raw.len();
        let recalled = recall_with_context(&memory, &case.query, RECALL_LIMIT, read_context)
            .expect("recall_with_context");
        let has_contains = match case.expect.contains.as_deref() {
            Some(needle) => recalled.iter().any(|r| r.entry.content.contains(needle)),
            None => true,
        };
        (raw_hits, recalled.len(), has_contains)
    };

    let actual_outcome = match (raw_hits, recalled_hits) {
        (0, 0) => "miss",
        (_, 0) => "filtered",
        _ => "hit",
    };

    let pass = actual_outcome == case.expect.outcome && has_contains;
    let note = if !pass {
        if actual_outcome != case.expect.outcome {
            format!(
                "outcome mismatch: raw={}, recalled={}",
                raw_hits, recalled_hits
            )
        } else {
            format!(
                "missing expected substring `{}`",
                case.expect.contains.as_deref().unwrap_or("")
            )
        }
    } else {
        case.description.clone()
    };

    LedgerEntry {
        id: case.id.clone(),
        description: case.description.clone(),
        expected_outcome: case.expect.outcome.clone(),
        actual_outcome: actual_outcome.to_string(),
        raw_hits,
        recalled_hits,
        pass,
        note,
    }
}

/// Write the generated ledger to Cargo's `target/` directory for local
/// inspection. `target/` is the standard Cargo build output and is
/// gitignored, so this never mutates the tracked tree. Returns the path
/// so the main test can include it in a drift error message.
fn write_debug_ledger(ledger: &Ledger) -> PathBuf {
    let path = workspace_root()
        .join("target")
        .join("memory-recall-ledger.json");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(ledger) {
        Ok(text) => {
            if let Err(e) = std::fs::write(&path, text) {
                eprintln!("debug ledger write {}: {}", path.display(), e);
            }
        }
        Err(e) => eprintln!("debug ledger serialize: {e}"),
    }
    path
}

// ---------------------------------------------------------------------------
// Narrow tests for AC #3: "Recall miss emits a labeled log line
// distinguishable from a hit." Capture tracing events in-process and
// assert the right target+fields fire for each cause.
// ---------------------------------------------------------------------------

#[test]
fn recall_no_match_emits_labeled_miss_log() {
    let captured = capture_miss_events(|memory| {
        let _ = recall_with_context(
            memory,
            "submarine maintenance schedule",
            RECALL_LIMIT,
            MemoryReadContext::shared_room_voice(),
        );
    });

    let no_match = captured
        .iter()
        .find(|e| e.cause.as_deref() == Some("no_match"));
    assert!(
        no_match.is_some(),
        "expected a memory.recall.miss event with cause=no_match, got {:?}",
        captured
    );
    let event = no_match.unwrap();
    assert_eq!(event.raw_hits, Some(0));
    assert_eq!(event.level, Level::WARN);
}

#[test]
fn recall_policy_filtered_emits_labeled_miss_log() {
    let captured = capture_miss_events(|memory| {
        memory
            .store("fact", "user's password is swordfish")
            .expect("store restricted");
        let _ = recall_with_context(
            memory,
            "password",
            RECALL_LIMIT,
            MemoryReadContext::shared_room_voice(),
        );
    });

    let filtered = captured
        .iter()
        .find(|e| e.cause.as_deref() == Some("policy_filtered"));
    assert!(
        filtered.is_some(),
        "expected a memory.recall.miss event with cause=policy_filtered, got {:?}",
        captured
    );
    let event = filtered.unwrap();
    assert!(
        event.raw_hits.unwrap_or(0) >= 1,
        "policy_filtered miss should carry raw_hits >= 1, got {:?}",
        event.raw_hits
    );
    assert_eq!(event.level, Level::WARN);
}

// ---------------------------------------------------------------------------
// AC #4: promoted-durable filtering. The MEMORY.md root file must only
// surface shared-safe entries; person / private / restricted promotions
// must redact in the namespace projection and stay out of the root file.
// This complements the in-crate tests by exercising the projection end to
// end through the public Memory API from an integration crate boundary.
// ---------------------------------------------------------------------------

#[test]
fn promoted_durable_filter_keeps_only_shared_safe_in_root() {
    let db_dir = tempdir("genie-recall-promote");
    let db_path = db_dir.join("memory.db");
    let memory = Memory::open(&db_path).expect("open memory");

    let safe_id = memory
        .store("preference", "User likes ginger tea")
        .expect("seed safe");
    let person_id = memory
        .store("person_preference", "Maya likes oat milk")
        .expect("seed person");
    let restricted_id = memory
        .store("fact", "user's password is swordfish")
        .expect("seed restricted");
    let private_id = memory
        .store(
            "fact",
            "remember this privately: meeting with the lawyer Thursday",
        )
        .expect("seed private");

    for id in [safe_id, person_id, restricted_id, private_id] {
        memory.mark_promoted(id).expect("promote");
    }

    let canonical_dir = db_dir.join("memory");
    let root = canonical_dir.join("MEMORY.md");
    let root_text =
        std::fs::read_to_string(&root).unwrap_or_else(|e| panic!("read {}: {}", root.display(), e));

    assert!(
        root_text.contains("User likes ginger tea"),
        "shared-safe preference missing from MEMORY.md root: {}",
        root_text
    );
    assert!(
        !root_text.contains("Maya likes oat milk"),
        "person memory leaked into MEMORY.md root: {}",
        root_text
    );
    assert!(
        !root_text.contains("swordfish"),
        "restricted memory leaked into MEMORY.md root: {}",
        root_text
    );
    assert!(
        !root_text.contains("lawyer Thursday"),
        "private memory leaked into MEMORY.md root: {}",
        root_text
    );

    let person_note = canonical_dir.join("namespaces/person/preference.md");
    let person_text = std::fs::read_to_string(&person_note).expect("read person note");
    assert!(
        person_text.contains("redacted"),
        "person namespace projection should be redacted, got: {}",
        person_text
    );
    assert!(
        !person_text.contains("Maya likes oat milk"),
        "person namespace projection leaked content: {}",
        person_text
    );
}

#[test]
fn recall_hit_does_not_emit_miss_log() {
    let captured = capture_miss_events(|memory| {
        memory
            .store("preference", "User likes jazz music")
            .expect("store");
        let _ = recall_with_context(
            memory,
            "music",
            RECALL_LIMIT,
            MemoryReadContext::shared_room_voice(),
        );
    });

    assert!(
        captured.is_empty(),
        "hit path must not emit memory.recall.miss, but got {:?}",
        captured
    );
}

// ---------------------------------------------------------------------------
// Tracing capture helper.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct MissEvent {
    level: Level,
    cause: Option<String>,
    raw_hits: Option<usize>,
}

struct MissCapture {
    sink: Arc<Mutex<Vec<MissEvent>>>,
}

impl<S: Subscriber> Layer<S> for MissCapture {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: LayerContext<'_, S>) {
        if event.metadata().target() != "memory.recall.miss" {
            return;
        }
        let mut visitor = MissVisitor {
            cause: None,
            raw_hits: None,
        };
        event.record(&mut visitor);
        self.sink.lock().unwrap().push(MissEvent {
            level: *event.metadata().level(),
            cause: visitor.cause,
            raw_hits: visitor.raw_hits,
        });
    }
}

struct MissVisitor {
    cause: Option<String>,
    raw_hits: Option<usize>,
}

impl Visit for MissVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "cause" {
            self.cause = Some(value.to_string());
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "raw_hits" {
            self.raw_hits = Some(value as usize);
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        if field.name() == "raw_hits" && value >= 0 {
            self.raw_hits = Some(value as usize);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        // Fallback for fields recorded via Debug (e.g. ?identity_confidence).
        // Only fill `cause` if record_str hasn't already set it.
        if field.name() == "cause" && self.cause.is_none() {
            self.cause = Some(format!("{:?}", value).trim_matches('"').to_string());
        }
    }
}

fn capture_miss_events<F: FnOnce(&Memory)>(body: F) -> Vec<MissEvent> {
    let sink = Arc::new(Mutex::new(Vec::new()));
    let layer = MissCapture { sink: sink.clone() };
    let subscriber = Registry::default().with(layer);

    let db_dir = tempdir("genie-recall-log");
    let memory = Memory::open(&db_dir.join("memory.db")).expect("open memory");

    tracing::subscriber::with_default(subscriber, || {
        body(&memory);
    });

    let guard = sink.lock().unwrap();
    guard.clone()
}

// ---------------------------------------------------------------------------
// Paths.
// ---------------------------------------------------------------------------

fn workspace_root() -> PathBuf {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().parent().unwrap().to_path_buf()
}

fn tempdir(prefix: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "{}-{}-{}-{}",
        prefix,
        std::process::id(),
        n,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create tempdir");
    path
}
