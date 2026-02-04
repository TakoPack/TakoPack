use anyhow::{Context, Result};
use cargo::core::{Resolve, Workspace};
use cargo::ops;
use cargo::util::GlobalContext;
use semver::Version;
use std::collections::BTreeMap;
use std::path::Path;

/// Information about a package in the dependency graph
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PackageInfo {
    /// Package name
    pub name: String,
    /// Package version
    pub version: Version,
    /// Dependencies of this package (name and version)
    pub dependencies: Vec<DependencyInfo>,
}

/// Information about a dependency
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DependencyInfo {
    /// Dependency name
    pub name: String,
    /// Dependency version
    pub version: Version,
}

/// Complete dependency graph parsed from Cargo.lock
#[derive(Debug, Clone)]
pub struct DependencyGraph {
    /// Map from (package_name, version) to PackageInfo
    /// We use BTreeMap to handle multiple versions of the same crate
    packages: BTreeMap<(String, Version), PackageInfo>,
}

impl DependencyGraph {
    /// Create a new empty dependency graph
    pub fn new() -> Self {
        Self {
            packages: BTreeMap::new(),
        }
    }

    /// Add a package to the dependency graph
    pub fn add_package(&mut self, package: PackageInfo) {
        let key = (package.name.clone(), package.version.clone());
        self.packages.insert(key, package);
    }

    /// Get all packages in the dependency graph
    pub fn packages(&self) -> impl Iterator<Item = &PackageInfo> {
        self.packages.values()
    }

    /// Get a specific package by name and version
    pub fn get_package(&self, name: &str, version: &Version) -> Option<&PackageInfo> {
        self.packages.get(&(name.to_string(), version.clone()))
    }

    /// Get all versions of a crate
    pub fn get_versions(&self, name: &str) -> Vec<&Version> {
        self.packages
            .keys()
            .filter(|(pkg_name, _)| pkg_name == name)
            .map(|(_, version)| version)
            .collect()
    }

    /// Get total number of packages (including different versions)
    pub fn len(&self) -> usize {
        self.packages.len()
    }

    /// Check if the dependency graph is empty
    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }

    /// Get dependencies for a specific package as a HashMap
    /// Returns None if package not found
    pub fn get_dependencies_map(
        &self,
        name: &str,
        version: &Version,
    ) -> Option<std::collections::HashMap<String, Version>> {
        self.get_package(name, version).map(|pkg| {
            pkg.dependencies
                .iter()
                .map(|dep| (dep.name.clone(), dep.version.clone()))
                .collect()
        })
    }
}

impl Default for DependencyGraph {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a Cargo.lock file and extract the complete dependency graph
///
/// # Arguments
/// * `lockfile_path` - Path to the Cargo.lock file
///
/// # Returns
/// A DependencyGraph containing all packages and their dependencies
///
/// # Note
/// This function can parse a standalone Cargo.lock file without requiring Cargo.toml
pub fn parse_lockfile(lockfile_path: &Path) -> Result<DependencyGraph> {
    use std::fs;

    if !lockfile_path.exists() {
        anyhow::bail!("Cargo.lock not found at: {:?}", lockfile_path);
    }

    // Read the Cargo.lock file content
    let content = fs::read_to_string(lockfile_path)
        .with_context(|| format!("Failed to read Cargo.lock: {:?}", lockfile_path))?;

    // Parse using cargo's internal TOML parser
    let lockfile: toml::Value = toml::de::from_str(&content)
        .with_context(|| format!("Failed to parse Cargo.lock as TOML: {:?}", lockfile_path))?;

    // Build dependency graph from parsed TOML
    build_dependency_graph_from_toml(&lockfile)
}

/// Build a DependencyGraph from a Resolve
#[allow(unused)]
fn build_dependency_graph(resolve: &Resolve) -> Result<DependencyGraph> {
    let mut graph = DependencyGraph::new();

    // Iterate through all packages in the resolve graph
    for package_id in resolve.iter() {
        let name = package_id.name().to_string();
        let version = package_id.version().clone();

        // Get dependencies for this package
        let mut dependencies = Vec::new();

        // resolve.deps() returns an iterator over (PackageId, &HashSet<Dependency>)
        // The PackageId is the actual resolved dependency with its version
        for (dep_pkg_id, _deps_set) in resolve.deps(package_id) {
            dependencies.push(DependencyInfo {
                name: dep_pkg_id.name().to_string(),
                version: dep_pkg_id.version().clone(),
            });
        }

        // Sort dependencies for consistent output
        dependencies.sort();
        dependencies.dedup();

        let package_info = PackageInfo {
            name,
            version,
            dependencies,
        };

        graph.add_package(package_info);
    }

    Ok(graph)
}

/// Build a DependencyGraph from parsed TOML (Cargo.lock format)
fn build_dependency_graph_from_toml(lockfile: &toml::Value) -> Result<DependencyGraph> {
    use std::collections::HashMap;

    // Get the [[package]] array
    let packages = lockfile
        .get("package")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("Cargo.lock missing 'package' array"))?;

