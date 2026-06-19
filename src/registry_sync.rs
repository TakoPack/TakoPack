//! Registry-sync subcommand.
//!
//! Synchronises a local Cargo registry directory from the set of Rust crate
//! providers declared in a ruyispec repository.  The resulting directory
//! layout mirrors what Cargo expects under `$CARGO_HOME/registry/src/` so
//! that it can be used as a path-based registry source.
//!
//! High-level flow:
//! 1. Scan `SPECS/rust-*/*.spec` for `%global crate_name` / `%global full_version`.
//! 2. Compare the scan against a persisted index (`.takopack/index.json`).
//! 3. Download, extract, and patch changed / new crates.
//! 4. Remove stale entries and orphan directories.
//! 5. Write the updated index atomically.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Context;
use flate2::read::GzDecoder;
use glob::glob;
use regex::Regex;
use serde_derive::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::config::{default_registry_dir, load_takopack_toml};
use crate::errors::Result;

const TAKOPACK_METADATA_DIR: &str = ".takopack";
const REGISTRY_MARKER: &str = "managed-by-takopack";
const REGISTRY_MARKER_CONTENT: &str = "TakoPack cargo registry\n";

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the `registry-sync` subcommand.
///
/// Returns an exit code (0 = success, 1 = sync errors present).
pub fn run_registry_sync(dry_run: bool, jobs: usize) -> Result<i32> {
    let (ruyispec_dir, registry_dir) = resolve_paths()?;
    let jobs = jobs.max(1);

    println!("Registry sync");
    println!("  ruyispec: {}", ruyispec_dir.display());
    println!("  registry: {}", registry_dir.display());
    if !dry_run {
        println!("  jobs: {}", jobs);
    }
    if dry_run {
        println!("  (dry-run mode — no files will be modified)");
    }
    println!();

    ensure_registry_managed(&registry_dir, dry_run)?;

    // 1. Scan providers
    let scan = scan_providers(&ruyispec_dir)?;
    for warning in &scan.warnings {
        takopack_warn!("{}", warning.message);
    }
    let scan_warning_count = scan.warnings.len();
    let parse_warnings = scan.warning_count(ScanWarningKind::ParseFailed);
    let missing_cargo_toml_warnings = scan.warning_count(ScanWarningKind::MissingCargoToml);

    let current = scan.providers;
    log::info!("scanned {} provider(s)", current.len());

    // 2. Load existing index
    let old_index = load_index(&registry_dir)?;

    // 3. Reconcile
    let plan = reconcile(&current, &old_index, &registry_dir);
    log::debug!("plan: {:?}", plan.summary());

    // 4. Execute (or just report in dry-run mode)
    let mut warnings: usize = scan_warning_count;
    let mut sync_errors: usize = 0;

    let mut sync_jobs = Vec::new();

    // --- adds ---
    for spec_key in &plan.add {
        let entry = &current[spec_key];
        if dry_run {
            println!("  [add] {} → {}", spec_key, entry.registry_path);
        } else {
            sync_jobs.push(SyncJob::new(SyncKind::Add, entry.clone()));
        }
    }

    // --- updates ---
    for spec_key in &plan.update {
        let entry = &current[spec_key];
        if dry_run {
            println!("  [update] {} → {}", spec_key, entry.registry_path);
        } else {
            sync_jobs.push(SyncJob::new(SyncKind::Update, entry.clone()));
        }
    }

    if !dry_run {
        let (parallel_jobs, serial_jobs) = split_conflicting_jobs(sync_jobs);

        for failure in sync_entries_parallel(parallel_jobs, &ruyispec_dir, &registry_dir, jobs) {
            takopack_warn!(
                "failed to {} {}: {}",
                failure.kind.verb(),
                failure.registry_path,
                failure.error
            );
            warnings += 1;
            sync_errors += 1;
        }

        for job in serial_jobs {
            log::info!("{} {}", job.kind.gerund(), job.entry.registry_path);
            if let Err(e) = sync_crate(&job.entry, &ruyispec_dir, &registry_dir) {
                takopack_warn!(
                    "failed to {} {}: {:#}",
                    job.kind.verb(),
                    job.entry.registry_path,
                    e
                );
                warnings += 1;
                sync_errors += 1;
            }
        }
    }

    // --- removes ---
    for spec_key in &plan.remove {
        let entry = &old_index.entries[spec_key];
        let dir = registry_dir.join(&entry.registry_path);
        if dry_run {
            println!("  [remove] {} ({})", spec_key, entry.registry_path);
        } else {
            log::info!("removing {}", entry.registry_path);
            if dir.is_dir() {
                if let Err(e) = fs::remove_dir_all(&dir) {
                    takopack_warn!("failed to remove {}: {}", dir.display(), e);
                    warnings += 1;
                    sync_errors += 1;
                }
            }
        }
    }

    // --- skips ---
    for spec_key in &plan.skip {
        log::debug!("unchanged: {}", spec_key);
    }

    // 5. Clean orphan directories
    if !dry_run {
        let orphan_errors = clean_orphans(&registry_dir, &current);
        warnings += orphan_errors;
        sync_errors += orphan_errors;
    }

    // 6. Write new index
    if !dry_run && sync_errors == 0 {
        let new_index = build_index(&current);
        write_index(&registry_dir, &new_index)?;
    } else if !dry_run {
        takopack_warn!("registry index was not updated because sync errors occurred");
    }

    // 7. Summary
    println!();
    println!("Summary:");
    println!("  add={}", plan.add.len());
    println!("  update={}", plan.update.len());
    println!("  remove={}", plan.remove.len());
    println!("  skip={}", plan.skip.len());
    println!("  warnings={}", warnings);
    println!("  parse_warnings={}", parse_warnings);
    println!("  missing_cargo_toml={}", missing_cargo_toml_warnings);
    println!("  sync_errors={}", sync_errors);

    if sync_errors > 0 {
        Ok(1)
    } else {
        Ok(0)
    }
}

