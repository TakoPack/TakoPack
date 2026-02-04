use anyhow::{Context, Result};
use semver::Version;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::lockfile_parser::DependencyGraph;

/// Get the default database path: ~/.config/takopack/crate_db.txt
pub fn get_default_database_path() -> PathBuf {
    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("takopack");

    // Create directory if it doesn't exist
    std::fs::create_dir_all(&config_dir).ok();

    config_dir.join("crate_db.txt")
}

#[cfg(feature = "back_db")]
mod git_helper {
    use std::path::Path;
    use std::process::Command;

    /// Check if git is available on the system
    pub fn is_git_available() -> bool {
        Command::new("git").arg("--version").output().is_ok()
    }

    /// Initialize git repo if it doesn't exist
    pub fn init_repo_if_needed(dir: &Path) -> bool {
        if !dir.join(".git").exists() {
            Command::new("git")
                .args(&["init"])
                .current_dir(dir)
                .output()
                .is_ok()
        } else {
            true
        }
    }

    /// Commit the database file with a message
    pub fn commit_file(dir: &Path, file: &str, message: &str) -> bool {
        // Add file
        let add_ok = Command::new("git")
            .args(&["add", file])
            .current_dir(dir)
            .output()
            .is_ok();

        if !add_ok {
            return false;
        }

        // Commit (might fail if no changes, that's ok)
        Command::new("git")
            .args(&["commit", "-m", message])
            .current_dir(dir)
            .output()
            .is_ok()
    }
}

/// Entry for a single crate in the database
/// TODO: If a crate does not follow Rust’s compatibility rules,
/// then it should not cause trouble for the database either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrateEntry {
    /// Crate name (keeps original formatting, no _ or - conversion)
    pub name: String,
    /// Full version
    pub version: Version,
    /// Whether this version follows standard Rust compatibility rules
    /// false = incompatible (has build metadata or pre-release)
    pub compatible: bool,
}
// TODO: Only deps like [dependencies.libbpf-rs] version = "=0.26.0-beta.1"
// the version must be full version string.
// The other likes 0.23.10+spec-1.0.0,just handle as normal.
impl CrateEntry {
    /// Create a new CrateEntry with automatic compatibility detection
    pub fn new(name: String, version: Version) -> Self {
        let compatible = Self::is_standard_version(&version);
        Self {
            name,
            version,
            compatible,
        }
    }

    /// Check if version is a standard release (no build metadata or pre-release)
    fn is_standard_version(version: &Version) -> bool {
        // 2026.01.24 only have pre is not standard.
        version.pre.is_empty()
        // version.build.is_empty() && version.pre.is_empty()
    }

    /// Calculate the compatibility version string
    /// For compatible versions: "0.x" or "major.0"
    /// For incompatible versions: full version string
    pub fn compat_version(&self) -> String {
        crate::util::calculate_compat_version(&self.version)
    }

    /// Get the unique key for this crate entry
    /// Format: "name@compat_version"
    pub fn key(&self) -> String {
        format!("{}@{}", self.name, self.compat_version())
    }

    /// Parse from text line format: "crate-name version [false]"
    pub fn from_line(line: &str) -> Result<Self> {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            anyhow::bail!("Empty or comment line");
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            anyhow::bail!("Invalid line format: {}", line);
        }

        let name = parts[0].to_string();
        let version_str = parts[1];
        let compatible = parts.get(2).map_or(true, |s| *s != "false");

        let version = Version::parse(version_str).with_context(|| {
            format!(
                "Failed to parse version '{}' for crate '{}'",
                version_str, name
            )
        })?;

        Ok(Self {
            name,
            version,
            compatible,
        })
    }

    /// Convert to text line format: "crate-name version [false]"
    pub fn to_line(&self) -> String {
        if self.compatible {
            format!("{} {}", self.name, self.version)
        } else {
            format!("{} {} false", self.name, self.version)
        }
    }
}

/// Database of crates with version management
#[derive(Debug, Clone)]
pub struct CrateDatabase {
    /// Map from key (name@compat_version) to CrateEntry
    entries: BTreeMap<String, CrateEntry>,
}

impl CrateDatabase {
    /// Create a new empty database
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    /// Create database from a DependencyGraph (from Cargo.lock)
    pub fn from_dependency_graph(dep_graph: &DependencyGraph) -> Self {
        let mut db = Self::new();

        for package in dep_graph.packages() {
            let entry = CrateEntry::new(package.name.clone(), package.version.clone());
            db.add_entry(entry);
        }

        db
    }

    /// Load database from file
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read database file: {}", path.display()))?;

        let mut db = Self::new();

        for (line_num, line) in content.lines().enumerate() {
            match CrateEntry::from_line(line) {
                Ok(entry) => db.add_entry(entry),
                Err(_) => {
                    // Skip empty lines and comments
                    if !line.trim().is_empty() && !line.trim().starts_with('#') {
                        log::warn!("Skipping invalid line {}: {}", line_num + 1, line);
                    }
                }
            }
        }

