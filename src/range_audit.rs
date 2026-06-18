//! Range capability audit for Cargo dependencies.
//!
//! Detects when a Cargo dependency version requirement spans multiple TakoPack
//! RPM compat keys, which would cause the generated `Requires: crate(foo-X/...)`
//! to be potentially unsatisfiable by a higher-compat provider.
//!
//! The core function [`audit_range_capability_ambiguity`] is intended to be
//! reusable from multiple call-sites: the `range-audit` subcommand, and the
//! `package`/`localpkg` spec generation paths.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use semver::{Comparator, Op, Version, VersionReq};

use crate::crates::dependency_is_runtime_candidate;
use crate::takopack::spec::normalize_crate_name;
use crate::util::calculate_compat_version;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Policy for handling range capability warnings during spec generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum RangeCapabilityPolicy {
    /// Print warnings to stderr but continue generating the spec.
    Warn,
    /// Print errors and exit non-zero; abort spec generation.
    Error,
    /// Suppress all range capability diagnostics (old behaviour).
    Allow,
}

impl Default for RangeCapabilityPolicy {
    fn default() -> Self {
        RangeCapabilityPolicy::Warn
    }
}

impl fmt::Display for RangeCapabilityPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RangeCapabilityPolicy::Warn => write!(f, "warn"),
            RangeCapabilityPolicy::Error => write!(f, "error"),
            RangeCapabilityPolicy::Allow => write!(f, "allow"),
        }
    }
}

/// A single range-capability warning.
#[derive(Debug, Clone, serde_derive::Serialize)]
pub struct RangeWarning {
    /// Path to the Cargo.toml that contains the dependency (if known).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<String>,
    /// The provider / crate being packaged (if known).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Dependency crate name.
    pub dependency: String,
    /// The raw version requirement string from Cargo.toml.
    pub requirement: String,
    /// The generated RPM capability string (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_capability: Option<String>,
    /// Human-readable reason.
    pub reason: String,
}

impl fmt::Display for RangeWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "warning: range dependency may map to wrong RPM crate capability"
        )?;
        if let Some(ref provider) = self.provider {
            writeln!(f, "  provider: {}", provider)?;
        }
        if let Some(ref manifest) = self.manifest {
            writeln!(f, "  manifest: {}", manifest)?;
        }
        writeln!(f, "  dependency: {}", self.dependency)?;
        writeln!(f, "  requirement: {}", self.requirement)?;
        if let Some(ref cap) = self.generated_capability {
            writeln!(f, "  generated capability: {}", cap)?;
        }
        writeln!(
            f,
            "  reason: requirement spans multiple RPM crate capability keys"
        )?;
        writeln!(f, "  risk: {}", self.reason)?;
        write!(
            f,
            "  recommendation: patch Cargo.toml to the lock-selected version, then regenerate provider spec"
        )
    }
}

// ---------------------------------------------------------------------------
// Core audit logic
// ---------------------------------------------------------------------------

/// Compute the compat key for a semver `Version`.
///
/// This delegates to [`crate::util::calculate_compat_version`] so the audit
/// logic always stays in sync with the actual spec-generation policy.
fn compat_key(version: &Version) -> String {
    calculate_compat_version(version)
}