// ---------------------------------------------------------------------------
// Configuration helpers
// ---------------------------------------------------------------------------

/// Resolve the ruyispec directory and registry directory from `takopack.toml`.
fn resolve_paths() -> Result<(PathBuf, PathBuf)> {
    let (config_path, config) = load_takopack_toml()?.ok_or_else(|| {
        anyhow::anyhow!(
            "missing takopack.toml; create one with [ruyispec].local_path and optionally [registry].local_path"
        )
    })?;

    // Ruyispec
    let ruyispec_local = config.ruyispec.and_then(|r| r.local_path).ok_or_else(|| {
        anyhow::anyhow!(
            "{} does not define [ruyispec].local_path",
            config_path.display()
        )
    })?;
    let ruyispec_dir = if ruyispec_local.is_absolute() {
        ruyispec_local
    } else {
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(ruyispec_local)
    };
    if !ruyispec_dir.is_dir() {
        takopack_bail!(
            "ruyispec directory does not exist: {}",
            ruyispec_dir.display()
        );
    }

    // Registry
    let registry_dir = if let Some(registry_local) = config.registry.and_then(|r| r.local_path) {
        if registry_local.is_absolute() {
            registry_local
        } else {
            config_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(registry_local)
        }
    } else {
        default_registry_dir()?
    };

    Ok((ruyispec_dir, registry_dir))
}

// ---------------------------------------------------------------------------
// Registry safety marker
// ---------------------------------------------------------------------------

fn ensure_registry_managed(registry_dir: &Path, dry_run: bool) -> Result<()> {
    let marker = registry_marker_path(registry_dir);

    if marker.is_file() {
        return Ok(());
    }

    if !registry_dir.exists() {
        if dry_run {
            println!("  registry marker: would initialize {}", marker.display());
        } else {
            write_registry_marker(registry_dir)?;
        }
        return Ok(());
    }

    if !registry_dir.is_dir() {
        takopack_bail!(
            "registry path exists but is not a directory: {}",
            registry_dir.display()
        );
    }

    if registry_is_empty_or_metadata_only(registry_dir)? {
        if dry_run {
            println!("  registry marker: would create {}", marker.display());
        } else {
            write_registry_marker(registry_dir)?;
        }
        return Ok(());
    }

    takopack_bail!(
        "{} is non-empty and has no {}; it does not look like a TakoPack-managed registry. Use a different registry.local_path, or manually confirm by clearing the directory or creating the marker file before syncing.",
        registry_dir.display(),
        marker.display()
    );
}

fn registry_marker_path(registry_dir: &Path) -> PathBuf {
    registry_dir
        .join(TAKOPACK_METADATA_DIR)
        .join(REGISTRY_MARKER)
}

fn write_registry_marker(registry_dir: &Path) -> Result<()> {
    let metadata_dir = registry_dir.join(TAKOPACK_METADATA_DIR);
    fs::create_dir_all(&metadata_dir)
        .with_context(|| format!("failed to create {}", metadata_dir.display()))?;
    let marker = registry_marker_path(registry_dir);
    fs::write(&marker, REGISTRY_MARKER_CONTENT)
        .with_context(|| format!("failed to write {}", marker.display()))?;
    Ok(())
}

fn registry_is_empty_or_metadata_only(registry_dir: &Path) -> Result<bool> {
    for entry in fs::read_dir(registry_dir)
        .with_context(|| format!("failed to read {}", registry_dir.display()))?
    {
        let entry = entry.with_context(|| format!("failed to read {}", registry_dir.display()))?;
        if entry.file_name() != TAKOPACK_METADATA_DIR {
            return Ok(false);
        }
    }

    Ok(true)
}

// ---------------------------------------------------------------------------
// Provider scanning
// ---------------------------------------------------------------------------

