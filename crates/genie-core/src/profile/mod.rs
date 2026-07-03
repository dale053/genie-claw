//! Personal profile system — ingest user data from files into memory.
//!
//! Supports:
//! - `profile.toml` — structured identity, preferences, relationships
//! - `*.md`, `*.txt` — free-form text, auto-extracted via patterns
//! - `*.pdf` — text extracted via `pdftotext`, then pattern-extracted
//!
//! All data stays local. Files live in `/opt/geniepod/data/profile/`.
//! On startup, genie-core scans this directory and ingests into memory.
//!
//! ## Version Roadmap
//! - V1: Single user, file-based profile
//! - V3: Speaker identification, multi-user, per-user isolation

pub mod ingest;
pub mod toml_profile;

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::memory::{Memory, SharedMemory, with_shared_memory};

/// Ingest all profile data from the profile directory into memory.
///
/// Called once at startup. Skips files that have already been ingested
/// (tracked via a metadata entry in memory).
///
/// PDF extraction shells out to `pdftotext` and is the one `.await` in this
/// function — deliberately run before taking the memory lock (via
/// [`ingest::extract_pdf_text`]) so a hung or slow `pdftotext` call never
/// holds `memory`'s mutex, and is itself timeout-guarded so it can't block
/// startup indefinitely.
pub async fn load_profile(profile_dir: &Path, memory: &SharedMemory) -> Result<ProfileReport> {
    let mut report = ProfileReport::default();

    if !profile_dir.exists() {
        tracing::debug!(dir = %profile_dir.display(), "profile directory not found — skipping");
        return Ok(report);
    }

    // 1. Load profile.toml (structured data — always re-read). Synchronous,
    // no subprocess involved, so a brief lock is fine.
    let toml_path = profile_dir.join("profile.toml");
    if toml_path.exists() {
        let result = with_shared_memory(memory, |mem| {
            toml_profile::load_toml_profile(&toml_path, mem)
        });
        match result {
            Ok(count) => {
                tracing::info!(facts = count, "profile.toml loaded");
                report.toml_facts = count;
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to load profile.toml");
            }
        }
    }

    // 2. Scan for document files (.md, .txt, .pdf).
    let entries = std::fs::read_dir(profile_dir)?;
    let mut doc_paths: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|n| n == "profile.toml") {
            continue;
        }
        doc_paths.push(path);
    }

    for path in doc_paths {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        match ext.as_str() {
            "md" | "txt" => {
                // Fast local disk read + synchronous memory writes — no
                // subprocess, so one short lock for the whole file is fine.
                let path_for_ingest = path.clone();
                let count = with_shared_memory(memory, move |mem| {
                    ingest::ingest_text_file(&path_for_ingest, mem)
                });
                if count > 0 {
                    tracing::info!(
                        file = %path.display(),
                        facts = count,
                        "ingested text file"
                    );
                }
                report.doc_facts += count;
                report.files_processed += 1;
            }
            "pdf" => {
                // Extraction (the subprocess call) runs with no lock held;
                // only the resulting text is ingested under the lock.
                let Some(text) = ingest::extract_pdf_text(&path).await else {
                    continue;
                };
                let filename = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let count = with_shared_memory(memory, move |mem: &Memory| {
                    ingest::ingest_pdf_text(&text, &filename, mem)
                });
                if count > 0 {
                    tracing::info!(
                        file = %path.display(),
                        facts = count,
                        "ingested PDF file"
                    );
                }
                report.doc_facts += count;
                report.files_processed += 1;
            }
            _ => {
                // Skip unsupported file types.
            }
        }
    }

    Ok(report)
}

/// Report from profile loading.
#[derive(Debug, Default)]
pub struct ProfileReport {
    pub toml_facts: usize,
    pub doc_facts: usize,
    pub files_processed: usize,
}

impl ProfileReport {
    pub fn total(&self) -> usize {
        self.toml_facts + self.doc_facts
    }
}