    // First pass: Build a map of package name -> versions
    // Only include packages from crates.io registry
    let mut name_to_versions: HashMap<String, Vec<Version>> = HashMap::new();
    let mut skipped_packages = Vec::new();

    for package in packages {
        let name = package
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Package missing 'name' field"))?;

        let version_str = package
            .get("version")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Package missing 'version' field"))?;

        // Check source - skip non-registry packages
        if let Some(source) = package.get("source").and_then(|v| v.as_str()) {
            if !source.starts_with("registry+") {
                // Skip git, path, and other non-registry sources
                skipped_packages.push(format!("{} {} (source: {})", name, version_str, source));
                continue;
            }
        } else {
            // No source field means it's a workspace member - skip
            continue;
        }

        let version = Version::parse(version_str)
            .with_context(|| format!("Failed to parse version for package '{}'", name))?;

        name_to_versions
            .entry(name.to_string())
            .or_insert_with(Vec::new)
            .push(version);
    }

    // Second pass: Build the dependency graph with resolved versions
    // Only include packages from crates.io registry
    let mut graph = DependencyGraph::new();

    for package in packages {
        let name = package.get("name").and_then(|v| v.as_str()).unwrap();
        let version_str = package.get("version").and_then(|v| v.as_str()).unwrap();

        // Skip non-registry packages (same check as first pass)
        if let Some(source) = package.get("source").and_then(|v| v.as_str()) {
            if !source.starts_with("registry+") {
                continue;
            }
        } else {
            // No source = workspace member, skip
            continue;
        }

        let version = Version::parse(version_str).unwrap();

        // Parse dependencies
        let mut dependencies = Vec::new();
        if let Some(deps_array) = package.get("dependencies").and_then(|v| v.as_array()) {
            for dep in deps_array {
                if let Some(dep_str) = dep.as_str() {
                    // Cargo.lock dependencies format:
                    // - "package_name" (no version = unique package)
                    // - "package_name version" (with version for multiple versions of same package)
                    // Examples: "bitflags 2.10.0", "objc2-foundation 0.2.2"

                    let parts: Vec<&str> = dep_str.split_whitespace().collect();
                    let dep_name = parts[0];

                    // Try to extract version from dependency string
                    let dep_version = if parts.len() > 1 {
                        // Version specified in dep string
                        Version::parse(parts[1]).ok()
                    } else {
                        // No version in string, lookup in map
                        None
                    };

                    // If we got version from string, use it; otherwise lookup in map
                    let dep_version = dep_version.or_else(|| {
                        name_to_versions.get(dep_name).and_then(|versions| {
                            if versions.len() == 1 {
                                Some(versions[0].clone())
                            } else {
                                // Multiple versions exist but none specified in dep string
                                // This shouldn't happen in a valid Cargo.lock
                                // Use max as fallback
                                versions.iter().max().cloned()
                            }
                        })
                    });

                    if let Some(version) = dep_version {
                        dependencies.push(DependencyInfo {
                            name: dep_name.to_string(),
                            version,
                        });
                    }
                }
            }
        }

        dependencies.sort();
        dependencies.dedup();

        let package_info = PackageInfo {
            name: name.to_string(),
            version,
            dependencies,
        };

        graph.add_package(package_info);
    }

    // Report skipped packages
    if !skipped_packages.is_empty() {
        eprintln!(
            "\n⚠ Skipped {} non-registry package(s):",
            skipped_packages.len()
        );
        for pkg in &skipped_packages {
            eprintln!("  - {}", pkg);
        }
        eprintln!();
    }
    Ok(graph)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dependency_graph_creation() {
        let mut graph = DependencyGraph::new();
        assert!(graph.is_empty());
        assert_eq!(graph.len(), 0);

        let package = PackageInfo {
            name: "test-crate".to_string(),
            version: Version::parse("1.0.0").unwrap(),
            dependencies: vec![],
        };

        graph.add_package(package.clone());
        assert_eq!(graph.len(), 1);
        assert!(!graph.is_empty());

        let retrieved = graph.get_package("test-crate", &Version::parse("1.0.0").unwrap());
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().name, "test-crate");
    }

    #[test]
    fn test_multiple_versions() {
        let mut graph = DependencyGraph::new();

        let package_v1 = PackageInfo {
            name: "test-crate".to_string(),
            version: Version::parse("1.0.0").unwrap(),
            dependencies: vec![],
        };

        let package_v2 = PackageInfo {
            name: "test-crate".to_string(),
            version: Version::parse("2.0.0").unwrap(),
            dependencies: vec![],
        };

        graph.add_package(package_v1);
        graph.add_package(package_v2);

        assert_eq!(graph.len(), 2);

        let versions = graph.get_versions("test-crate");
        assert_eq!(versions.len(), 2);
    }
}