/// Information extracted from a single `.spec` file.
#[derive(Debug, Clone)]
struct ProviderEntry {
    /// Relative path to the spec file from the ruyispec root, e.g.
    /// `SPECS/rust-tokio-1/rust-tokio-1.spec`.
    spec_key: String,
    /// Crate name as declared via `%global crate_name`.
    crate_name: String,
    /// Crate version as declared via `%global full_version`.
    version: String,
    /// RPM name derived from the directory name (e.g. `rust-tokio-1`).
    rpm_name: String,
    /// Registry sub-directory, `{crate_name}-{version}`.
    registry_path: String,
    /// SHA-256 hex digest of the spec file contents.
    spec_hash: String,
    /// SHA-256 hex digest of the provider `Cargo.toml`, if present.
    cargo_toml_hash: Option<String>,
}

/// Result of scanning the ruyispec tree.
#[derive(Debug, Default)]
struct ScanResult {
    providers: BTreeMap<String, ProviderEntry>,
    warnings: Vec<ScanWarning>,
}

impl ScanResult {
    fn warning_count(&self, kind: ScanWarningKind) -> usize {
        self.warnings
            .iter()
            .filter(|warning| warning.kind == kind)
            .count()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanWarningKind {
    MissingCargoToml,
    ParseFailed,
}

#[derive(Debug, Clone)]
struct ScanWarning {
    kind: ScanWarningKind,
    message: String,
}

impl ScanWarning {
    fn new(kind: ScanWarningKind, message: String) -> Self {
        Self { kind, message }
    }
}

/// Scan `{ruyispec}/SPECS/rust-*/*.spec` and return a map keyed by `spec_key`.
fn scan_providers(ruyispec_dir: &Path) -> Result<ScanResult> {
    let pattern = format!("{}/SPECS/rust-*/*.spec", ruyispec_dir.display());
    let re_crate = Regex::new(r"^%global\s+crate_name\s+(\S+)").expect("regex");
    let re_version = Regex::new(r"^%global\s+full_version\s+(\S+)").expect("regex");

    let mut providers = BTreeMap::new();
    let mut warnings = Vec::new();

    for entry in glob(&pattern).context("invalid glob pattern")? {
        let spec_path = entry.context("glob error")?;
        let content = fs::read_to_string(&spec_path)
            .with_context(|| format!("failed to read {}", spec_path.display()))?;
        if !is_rustcrates_spec(&content) {
            continue;
        }

        // Directory-based RPM name
        let spec_dir = spec_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("spec file has no parent dir"))?;
        let rpm_name = spec_dir
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("cannot extract dir name for {}", spec_path.display()))?
            .to_string_lossy()
            .to_string();

        let spec_file = spec_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| spec_path.display().to_string());

        // Parse %global macros
        let mut crate_name: Option<String> = None;
        let mut version: Option<String> = None;
        for line in content.lines() {
            if let Some(caps) = re_crate.captures(line) {
                crate_name = Some(caps[1].to_string());
            }
            if let Some(caps) = re_version.captures(line) {
                version = Some(caps[1].to_string());
            }
            if crate_name.is_some() && version.is_some() {
                break;
            }
        }

        let (crate_name, version) = match (crate_name, version) {
            (Some(crate_name), Some(version)) => (crate_name, version),
            (None, Some(_)) => {
                warnings.push(ScanWarning::new(
                    ScanWarningKind::ParseFailed,
                    format!(
                        "warning: {} failed to parse crate_name from {}",
                        rpm_name, spec_file
                    ),
                ));
                continue;
            }
            (Some(_), None) => {
                warnings.push(ScanWarning::new(
                    ScanWarningKind::ParseFailed,
                    format!(
                        "warning: {} failed to parse full_version from {}",
                        rpm_name, spec_file
                    ),
                ));
                continue;
            }
            (None, None) => {
                warnings.push(ScanWarning::new(
                    ScanWarningKind::ParseFailed,
                    format!(
                        "warning: {} failed to parse crate_name/full_version from {}",
                        rpm_name, spec_file
                    ),
                ));
                continue;
            }
        };

        // Relative spec_key
        let spec_key = spec_path
            .strip_prefix(ruyispec_dir)
            .with_context(|| {
                format!(
                    "{} is not under {}",
                    spec_path.display(),
                    ruyispec_dir.display()
                )
            })?
            .to_string_lossy()
            .replace('\\', "/");

        let spec_hash = sha256_hex(content.as_bytes());

        // Optional provider Cargo.toml
        let cargo_toml_path = spec_dir.join("Cargo.toml");
        let cargo_toml_hash = if cargo_toml_path.is_file() {
            let cargo_bytes = fs::read(&cargo_toml_path)
                .with_context(|| format!("failed to read {}", cargo_toml_path.display()))?;
            Some(sha256_hex(&cargo_bytes))
        } else {
            warnings.push(ScanWarning::new(
                ScanWarningKind::MissingCargoToml,
                format!(
                    "warning: {} has no Cargo.toml override; using crates.io Cargo.toml",
                    rpm_name
                ),
            ));
            None
        };

        let registry_path = format!("{}-{}", crate_name, version);

        providers.insert(
            spec_key.clone(),
            ProviderEntry {
                spec_key,
                crate_name,
                version,
                rpm_name,
                registry_path,
                spec_hash,
                cargo_toml_hash,
            },
        );
    }

    Ok(ScanResult {
        providers,
        warnings,
    })
}