        Ok(db)
    }

    /// Save database to file
    pub fn to_file(&self, path: &Path) -> Result<()> {
        let mut lines: Vec<String> = self.entries.values().map(|entry| entry.to_line()).collect();

        lines.sort();

        fs::write(path, lines.join("\n") + "\n")
            .with_context(|| format!("Failed to write database file: {}", path.display()))?;

        Ok(())
    }

    /// Commit the database file to git (if back_db feature is enabled and git is available)
    #[cfg(feature = "back_db")]
    pub fn git_commit(path: &Path, commit_message: &str) {
        if let Some(dir) = path.parent() {
            if git_helper::is_git_available() {
                if git_helper::init_repo_if_needed(dir) {
                    let filename = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("crate_db.txt");
                    git_helper::commit_file(dir, filename, commit_message);
                }
            }
        }
    }

    #[cfg(not(feature = "back_db"))]
    pub fn git_commit(_path: &Path, _commit_message: &str) {}

    /// Add a single entry to the database
    pub fn add_entry(&mut self, entry: CrateEntry) {
        let key = entry.key();
        self.entries.insert(key, entry);
    }

    /// Get all entries
    pub fn entries(&self) -> impl Iterator<Item = &CrateEntry> {
        self.entries.values()
    }

    /// Get entry by exact match
    pub fn get(&self, name: &str, version: &Version) -> Option<&CrateEntry> {
        // Try to find by creating a temporary entry
        let temp_entry = CrateEntry::new(name.to_string(), version.clone());
        self.entries.get(&temp_entry.key())
    }

    /// Get the number of entries
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if database is empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Merge another database into this one
    /// Returns a list of crate entries that are new or have higher versions
    pub fn merge(&mut self, other: &CrateDatabase) -> Vec<CrateEntry> {
        let mut needs_action = Vec::new();

        for new_entry in other.entries() {
            let key = new_entry.key();

            match self.entries.get(&key) {
                Some(existing_entry) => {
                    // Entry exists, check if we need to update
                    if new_entry.version > existing_entry.version {
                        // Higher version found, update and mark for action
                        needs_action.push(new_entry.clone());
                        self.entries.insert(key, new_entry.clone());
                    }
                    // Equal or lower version, skip
                }
                None => {
                    // New crate or new incompatible version, add and mark for action
                    needs_action.push(new_entry.clone());
                    self.entries.insert(key, new_entry.clone());
                }
            }
        }

        needs_action
    }

    /// Merge a DependencyGraph into the database
    /// Returns a list of crate entries that need to be processed
    pub fn merge_dependency_graph(&mut self, dep_graph: &DependencyGraph) -> Vec<CrateEntry> {
        let new_db = Self::from_dependency_graph(dep_graph);
        self.merge(&new_db)
    }
}

impl Default for CrateDatabase {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_detection() {
        // Standard version
        let v1 = Version::parse("1.0.0").unwrap();
        assert!(CrateEntry::is_standard_version(&v1));

        // Version with build metadata
        let v2 = Version::parse("0.9.11+spec-1.1.0").unwrap();
        assert!(CrateEntry::is_standard_version(&v2));

        // Pre-release version
        let v3 = Version::parse("1.0.0-beta.1").unwrap();
        assert!(!CrateEntry::is_standard_version(&v3));
    }

    #[test]
    fn test_compat_version() {
        let e1 = CrateEntry::new("serde".to_string(), Version::parse("1.0.0").unwrap());
        assert_eq!(e1.compat_version(), "1.0");
        assert_eq!(e1.key(), "serde@1.0");

        let e2 = CrateEntry::new("toml".to_string(), Version::parse("0.8.23").unwrap());
        assert_eq!(e2.compat_version(), "0.8");
        assert_eq!(e2.key(), "toml@0.8");

        let e3 = CrateEntry::new(
            "toml".to_string(),
            Version::parse("0.9.11+spec-1.1.0").unwrap(),
        );
        assert_eq!(e3.compat_version(), "0.9");
        assert_eq!(e3.key(), "toml@0.9");
    }

    #[test]
    fn test_parse_line() {
        let line1 = "serde 1.0.0";
        let e1 = CrateEntry::from_line(line1).unwrap();
        assert_eq!(e1.name, "serde");
        assert_eq!(e1.version, Version::parse("1.0.0").unwrap());
        assert!(e1.compatible);

        let line2 = "toml 0.9.11+spec-1.1.0 false";
        let e2 = CrateEntry::from_line(line2).unwrap();
        assert_eq!(e2.name, "toml");
        assert!(!e2.compatible);
    }

    #[test]
    fn test_database_merge() {
        let mut db1 = CrateDatabase::new();
        db1.add_entry(CrateEntry::new(
            "serde".to_string(),
            Version::parse("1.0.0").unwrap(),
        ));
        db1.add_entry(CrateEntry::new(
            "toml".to_string(),
            Version::parse("0.8.0").unwrap(),
        ));

        let mut db2 = CrateDatabase::new();
        db2.add_entry(CrateEntry::new(
            "serde".to_string(),
            Version::parse("1.0.200").unwrap(),
        ));
        db2.add_entry(CrateEntry::new(
            "anyhow".to_string(),
            Version::parse("1.0.0").unwrap(),
        ));

        let needs_action = db1.merge(&db2);

        assert_eq!(needs_action.len(), 2); // Updated serde and new anyhow
        assert_eq!(db1.len(), 3); // serde, toml, anyhow
    }
}