/// Given a version requirement string, determine whether it spans more than
/// one TakoPack RPM compat key.
///
/// Returns `Some(RangeWarning)` when the requirement is ambiguous, `None` when
/// the requirement maps cleanly to a single compat key.
pub fn audit_range_capability_ambiguity(
    dependency_name: &str,
    requirement_str: &str,
    provider_name: Option<&str>,
    manifest_path: Option<&str>,
    generated_capability: Option<&str>,
) -> Option<RangeWarning> {
    let requirement_str = requirement_str.trim();

    // Parse the version requirement.
    let req = match VersionReq::parse(requirement_str) {
        Ok(r) => r,
        Err(_) => {
            // Unparsable requirement – conservatively warn.
            return Some(RangeWarning {
                manifest: manifest_path.map(String::from),
                provider: provider_name.map(String::from),
                dependency: dependency_name.to_string(),
                requirement: requirement_str.to_string(),
                generated_capability: generated_capability.map(String::from),
                reason: "generated lower-bound capability may not match Cargo lock selected version because the requirement cannot be parsed".to_string(),
            });
        }
    };

    // Star / wildcard – too broad.
    if requirement_str == "*" || req.comparators.is_empty() {
        return Some(RangeWarning {
            manifest: manifest_path.map(String::from),
            provider: provider_name.map(String::from),
            dependency: dependency_name.to_string(),
            requirement: requirement_str.to_string(),
            generated_capability: generated_capability.map(String::from),
            reason: "generated lower-bound capability may not match Cargo lock selected version because wildcard requirements can select any compat key".to_string(),
        });
    }

    // Determine the effective lower and upper bounds of the requirement.
    let (lower, upper) = match extract_effective_bounds(&req) {
        Some(bounds) => bounds,
        None => {
            // Cannot determine bounds – warn conservatively.
            return Some(RangeWarning {
                manifest: manifest_path.map(String::from),
                provider: provider_name.map(String::from),
                dependency: dependency_name.to_string(),
                requirement: requirement_str.to_string(),
                generated_capability: generated_capability.map(String::from),
                reason: "generated lower-bound capability may not match Cargo lock selected version because the requirement has no clear lower and upper compat bounds".to_string(),
            });
        }
    };

    let lower_compat = compat_key(&lower);
    let upper_compat = compat_key(&upper);

    if lower_compat == upper_compat {
        // Single compat key – safe.
        None
    } else {
        let crate_base = normalize_crate_name(dependency_name);
        let lower_capability = format!("{}-{}", crate_base, lower_compat);
        let upper_capability = format!("{}-{}", crate_base, upper_compat);
        let reason = format!(
            "requirement may also be satisfied by {}, but RPM capability {} cannot be satisfied by {}",
            upper_capability, lower_capability, upper_capability
        );
        Some(RangeWarning {
            manifest: manifest_path.map(String::from),
            provider: provider_name.map(String::from),
            dependency: dependency_name.to_string(),
            requirement: requirement_str.to_string(),
            generated_capability: generated_capability.map(String::from),
            reason,
        })
    }
}

/// Extract the effective [lower, upper) bounds of a `VersionReq` and return
/// the inclusive lower bound and the highest satisfiable version (upper − ε).
///
/// Returns `None` if bounds cannot be determined (e.g. only upper bounds
/// without a lower bound).
fn extract_effective_bounds(req: &VersionReq) -> Option<(Version, Version)> {
    let mut lower: Option<Version> = None;
    let mut upper: Option<Version> = None;

    for comp in &req.comparators {
        match comp.op {
            // Operators that establish a lower bound
            Op::Exact => {
                let lo = comparator_to_version(comp);
                let hi = exact_upper_bound(comp);
                update_lower(&mut lower, &lo);
                update_upper(&mut upper, &hi);
            }
            Op::GreaterEq => {
                let lo = comparator_to_version(comp);
                update_lower(&mut lower, &lo);
            }
            Op::Greater => {
                // > X.Y.Z means >= X.Y.(Z+1) for our purposes
                let lo = comparator_to_version_incremented(comp);
                update_lower(&mut lower, &lo);
            }
            // Operators that establish an upper bound
            Op::Less => {
                let hi = comparator_to_version(comp);
                // upper exclusive: the highest satisfiable version is hi − ε.
                // For compat-key purposes we check hi − one patch.
                let hi_inclusive = version_decrement(&hi)?;
                update_upper_inclusive(&mut upper, &hi_inclusive);
            }
            Op::LessEq => {
                let hi = comparator_to_version(comp);
                update_upper_inclusive(&mut upper, &hi);
            }
            // Caret (default Cargo behaviour)
            Op::Caret => {
                let lo = comparator_to_version(comp);
                let hi = caret_upper_bound(comp);
                update_lower(&mut lower, &lo);
                // hi is exclusive upper bound
                let hi_inclusive = version_decrement(&hi)?;
                update_upper_inclusive(&mut upper, &hi_inclusive);
            }
            // Tilde
            Op::Tilde => {
                let lo = comparator_to_version(comp);
                let hi = tilde_upper_bound(comp);
                update_lower(&mut lower, &lo);
                let hi_inclusive = version_decrement(&hi)?;
                update_upper_inclusive(&mut upper, &hi_inclusive);
            }
            // Wildcard with partial version, e.g. 1.* or 1.2.*
            Op::Wildcard => {
                let lo = comparator_to_version(comp);
                let hi = wildcard_upper_bound(comp);
                update_lower(&mut lower, &lo);
                let hi_inclusive = version_decrement(&hi)?;
                update_upper_inclusive(&mut upper, &hi_inclusive);
            }
            _ => return None,
        }
    }

    match (lower, upper) {
        (Some(lo), Some(hi)) => Some((lo, hi)),
        (Some(_lo), None) => {
            // No upper bound – conservatively return None so we warn.
            // Exception: a single caret or tilde would have set upper too,
            // so this means truly unbounded (e.g. ">= 1.0").
            None
        }
        _ => None,
    }
}