fn is_rustcrates_spec(content: &str) -> bool {
    content.lines().any(|line| {
        let Some((key, value)) = line.trim().split_once(':') else {
            return false;
        };

        key.trim().eq_ignore_ascii_case("BuildSystem")
            && value.trim().eq_ignore_ascii_case("rustcrates")
    })
}

// ---------------------------------------------------------------------------
// Persistent index
// ---------------------------------------------------------------------------

/// On-disk index stored at `{registry}/.takopack/index.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistryIndex {
    schema_version: u32,
    entries: BTreeMap<String, IndexEntry>,
}

/// One entry in the persisted index.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexEntry {
    crate_name: String,
    version: String,
    rpm_name: String,
    registry_path: String,
    spec_hash: String,
    cargo_toml_hash: Option<String>,
}

impl RegistryIndex {
    fn empty() -> Self {
        Self {
            schema_version: 1,
            entries: BTreeMap::new(),
        }
    }
}

fn load_index(registry_dir: &Path) -> Result<RegistryIndex> {
    let path = registry_dir.join(".takopack").join("index.json");
    if !path.is_file() {
        return Ok(RegistryIndex::empty());
    }
    let data =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let index: RegistryIndex = serde_json::from_str(&data)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if index.schema_version != 1 {
        takopack_bail!(
            "unsupported registry index schema_version {}; expected 1",
            index.schema_version
        );
    }
    Ok(index)
}

fn build_index(providers: &BTreeMap<String, ProviderEntry>) -> RegistryIndex {
    let entries = providers
        .iter()
        .map(|(key, p)| {
            (
                key.clone(),
                IndexEntry {
                    crate_name: p.crate_name.clone(),
                    version: p.version.clone(),
                    rpm_name: p.rpm_name.clone(),
                    registry_path: p.registry_path.clone(),
                    spec_hash: p.spec_hash.clone(),
                    cargo_toml_hash: p.cargo_toml_hash.clone(),
                },
            )
        })
        .collect();

    RegistryIndex {
        schema_version: 1,
        entries,
    }
}

/// Atomically write the index: write to a temporary file then rename.
fn write_index(registry_dir: &Path, index: &RegistryIndex) -> Result<()> {
    let dir = registry_dir.join(".takopack");
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;

    let final_path = dir.join("index.json");
    let tmp_path = dir.join("index.json.tmp");

    let json = serde_json::to_string_pretty(index)?;
    fs::write(&tmp_path, format!("{json}\n").as_bytes())
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    fs::rename(&tmp_path, &final_path).with_context(|| {
        format!(
            "failed to rename {} → {}",
            tmp_path.display(),
            final_path.display()
        )
    })?;

    log::info!("wrote {}", final_path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Reconciliation
// ---------------------------------------------------------------------------

/// Actions determined by comparing the current scan against the persisted index.
#[derive(Debug)]
struct SyncPlan {
    add: Vec<String>,
    update: Vec<String>,
    remove: Vec<String>,
    skip: Vec<String>,
}

impl SyncPlan {
    fn summary(&self) -> String {
        format!(
            "add={} update={} remove={} skip={}",
            self.add.len(),
            self.update.len(),
            self.remove.len(),
            self.skip.len(),
        )
    }
}

fn reconcile(
    current: &BTreeMap<String, ProviderEntry>,
    old_index: &RegistryIndex,
    registry_dir: &Path,
) -> SyncPlan {
    let mut add = Vec::new();
    let mut update = Vec::new();
    let mut skip = Vec::new();
    let mut remove = Vec::new();

    for (key, entry) in current {
        match old_index.entries.get(key) {
            None => add.push(key.clone()),
            Some(old) => {
                let hashes_match = old.spec_hash == entry.spec_hash
                    && old.cargo_toml_hash == entry.cargo_toml_hash;
                let dir_exists = registry_dir.join(&entry.registry_path).is_dir();

                if hashes_match && dir_exists {
                    skip.push(key.clone());
                } else {
                    update.push(key.clone());
                }
            }
        }
    }

    for key in old_index.entries.keys() {
        if !current.contains_key(key) {
            remove.push(key.clone());
        }
    }

    SyncPlan {
        add,
        update,
        remove,
        skip,
    }
}

// ---------------------------------------------------------------------------
// Parallel execution
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum SyncKind {
    Add,
    Update,
}

impl SyncKind {
    fn verb(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Update => "update",
        }
    }

    fn gerund(self) -> &'static str {
        match self {
            Self::Add => "adding",
            Self::Update => "updating",
        }
    }
}

#[derive(Debug, Clone)]
struct SyncJob {
    kind: SyncKind,
    entry: ProviderEntry,
}

impl SyncJob {
    fn new(kind: SyncKind, entry: ProviderEntry) -> Self {
        Self { kind, entry }
    }
}

#[derive(Debug)]
struct SyncFailure {
    kind: SyncKind,
    registry_path: String,
    error: String,
}

