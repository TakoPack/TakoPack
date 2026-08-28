//! Cargo-free data model for a single Rust crate.
//!
//! These types deliberately do **not** reference any `cargo::*` types, so that
//! downstream consumers (RPM spec generation, local registry management, etc.)
//! can depend on a stable, immutable view of a crate without being coupled to
//! cargo's internal representation.
//!
//! The model is produced by [`super::crates::load_crate`] and is meant to be
//! treated as read-only after construction.

use std::collections::{BTreeMap, HashSet};

use semver::Version;

/// Crate-level metadata extracted from `Cargo.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CrateMetadata {
    pub description: Option<String>,
    pub license: Option<String>,
    pub license_file: Option<String>,
    pub readme: Option<String>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
}

/// High-level kind of a build target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Lib,
    Bin,
    Other,
}

/// A single build target (lib / bin / example / ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetModel {
    pub name: String,
    pub kind: TargetKind,
    /// Source path relative to the package root, if the target is a real file.
    pub src_path: Option<String>,
}

/// Dependency kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepKind {
    Normal,
    Build,
    Dev,
}

/// A single dependency as declared in `Cargo.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepModel {
    /// Name used on the left-hand side of the dependency declaration.
    pub toml_name: String,
    /// Actual package name (after renaming via `package = "..."`).
    pub package_name: String,
    /// Raw version requirement string.
    pub version_req: String,
    pub kind: DepKind,
    pub optional: bool,
    pub default_features: bool,
    pub features: Vec<String>,
    /// Target-specific restriction, if any (e.g. `cfg(unix)`).
    pub platform: Option<String>,
    /// Whether this dependency enters runtime Requires computation.
    pub runtime_candidate: bool,
}

/// One node in the feature graph.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeatureNode {
    /// Other features (of the same crate) this feature activates.
    pub feature_deps: Vec<String>,
    /// External crate dependencies this feature activates.
    pub crate_deps: Vec<String>,
}

/// Feature graph: feature name -> node.
///
/// The empty string `""` denotes the implicit "base package / no default
/// features" node, mirroring `all_dependencies_and_features`.
pub type FeatureGraph = BTreeMap<String, FeatureNode>;

/// A package recorded in a `Cargo.lock`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockPackage {
    pub name: String,
    pub version: Version,
    pub source: Option<String>,
}

/// Collect every external crate dependency reachable from `feature`
/// (transitively through feature dependencies), deduplicated and sorted.
///
/// This is a pure computation over the projected [`FeatureGraph`] and does not
/// touch cargo.
pub fn transitive_crate_deps(graph: &FeatureGraph, feature: &str) -> Vec<String> {
    let mut visited = HashSet::new();
    let mut out = Vec::new();
    walk(graph, feature, &mut visited, &mut out);
    out.sort();
    out.dedup();
    out
}

fn walk(graph: &FeatureGraph, feature: &str, visited: &mut HashSet<String>, out: &mut Vec<String>) {
    if !visited.insert(feature.to_string()) {
        return;
    }
    let Some(node) = graph.get(feature) else {
        return;
    };
    for dep in &node.crate_deps {
        out.push(dep.clone());
    }
    for child in &node.feature_deps {
        walk(graph, child, visited, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> FeatureGraph {
        let mut g = FeatureGraph::new();
        g.insert(
            "default".to_string(),
            FeatureNode {
                feature_deps: vec!["f1".to_string()],
                crate_deps: vec![],
            },
        );
        g.insert(
            "f1".to_string(),
            FeatureNode {
                feature_deps: vec!["f2".to_string()],
                crate_deps: vec!["alpha".to_string(), "beta".to_string()],
            },
        );
        g.insert(
            "f2".to_string(),
            FeatureNode {
                feature_deps: vec!["f1".to_string()], // cycle
                crate_deps: vec!["alpha".to_string()],
            },
        );
        g
    }

    #[test]
    fn collects_transitive_deps_and_breaks_cycles() {
        let g = graph();
        let deps = transitive_crate_deps(&g, "default");
        assert_eq!(deps, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn unknown_feature_yields_empty() {
        let g = graph();
        assert!(transitive_crate_deps(&g, "nope").is_empty());
    }
}