fn comparator_to_version(comp: &Comparator) -> Version {
    Version::new(comp.major, comp.minor.unwrap_or(0), comp.patch.unwrap_or(0))
}

fn comparator_to_version_incremented(comp: &Comparator) -> Version {
    // Increment the least-specified component.
    match (comp.minor, comp.patch) {
        (Some(minor), Some(patch)) => Version::new(comp.major, minor, patch + 1),
        (Some(minor), None) => Version::new(comp.major, minor + 1, 0),
        _ => Version::new(comp.major + 1, 0, 0),
    }
}

fn exact_upper_bound(comp: &Comparator) -> Version {
    // An exact match like =1.2.3 only matches 1.2.3.
    comparator_to_version(comp)
}

fn caret_upper_bound(comp: &Comparator) -> Version {
    let major = comp.major;
    let minor = comp.minor.unwrap_or(0);
    let patch = comp.patch.unwrap_or(0);

    if major > 0 {
        Version::new(major + 1, 0, 0)
    } else if comp.minor.is_none() {
        Version::new(1, 0, 0)
    } else if minor > 0 {
        Version::new(0, minor + 1, 0)
    } else if comp.patch.is_none() {
        Version::new(0, 1, 0)
    } else {
        Version::new(0, 0, patch + 1)
    }
}

fn tilde_upper_bound(comp: &Comparator) -> Version {
    let major = comp.major;
    let minor = comp.minor.unwrap_or(0);

    match comp.patch {
        Some(_) => Version::new(major, minor + 1, 0),
        None => match comp.minor {
            Some(_) => Version::new(major, minor + 1, 0),
            None => Version::new(major + 1, 0, 0),
        },
    }
}

fn wildcard_upper_bound(comp: &Comparator) -> Version {
    match (comp.minor, comp.patch) {
        (Some(minor), Some(_)) => Version::new(comp.major, minor + 1, 0),
        (Some(_), None) => Version::new(comp.major + 1, 0, 0),
        _ => Version::new(comp.major + 1, 0, 0),
    }
}

fn version_decrement(v: &Version) -> Option<Version> {
    if v.patch > 0 {
        Some(Version::new(v.major, v.minor, v.patch - 1))
    } else if v.minor > 0 {
        // 1.2.0 → 1.1.MAX; for compat-key purposes 1.1.9999 is fine
        Some(Version::new(v.major, v.minor - 1, 9999))
    } else if v.major > 0 {
        Some(Version::new(v.major - 1, 9999, 9999))
    } else {
        // Cannot go below 0.0.0
        None
    }
}

fn update_lower(current: &mut Option<Version>, candidate: &Version) {
    match current {
        Some(ref cur) if candidate > cur => *current = Some(candidate.clone()),
        None => *current = Some(candidate.clone()),
        _ => {}
    }
}