fn split_conflicting_jobs(jobs: Vec<SyncJob>) -> (Vec<SyncJob>, Vec<SyncJob>) {
    let mut counts = BTreeMap::<String, usize>::new();
    for job in &jobs {
        *counts.entry(job.entry.registry_path.clone()).or_default() += 1;
    }

    let mut parallel_jobs = Vec::new();
    let mut serial_jobs = Vec::new();

    for job in jobs {
        if counts.get(&job.entry.registry_path).copied().unwrap_or(0) > 1 {
            serial_jobs.push(job);
        } else {
            parallel_jobs.push(job);
        }
    }

    (parallel_jobs, serial_jobs)
}

fn sync_entries_parallel(
    jobs: Vec<SyncJob>,
    ruyispec_dir: &Path,
    registry_dir: &Path,
    worker_count: usize,
) -> Vec<SyncFailure> {
    if jobs.is_empty() {
        return Vec::new();
    }

    let worker_count = worker_count.max(1).min(jobs.len());
    let queue = Arc::new(Mutex::new(jobs.into_iter().collect::<VecDeque<_>>()));
    let failures = Arc::new(Mutex::new(Vec::new()));
    let ruyispec_dir = Arc::new(ruyispec_dir.to_path_buf());
    let registry_dir = Arc::new(registry_dir.to_path_buf());

    let mut handles = Vec::with_capacity(worker_count);
    for _ in 0..worker_count {
        let queue = Arc::clone(&queue);
        let failures = Arc::clone(&failures);
        let ruyispec_dir = Arc::clone(&ruyispec_dir);
        let registry_dir = Arc::clone(&registry_dir);

        handles.push(thread::spawn(move || loop {
            let job = {
                let mut queue = queue.lock().expect("sync job queue should not be poisoned");
                queue.pop_front()
            };

            let Some(job) = job else {
                break;
            };

            log::info!("{} {}", job.kind.gerund(), job.entry.registry_path);
            if let Err(err) = sync_crate(&job.entry, &ruyispec_dir, &registry_dir) {
                let failure = SyncFailure {
                    kind: job.kind,
                    registry_path: job.entry.registry_path,
                    error: format!("{:#}", err),
                };
                failures
                    .lock()
                    .expect("sync failure list should not be poisoned")
                    .push(failure);
            }
        }));
    }

    for handle in handles {
        if handle.join().is_err() {
            failures
                .lock()
                .expect("sync failure list should not be poisoned")
                .push(SyncFailure {
                    kind: SyncKind::Update,
                    registry_path: "<worker>".to_string(),
                    error: "registry sync worker panicked".to_string(),
                });
        }
    }

    Arc::try_unwrap(failures)
        .expect("all worker references should be dropped")
        .into_inner()
        .expect("sync failure list should not be poisoned")
}

// ---------------------------------------------------------------------------
// Crate download / extract / patch
// ---------------------------------------------------------------------------

/// Download a crate tarball from crates.io, extract it, optionally overlay
/// the provider `Cargo.toml`, and regenerate `.cargo-checksum.json`.
fn sync_crate(entry: &ProviderEntry, ruyispec_dir: &Path, registry_dir: &Path) -> Result<()> {
    let dest = registry_dir.join(&entry.registry_path);
    fs::create_dir_all(registry_dir)
        .with_context(|| format!("failed to create {}", registry_dir.display()))?;

    let temp_dir = tempfile::Builder::new()
        .prefix(".takopack-sync-")
        .tempdir_in(registry_dir)
        .with_context(|| format!("failed to create temp dir in {}", registry_dir.display()))?;
    let work_dir = temp_dir.path();

    // Download
    let url = format!(
        "https://static.crates.io/crates/{}/{}/download",
        entry.crate_name, entry.version
    );
    log::info!("downloading {}", url);
    let body = download_crate_tarball(&url)?;

    // Extract (gzipped tar)
    extract_tarball(&body, work_dir, &entry.registry_path)?;

    // Overlay provider Cargo.toml if present
    let spec_dir = ruyispec_dir.join(
        Path::new(&entry.spec_key)
            .parent()
            .unwrap_or_else(|| Path::new("")),
    );
    let provider_cargo = spec_dir.join("Cargo.toml");
    if provider_cargo.is_file() {
        let dest_cargo = work_dir.join("Cargo.toml");
        fs::copy(&provider_cargo, &dest_cargo).with_context(|| {
            format!(
                "failed to copy {} → {}",
                provider_cargo.display(),
                dest_cargo.display()
            )
        })?;
        log::debug!("overlaid provider Cargo.toml for {}", entry.registry_path);
    }

    // Regenerate .cargo-checksum.json
    write_cargo_checksum(work_dir)?;

    // Replace the target only after the new crate directory is complete.
    if dest.is_dir() {
        fs::remove_dir_all(&dest)
            .with_context(|| format!("failed to remove old {}", dest.display()))?;
    } else if dest.exists() {
        fs::remove_file(&dest)
            .with_context(|| format!("failed to remove old {}", dest.display()))?;
    }
    fs::rename(work_dir, &dest).with_context(|| {
        format!(
            "failed to rename {} → {}",
            work_dir.display(),
            dest.display()
        )
    })?;

    Ok(())
}

fn download_crate_tarball(url: &str) -> Result<Vec<u8>> {
    const ATTEMPTS: usize = 3;

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(120))
        .timeout_write(Duration::from_secs(30))
        .build();

    let mut last_error = String::new();
    for attempt in 1..=ATTEMPTS {
        let result = (|| -> Result<Vec<u8>> {
            let resp = agent
                .get(url)
                .call()
                .with_context(|| format!("HTTP request failed for {}", url))?;

            let mut body = Vec::new();
            resp.into_reader()
                .read_to_end(&mut body)
                .with_context(|| format!("failed to read response body from {}", url))?;
            Ok(body)
        })();

        match result {
            Ok(body) => return Ok(body),
            Err(err) => {
                last_error = format!("{:#}", err);
                if attempt < ATTEMPTS {
                    log::warn!(
                        "download attempt {}/{} failed for {}: {}",
                        attempt,
                        ATTEMPTS,
                        url,
                        last_error
                    );
                }
            }
        }
    }

    takopack_bail!(
        "failed to download {} after {} attempts: {}",
        url,
        ATTEMPTS,
        last_error
    );
}

/// Extract a gzipped tarball into `dest`, stripping the common top-level
/// directory prefix (which is typically `{crate_name}-{version}/`).
fn extract_tarball(tarball: &[u8], dest: &Path, expected_prefix: &str) -> Result<()> {
    let gz = GzDecoder::new(tarball);
    let mut archive = tar::Archive::new(gz);
    let mut extracted_files = 0usize;

    for file in archive.entries().context("failed to read tar entries")? {
        let mut file = file.context("corrupt tar entry")?;
        let raw_path = file.path().context("bad tar entry path")?.into_owned();

        // Strip the top-level directory.  Most crates.io tarballs contain
        // `{name}-{version}/…` as the prefix.  We strip exactly one leading
        // component so the result lands directly in `dest`.
        let stripped = safe_crate_entry_path(&raw_path, expected_prefix)?;

        let Some(stripped) = stripped else {
            // Top-level directory entry itself — skip it.
            continue;
        };

        let out_path = dest.join(&stripped);

        if file.header().entry_type().is_dir() {
            fs::create_dir_all(&out_path)
                .with_context(|| format!("mkdir {}", out_path.display()))?;
        } else {
            // Ensure parent exists
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("mkdir -p {}", parent.display()))?;
            }
            let mut out_file = fs::File::create(&out_path)
                .with_context(|| format!("create {}", out_path.display()))?;
            io::copy(&mut file, &mut out_file)
                .with_context(|| format!("write {}", out_path.display()))?;
            extracted_files += 1;
        }
    }

    if extracted_files == 0 {
        takopack_bail!(
            "crate archive for {} did not contain any files",
            expected_prefix
        );
    }

    Ok(())
}

fn safe_crate_entry_path(raw_path: &Path, expected_prefix: &str) -> Result<Option<PathBuf>> {
    let mut components = raw_path.components();
    match components.next() {
        Some(std::path::Component::Normal(prefix)) if prefix == expected_prefix => {}
        Some(std::path::Component::Normal(prefix)) => {
            takopack_bail!(
                "crate archive entry {} has unexpected top-level directory {}; expected {}",
                raw_path.display(),
                prefix.to_string_lossy(),
                expected_prefix
            );
        }
        Some(_) => {
            takopack_bail!("unsafe path in crate archive: {}", raw_path.display());
        }
        None => return Ok(None),
    }

    let mut stripped = PathBuf::new();
    for component in components {
        match component {
            std::path::Component::Normal(part) => stripped.push(part),
            std::path::Component::CurDir => {}
            _ => {
                takopack_bail!("unsafe path in crate archive: {}", raw_path.display());
            }
        }
    }

    if stripped.as_os_str().is_empty() {
        Ok(None)
    } else {
        Ok(Some(stripped))
    }
}