fn update_upper(current: &mut Option<Version>, candidate: &Version) {
    // For exclusive upper bound, convert to inclusive by decrementing
    match current {
        Some(ref cur) if candidate < cur => *current = Some(candidate.clone()),
        None => *current = Some(candidate.clone()),
        _ => {}
    }
}

fn update_upper_inclusive(current: &mut Option<Version>, candidate: &Version) {
    match current {
        Some(ref cur) if candidate < cur => *current = Some(candidate.clone()),
        None => *current = Some(candidate.clone()),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Cargo.toml scanning
// ---------------------------------------------------------------------------

/// Scan a Cargo.toml file and return range warnings for all dependencies.
pub fn scan_cargo_toml(
    cargo_toml_path: &Path,
    provider_name: Option<&str>,
) -> anyhow::Result<Vec<RangeWarning>> {
    let mut visited = BTreeSet::new();
    scan_cargo_toml_with_visited(cargo_toml_path, provider_name, &mut visited)
}

fn scan_cargo_toml_with_visited(
    cargo_toml_path: &Path,
    provider_name: Option<&str>,
    visited: &mut BTreeSet<PathBuf>,
) -> anyhow::Result<Vec<RangeWarning>> {
    let visit_key =
        std::fs::canonicalize(cargo_toml_path).unwrap_or_else(|_| cargo_toml_path.to_path_buf());
    if !visited.insert(visit_key) {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(cargo_toml_path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {}", cargo_toml_path.display(), e))?;
    let manifest: toml::Value = toml::from_str(&content)
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {}", cargo_toml_path.display(), e))?;

    let manifest_str = cargo_toml_path.to_string_lossy().to_string();
    let mut warnings = Vec::new();

    let dep_tables = collect_dependency_tables(&manifest);

    for (dep_name, version_req) in dep_tables {
        if let Some(w) = audit_range_capability_ambiguity(
            &dep_name,
            &version_req,
            provider_name,
            Some(&manifest_str),
            None, // No generated capability at scan time
        ) {
            warnings.push(w);
        }
    }

    let manifest_dir = cargo_toml_path.parent().unwrap_or_else(|| Path::new("."));
    for path_dep in collect_dependency_paths(&manifest) {
        let member_toml = manifest_dir.join(path_dep).join("Cargo.toml");
        if member_toml.exists() {
            warnings.extend(scan_cargo_toml_with_visited(
                &member_toml,
                provider_name,
                visited,
            )?);
        }
    }

    Ok(warnings)
}

/// Collect all (dependency_name, version_requirement) pairs from a parsed
/// Cargo.toml manifest, including `[dependencies]`, `[build-dependencies]`,
/// `[dev-dependencies]`, and `[target.*.dependencies]` tables.
fn collect_dependency_tables(manifest: &toml::Value) -> Vec<(String, String)> {
    let mut deps = Vec::new();

    // Standard dependency sections
    for section in &["dependencies", "build-dependencies", "dev-dependencies"] {
        if let Some(table) = manifest.get(section).and_then(toml::Value::as_table) {
            collect_deps_from_table(table, &mut deps);
        }
    }

    // Target-specific dependency sections: [target."cfg(...)".dependencies]
    if let Some(target) = manifest.get("target").and_then(toml::Value::as_table) {
        for (_target_spec, target_value) in target {
            if let Some(target_table) = target_value.as_table() {
                for section in &["dependencies", "build-dependencies", "dev-dependencies"] {
                    if let Some(table) = target_table.get(*section).and_then(toml::Value::as_table)
                    {
                        collect_deps_from_table(table, &mut deps);
                    }
                }
            }
        }
    }

    deps
}

/// Extract (name, version) pairs from a TOML dependency table.
fn collect_deps_from_table(
    table: &toml::map::Map<String, toml::Value>,
    out: &mut Vec<(String, String)>,
) {
    for (name, value) in table {
        let version = match value {
            toml::Value::String(v) => Some(v.clone()),
            toml::Value::Table(t) => t
                .get("version")
                .and_then(toml::Value::as_str)
                .map(String::from),
            _ => None,
        };
        if let Some(v) = version {
            out.push((name.clone(), v));
        }
    }
}

fn collect_dependency_paths(manifest: &toml::Value) -> Vec<String> {
    let mut paths = Vec::new();

    for table in dependency_tables(manifest) {
        for value in table.values() {
            if let toml::Value::Table(t) = value {
                if let Some(path) = t.get("path").and_then(toml::Value::as_str) {
                    paths.push(path.to_string());
                }
            }
        }
    }

    paths.sort();
    paths.dedup();
    paths
}

fn dependency_tables<'a>(
    manifest: &'a toml::Value,
) -> Vec<&'a toml::map::Map<String, toml::Value>> {
    let mut tables = Vec::new();

    for section in &["dependencies", "build-dependencies", "dev-dependencies"] {
        if let Some(table) = manifest.get(section).and_then(toml::Value::as_table) {
            tables.push(table);
        }
    }

    if let Some(target) = manifest.get("target").and_then(toml::Value::as_table) {
        for target_value in target.values() {
            if let Some(target_table) = target_value.as_table() {
                for section in &["dependencies", "build-dependencies", "dev-dependencies"] {
                    if let Some(table) = target_table.get(*section).and_then(toml::Value::as_table)
                    {
                        tables.push(table);
                    }
                }
            }
        }
    }

    tables
}

/// Scan a directory for Cargo.toml files (handles workspace with members).
pub fn scan_directory(dir: &Path) -> anyhow::Result<Vec<RangeWarning>> {
    let cargo_toml = if dir.is_file() {
        dir.to_path_buf()
    } else {
        dir.join("Cargo.toml")
    };

    if !cargo_toml.exists() {
        anyhow::bail!("Cargo.toml not found at: {}", cargo_toml.display());
    }

    let provider_name = provider_name_from_input(dir);

    let mut all_warnings = Vec::new();
    let mut visited = BTreeSet::new();

    // Scan the root Cargo.toml
    let warnings =
        scan_cargo_toml_with_visited(&cargo_toml, provider_name.as_deref(), &mut visited)?;
    all_warnings.extend(warnings);

    // Check if it's a workspace and scan member Cargo.tomls
    let content = std::fs::read_to_string(&cargo_toml)?;
    if let Ok(manifest) = toml::from_str::<toml::Value>(&content) {
        if let Some(workspace) = manifest.get("workspace").and_then(toml::Value::as_table) {
            if let Some(members) = workspace.get("members").and_then(toml::Value::as_array) {
                let base_dir = if dir.is_file() {
                    dir.parent().unwrap_or(dir)
                } else {
                    dir
                };
                for member in members.iter().filter_map(toml::Value::as_str) {
                    // Handle glob patterns simply
                    if member.contains('*') {
                        if let Ok(pattern) = glob::glob(&base_dir.join(member).to_string_lossy()) {
                            for entry in pattern.flatten() {
                                let member_toml = entry.join("Cargo.toml");
                                if member_toml.exists() {
                                    if let Ok(ws) = scan_cargo_toml_with_visited(
                                        &member_toml,
                                        provider_name.as_deref(),
                                        &mut visited,
                                    ) {
                                        all_warnings.extend(ws);
                                    }
                                }
                            }
                        }
                    } else {
                        let member_toml = base_dir.join(member).join("Cargo.toml");
                        if member_toml.exists() {
                            if let Ok(ws) = scan_cargo_toml_with_visited(
                                &member_toml,
                                provider_name.as_deref(),
                                &mut visited,
                            ) {
                                all_warnings.extend(ws);
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(all_warnings)
}

fn provider_name_from_input(path: &Path) -> Option<String> {
    if path.is_file() {
        path.parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            .map(String::from)
    } else {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(String::from)
    }
}

// ---------------------------------------------------------------------------
// Integration: audit from structured Cargo Dependency objects
// ---------------------------------------------------------------------------

/// Audit a set of Cargo [`Dependency`] objects and return range warnings.
///
/// This is the entry point used by `package` / `localpkg` spec generation.
pub fn audit_cargo_dependencies(
    deps: &[cargo::core::Dependency],
    provider_name: Option<&str>,
) -> Vec<RangeWarning> {
    let mut warnings = Vec::new();

    for dep in deps {
        if !dependency_is_runtime_candidate(dep, false) {
            continue;
        }

        let req_str = dep.version_req().to_string();
        if req_str == "*" || req_str.is_empty() {
            if let Some(w) = audit_range_capability_ambiguity(
                dep.package_name().as_str(),
                &req_str,
                provider_name,
                None,
                None,
            ) {
                warnings.push(w);
            }
            continue;
        }

        // Build the generated capability string for context
        let lower_bound = lower_bound_version_string(&req_str);
        let generated_cap = lower_bound.as_deref().map(|lb| {
            let crate_base = normalize_crate_name(dep.package_name().as_str());
            if lb.contains('-') {
                format!("crate({}-{})", crate_base, lb)
            } else if let Ok(ver) = Version::parse(lb) {
                format!("crate({}-{})", crate_base, calculate_compat_version(&ver))
            } else {
                format!("crate({})", crate_base)
            }
        });

        if let Some(w) = audit_range_capability_ambiguity(
            dep.package_name().as_str(),
            &req_str,
            provider_name,
            None,
            generated_cap.as_deref(),
        ) {
            warnings.push(w);
        }
    }

    warnings
}

/// Extract a simple lower bound version string from a version requirement for
/// generating the capability context string.
fn lower_bound_version_string(req_str: &str) -> Option<String> {
    let req = VersionReq::parse(req_str).ok()?;
    req.comparators
        .iter()
        .filter_map(|comp| match comp.op {
            Op::Exact | Op::GreaterEq | Op::Tilde | Op::Caret | Op::Wildcard => Some(format!(
                "{}.{}.{}",
                comp.major,
                comp.minor.unwrap_or(0),
                comp.patch.unwrap_or(0)
            )),
            Op::Greater => Some(format!(
                "{}.{}.{}",
                comp.major,
                comp.minor.unwrap_or(0),
                comp.patch.unwrap_or(0)
            )),
            _ => None,
        })
        .max()
}

// ---------------------------------------------------------------------------
// Warning output helpers
// ---------------------------------------------------------------------------

/// Print range warnings to stderr according to the given policy.
///
/// Returns `true` if the caller should abort (policy = Error and warnings exist).
pub fn emit_warnings(warnings: &[RangeWarning], policy: RangeCapabilityPolicy) -> bool {
    if warnings.is_empty() || policy == RangeCapabilityPolicy::Allow {
        return false;
    }

    for w in warnings {
        if policy == RangeCapabilityPolicy::Error {
            eprintln!(
                "{}",
                format!("{}", w).replace("warning: range dependency", "error: range dependency")
            );
        } else {
            eprintln!("{}", w);
        }
        eprintln!();
    }

    policy == RangeCapabilityPolicy::Error
}

// ---------------------------------------------------------------------------
// JSON output
// ---------------------------------------------------------------------------

/// JSON wrapper for range-audit output.
#[derive(serde_derive::Serialize)]
pub struct RangeAuditReport {
    pub warnings: Vec<RangeWarning>,
}

impl RangeAuditReport {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use cargo::core::{EitherManifest, SourceId};
    use cargo::util::toml::read_manifest;
    use cargo::GlobalContext;
    use std::fs;

    fn check(req: &str) -> Option<RangeWarning> {
        audit_range_capability_ambiguity("testcrate", req, None, None, None)
    }

    fn manifest_from_toml(toml: &str) -> cargo::core::Manifest {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("src")).unwrap();
        fs::write(temp.path().join("src/lib.rs"), "pub fn marker() {}\n").unwrap();
        let cargo_toml = temp.path().join("Cargo.toml");
        fs::write(&cargo_toml, toml).unwrap();
        let source_id = SourceId::for_path(temp.path()).unwrap();
        match read_manifest(&cargo_toml, source_id, &GlobalContext::default().unwrap()).unwrap() {
            EitherManifest::Real(manifest) => manifest,
            _ => panic!("expected real manifest"),
        }
    }

    #[test]
    fn caret_single_minor_no_warning() {
        // "0.9" is caret 0.9 -> >=0.9.0, <0.10.0 -> only in 0.9
        assert!(check("0.9").is_none(), "0.9 should not warn");
    }

    #[test]
    fn explicit_range_within_compat_no_warning() {
        // ">=0.9,<0.10" -> only in 0.9
        assert!(
            check(">=0.9, <0.10").is_none(),
            ">=0.9,<0.10 should not warn"
        );
        assert!(check(">=0.9.0, <0.10.0").is_none());
    }

    #[test]
    fn range_crosses_minor_compat_warns() {
        // ">=0.9,<0.11" -> spans 0.9 and 0.10
        let w = check(">=0.9, <0.11");
        assert!(w.is_some(), ">=0.9,<0.11 should warn");
        let w = w.unwrap();
        assert!(w.reason.contains("0.9"), "should mention 0.9");
        assert!(w.reason.contains("0.10"), "should mention 0.10");
    }

    #[test]
    fn range_crosses_minor_windows_warns() {
        // ">=0.61,<0.63" -> spans 0.61 and 0.62
        let w = check(">=0.61.0, <0.63.0");
        assert!(w.is_some(), ">=0.61,<0.63 should warn");

        let w = audit_range_capability_ambiguity(
            "windows",
            ">=0.61, <0.63",
            Some("rust-reflink-copy-0.1"),
            None,
            Some("crate(windows-0.61)"),
        )
        .unwrap();
        assert!(w.reason.contains("windows-0.61"));
        assert!(w.reason.contains("windows-0.62"));
    }

    #[test]
    fn major_single_no_warning() {
        // ">=1,<2" -> only in major 1
        assert!(check(">=1, <2").is_none(), ">=1,<2 should not warn");
        // Caret "1" -> >=1.0.0, <2.0.0 -> only in major 1
        assert!(check("1").is_none(), "1 should not warn");
    }

    #[test]
    fn major_crosses_warns() {
        // ">=1,<3" -> spans major 1 and 2
        let w = check(">=1, <3");
        assert!(w.is_some(), ">=1,<3 should warn");
    }

    #[test]
    fn wildcard_warns() {
        let w = check("*");
        assert!(w.is_some(), "* should warn");
    }

    #[test]
    fn caret_major_no_warning() {
        // "^1.5" -> >=1.5.0, <2.0.0 -> only in major 1
        assert!(check("^1.5").is_none());
    }

    #[test]
    fn tilde_no_warning() {
        // "~0.9.3" -> >=0.9.3, <0.10.0 -> only in 0.9
        assert!(check("~0.9.3").is_none());
    }

    #[test]
    fn exact_no_warning() {
        // "=1.2.3" -> only 1.2.3 -> only in major 1
        assert!(check("=1.2.3").is_none());
    }

    #[test]
    fn range_0_0_x_no_warning() {
        // ">=0.0.3, <0.0.4" -> only in 0.0.3 compat
        assert!(check(">=0.0.3, <0.0.4").is_none());
        assert!(check("0.0.6").is_none());
    }

    #[test]
    fn range_0_0_x_crosses_warns() {
        // ">=0.0.3, <0.0.5" -> spans 0.0.3 and 0.0.4
        let w = check(">=0.0.3, <0.0.5");
        assert!(w.is_some(), ">=0.0.3,<0.0.5 should warn (0.0.x compat)");

        let w = check("0.0");
        assert!(
            w.is_some(),
            "0.0 should warn because it spans 0.0.x compat keys"
        );

        let w = check(">=0.0, <0.1");
        assert!(w.is_some(), ">=0.0,<0.1 should warn");
    }

    #[test]
    fn range_wide_warns() {
        // ">=0.6, <8" -> spans many compat keys
        let w = check(">=0.6, <8");
        assert!(w.is_some(), ">=0.6,<8 should warn");
    }

    #[test]
    fn serde_style_no_warning() {
        // serde = "1" (caret) -> >=1.0.0, <2.0.0
        assert!(check("1").is_none());
        // serde = ">=1.0, <2.0"
        assert!(check(">=1.0, <2.0").is_none());
        assert!(check(">=1.0.0, <2.0.0").is_none());
    }

    #[test]
    fn warning_display_includes_required_guidance() {
        let warning = audit_range_capability_ambiguity(
            "goblin",
            ">=0.9, <0.11",
            Some("rust-pyo3-introspection-0.28"),
            Some("/tmp/Cargo.toml"),
            Some("crate(goblin-0.9)"),
        )
        .unwrap();
        let rendered = warning.to_string();

        assert!(rendered.contains("provider: rust-pyo3-introspection-0.28"));
        assert!(rendered.contains("dependency: goblin"));
        assert!(rendered.contains("requirement: >=0.9, <0.11"));
        assert!(rendered.contains("reason: requirement spans multiple RPM crate capability keys"));
        assert!(rendered.contains("risk: requirement may also be satisfied by goblin-0.10"));
        assert!(rendered.contains("recommendation: patch Cargo.toml to the lock-selected version"));
    }

    #[test]
    fn scan_cargo_toml_sees_target_specific_dependency() {
        let temp = tempfile::tempdir().unwrap();
        let cargo_toml = temp.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"
[package]
name = "fixture"
version = "0.1.0"
edition = "2021"

[target.'cfg(windows)'.dependencies.windows]
version = ">=0.61,<0.63"
"#,
        )
        .unwrap();

        let warnings = scan_cargo_toml(&cargo_toml, Some("rust-fixture-0.1")).unwrap();

        assert!(warnings.iter().any(|w| w.dependency == "windows"));
    }

    #[test]
    fn scan_directory_sees_path_member_dependency() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("member/src")).unwrap();
        fs::write(
            temp.path().join("Cargo.toml"),
            r#"
[package]
name = "fixture"
version = "0.1.0"
edition = "2021"

[dependencies]
member = { path = "member" }
"#,
        )
        .unwrap();
        fs::write(
            temp.path().join("member/Cargo.toml"),
            r#"
[package]
name = "member"
version = "0.1.0"
edition = "2021"

[dependencies]
goblin = ">=0.9,<0.11"
"#,
        )
        .unwrap();
        fs::write(
            temp.path().join("member/src/lib.rs"),
            "pub fn marker() {}\n",
        )
        .unwrap();

        let warnings = scan_directory(temp.path()).unwrap();

        assert!(warnings.iter().any(|w| w.dependency == "goblin"));
    }

    #[test]
    fn provider_dependency_audit_skips_dev_dependencies() {
        let manifest = manifest_from_toml(
            r#"
[package]
name = "fixture"
version = "0.1.0"
edition = "2021"

[dev-dependencies]
goblin = ">=0.9,<0.11"
"#,
        );

        let warnings = audit_cargo_dependencies(manifest.dependencies(), Some("rust-fixture-0.1"));

        assert!(warnings.is_empty());
    }

    #[test]
    fn provider_dependency_audit_includes_target_normal_dependencies() {
        let manifest = manifest_from_toml(
            r#"
[package]
name = "fixture"
version = "0.1.0"
edition = "2021"

[target.'cfg(windows)'.dependencies.windows]
version = ">=0.61,<0.63"
"#,
        );

        let warnings = audit_cargo_dependencies(manifest.dependencies(), Some("rust-fixture-0.1"));

        assert!(warnings.iter().any(|w| w.dependency == "windows"));
    }
}