/// Regenerate `.cargo-checksum.json` for a crate directory.
///
/// The format is:
/// ```json
/// {"files":{"relative/path":"sha256hex",…},"package":null}
/// ```
fn write_cargo_checksum(crate_dir: &Path) -> Result<()> {
    let checksum_name = ".cargo-checksum.json";
    let mut files: BTreeMap<String, String> = BTreeMap::new();

    for entry in WalkDir::new(crate_dir).sort_by_file_name() {
        let entry = entry.context("walkdir error")?;
        if !entry.file_type().is_file() {
            continue;
        }
        let abs_path = entry.path();
        let rel = abs_path.strip_prefix(crate_dir).unwrap_or(abs_path);
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        // Exclude the checksum file itself.
        if rel_str == checksum_name {
            continue;
        }

        let data =
            fs::read(abs_path).with_context(|| format!("failed to read {}", abs_path.display()))?;
        files.insert(rel_str, sha256_hex(&data));
    }

    #[derive(Serialize)]
    struct CargoChecksum {
        files: BTreeMap<String, String>,
        package: Option<String>,
    }

    let checksum = CargoChecksum {
        files,
        package: None,
    };
    let json = serde_json::to_string(&checksum)?;

    let out = crate_dir.join(checksum_name);
    fs::write(&out, json.as_bytes())
        .with_context(|| format!("failed to write {}", out.display()))?;

    log::debug!("wrote {}", out.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Orphan cleanup
// ---------------------------------------------------------------------------

/// Remove directories under `registry_dir` that are not in the desired set
/// and not the `.takopack` metadata directory.
///
/// Returns the number of warnings emitted.
fn clean_orphans(registry_dir: &Path, current: &BTreeMap<String, ProviderEntry>) -> usize {
    let desired: HashSet<String> = current.values().map(|e| e.registry_path.clone()).collect();

    let mut warnings = 0usize;

    let entries = match fs::read_dir(registry_dir) {
        Ok(e) => e,
        Err(_) => return 0, // directory may not exist yet
    };

    for dir_entry in entries {
        let dir_entry = match dir_entry {
            Ok(e) => e,
            Err(e) => {
                log::warn!("readdir error: {}", e);
                warnings += 1;
                continue;
            }
        };

        let name = dir_entry.file_name().to_string_lossy().to_string();
        if name == ".takopack" {
            continue;
        }

        if !dir_entry.path().is_dir() {
            continue;
        }

        if !desired.contains(&name) {
            log::info!("removing orphan directory: {}", name);
            if let Err(e) = fs::remove_dir_all(dir_entry.path()) {
                takopack_warn!("failed to remove orphan {}: {}", name, e);
                warnings += 1;
            }
        }
    }

    warnings
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// Compute the SHA-256 hex digest of `data`.
fn sha256_hex(data: &[u8]) -> String {
    let hash = Sha256::digest(data);
    hex_encode(&hash)
}

/// Lower-case hex encoding (avoids pulling in `hex` crate).
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_hex() {
        let hash = sha256_hex(b"hello world");
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn test_empty_index_roundtrip() {
        let idx = RegistryIndex::empty();
        let json = serde_json::to_string_pretty(&idx).unwrap();
        let parsed: RegistryIndex = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.schema_version, 1);
        assert!(parsed.entries.is_empty());
    }

    #[test]
    fn load_index_rejects_unsupported_schema_version() {
        let temp = tempfile::tempdir().unwrap();
        let index_dir = temp.path().join(TAKOPACK_METADATA_DIR);
        std::fs::create_dir_all(&index_dir).unwrap();
        std::fs::write(
            index_dir.join("index.json"),
            r#"{"schema_version":2,"entries":{}}"#,
        )
        .unwrap();

        let err = load_index(temp.path()).unwrap_err();
        assert!(err
            .to_string()
            .contains("unsupported registry index schema_version 2; expected 1"));
    }

    #[test]
    fn registry_marker_initializes_new_directory() {
        let temp = tempfile::tempdir().unwrap();
        let registry = temp.path().join("registry");

        ensure_registry_managed(&registry, false).unwrap();

        assert!(registry_marker_path(&registry).is_file());
    }

    #[test]
    fn registry_marker_accepts_existing_marker() {
        let temp = tempfile::tempdir().unwrap();
        let registry = temp.path().join("registry");
        write_registry_marker(&registry).unwrap();

        ensure_registry_managed(&registry, false).unwrap();
    }

    #[test]
    fn registry_marker_rejects_nonempty_unmarked_directory() {
        let temp = tempfile::tempdir().unwrap();
        let registry = temp.path().join("registry");
        std::fs::create_dir_all(registry.join("serde-1.0.0")).unwrap();

        let err = ensure_registry_managed(&registry, false).unwrap_err();

        assert!(err
            .to_string()
            .contains("does not look like a TakoPack-managed registry"));
        assert!(registry.join("serde-1.0.0").is_dir());
    }

    #[test]
    fn scan_provider_reports_parse_and_manifest_warnings() {
        let temp = tempfile::tempdir().unwrap();
        let rust_foo = temp.path().join("SPECS/rust-foo-1");
        std::fs::create_dir_all(&rust_foo).unwrap();
        std::fs::write(
            rust_foo.join("rust-foo-1.spec"),
            "BuildSystem: rustcrates\n%global crate_name foo\n%global full_version 1.2.3\n",
        )
        .unwrap();

        let rust_bin = temp.path().join("SPECS/rust-bin");
        std::fs::create_dir_all(&rust_bin).unwrap();
        std::fs::write(rust_bin.join("rust-bin.spec"), "Name: rust-bin\n").unwrap();

        let rust_bad = temp.path().join("SPECS/rust-bad-1");
        std::fs::create_dir_all(&rust_bad).unwrap();
        std::fs::write(
            rust_bad.join("rust-bad-1.spec"),
            "BuildSystem: rustcrates\nName: rust-bad-1\n",
        )
        .unwrap();

        let scan = scan_providers(temp.path()).unwrap();
        assert_eq!(scan.providers.len(), 1);
        assert_eq!(scan.warnings.len(), 2);
        assert_eq!(scan.warning_count(ScanWarningKind::MissingCargoToml), 1);
        assert_eq!(scan.warning_count(ScanWarningKind::ParseFailed), 1);
        assert!(scan
            .warnings
            .iter()
            .any(|warning| warning.message.contains("has no Cargo.toml override")));
        assert!(scan.warnings.iter().any(|warning| warning
            .message
            .contains("failed to parse crate_name/full_version")));
        assert!(!scan
            .warnings
            .iter()
            .any(|warning| warning.message.contains("rust-bin")));
    }

    #[test]
    fn is_rustcrates_spec_accepts_case_and_spacing() {
        assert!(is_rustcrates_spec("  BuildSystem:   rustcrates  \n"));
        assert!(is_rustcrates_spec("buildsystem: RUSTCRATES\n"));
        assert!(!is_rustcrates_spec("BuildSystem: rust\n"));
        assert!(!is_rustcrates_spec("%global crate_name foo\n"));
    }

    #[test]
    fn safe_crate_entry_path_rejects_parent_components() {
        let err = safe_crate_entry_path(Path::new("crate-1.0.0/../Cargo.toml"), "crate-1.0.0")
            .unwrap_err();
        assert!(err.to_string().contains("unsafe path in crate archive"));

        let safe = safe_crate_entry_path(Path::new("crate-1.0.0/src/lib.rs"), "crate-1.0.0")
            .unwrap()
            .unwrap();
        assert_eq!(safe, PathBuf::from("src/lib.rs"));
    }

    #[test]
    fn extract_tarball_accepts_expected_prefix() {
        let temp = tempfile::tempdir().unwrap();
        let tarball = test_tarball(&[("foo-1.2.3/src/lib.rs", b"pub fn ok() {}\n")]);

        extract_tarball(&tarball, temp.path(), "foo-1.2.3").unwrap();

        assert_eq!(
            std::fs::read_to_string(temp.path().join("src/lib.rs")).unwrap(),
            "pub fn ok() {}\n"
        );
    }

    #[test]
    fn extract_tarball_rejects_wrong_prefix() {
        let temp = tempfile::tempdir().unwrap();
        let tarball = test_tarball(&[("bar-1.2.3/src/lib.rs", b"")]);

        let err = extract_tarball(&tarball, temp.path(), "foo-1.2.3").unwrap_err();

        assert!(err
            .to_string()
            .contains("unexpected top-level directory bar-1.2.3; expected foo-1.2.3"));
    }

    #[test]
    fn extract_tarball_rejects_parent_component() {
        let temp = tempfile::tempdir().unwrap();
        let tarball = test_tarball(&[("foo-1.2.3/../Cargo.toml", b"")]);

        let err = extract_tarball(&tarball, temp.path(), "foo-1.2.3").unwrap_err();

        assert!(err.to_string().contains("unsafe path in crate archive"));
    }

    #[test]
    fn extract_tarball_rejects_archives_without_files() {
        let temp = tempfile::tempdir().unwrap();
        let tarball = test_tarball(&[]);

        let err = extract_tarball(&tarball, temp.path(), "foo-1.2.3").unwrap_err();

        assert!(err
            .to_string()
            .contains("crate archive for foo-1.2.3 did not contain any files"));
    }

    fn test_tarball(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tarball = Vec::new();

        for (path, data) in entries {
            let path_bytes = path.as_bytes();
            assert!(path_bytes.len() <= 100);

            let mut header = [0u8; 512];
            header[..path_bytes.len()].copy_from_slice(path_bytes);
            write_tar_octal(&mut header[100..108], 0o644);
            write_tar_octal(&mut header[108..116], 0);
            write_tar_octal(&mut header[116..124], 0);
            write_tar_octal(&mut header[124..136], data.len() as u64);
            write_tar_octal(&mut header[136..148], 0);
            header[148..156].fill(b' ');
            header[156] = b'0';
            header[257..263].copy_from_slice(b"ustar\0");
            header[263..265].copy_from_slice(b"00");

            let checksum: u32 = header.iter().map(|byte| *byte as u32).sum();
            let checksum = format!("{:06o}\0 ", checksum);
            header[148..156].copy_from_slice(checksum.as_bytes());

            tarball.extend_from_slice(&header);
            tarball.extend_from_slice(data);
            let padding = (512 - (data.len() % 512)) % 512;
            tarball.extend(std::iter::repeat(0).take(padding));
        }

        tarball.extend(std::iter::repeat(0).take(1024));

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, &tarball).unwrap();
        encoder.finish().unwrap()
    }

    fn write_tar_octal(field: &mut [u8], value: u64) {
        let text = format!("{:0width$o}\0", value, width = field.len() - 1);
        field.copy_from_slice(text.as_bytes());
    }
}
