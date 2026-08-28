//! The crate-loading pipeline: [`load_crate`] turns any [`CrateSource`] into
//! an immutable, cargo-free [`CrateModel`].
//!
//! # Design notes / side-effect governance
//!
//! - All network / cache / filesystem I/O is confined to [`load_crate`]. The
//!   projection into [`CrateModel`] and all subsequent graph analysis are pure.
//! - The produced model is immutable: there is no `replace_manifest`, no
//!   mutable internal state, and no shared file-pointer seeking that leaks to
//!   callers.
//! - This module never prints to stdout/stderr. Diagnostics go through `log::`.
//! - The heavy loading implementation lives in [`super::crateinfo`] (the legacy
//!   `CrateInfo` backend), relocated into this module tree so the pipeline is
//!   self-contained. Consumers only ever see the cargo-free [`CrateModel`].

use std::collections::HashMap;

use cargo::core::TargetKind as CargoTargetKind;
use cargo::core::dependency::DepKind as CargoDepKind;
use semver::Version;

use super::crateinfo::{
    CrateInfo, all_dependencies_and_features_filtered, crate_name_ver_to_dep,
    dependency_is_runtime_candidate,
};

use super::error::{Error, Result};
use super::model::{
    CrateMetadata, DepKind, DepModel, FeatureGraph, FeatureNode, TargetKind, TargetModel,
};
use super::source::{CrateSource, PreparedSource};

/// Options that shape how a crate is loaded.
#[derive(Debug, Clone)]
pub struct LoadOptions {
    /// Include dev-dependencies in the dependency list / feature graph.
    pub include_dev_dependencies: bool,
    /// Compute the SHA-256 of the crate file (requires a real `.crate`).
    pub compute_sha256: bool,
    /// Source archive excludes (glob patterns relative to the package root).
    pub excludes: Option<Vec<String>>,
    /// Source archive whitelist (glob patterns).
    pub includes: Option<Vec<String>>,
    /// Optional resolved dependency versions from a `Cargo.lock`.
    pub lockfile_deps: Option<HashMap<String, Version>>,
}

impl Default for LoadOptions {
    fn default() -> Self {
        LoadOptions {
            include_dev_dependencies: false,
            compute_sha256: true,
            excludes: None,
            includes: None,
            lockfile_deps: None,
        }
    }
}

/// An immutable, cargo-free view of a loaded crate.
///
/// Not `Clone`: it may hold a [`tempfile::TempDir`] (for `Local` sources),
/// which cannot be cloned. Treat it as owned and read-only.
#[derive(Debug)]
pub struct CrateModel {
    pub name: String,
    pub version: Version,
    /// TakoPack compat key (e.g. `0.26` or `1`).
    pub semver_compat: String,
    /// Full semver string.
    pub full_version: String,
    pub metadata: CrateMetadata,
    pub targets: Vec<TargetModel>,
    pub is_lib: bool,
    pub binary_targets: Vec<String>,
    pub rust_version: Option<String>,
    pub checksum: Option<String>,
    pub sha256: Option<String>,
    pub dependencies: Vec<DepModel>,
    /// Feature graph keyed by feature name (including `""` base node).
    pub features: FeatureGraph,
    pub lockfile_deps: Option<HashMap<String, Version>>,
    /// Holds the materialized temp crate alive for `Local` sources.
    _workspace: Option<tempfile::TempDir>,
}

impl CrateModel {
    /// All external crate dependencies transitively reachable from `feature`.
    pub fn transitive_deps(&self, feature: &str) -> Vec<String> {
        super::model::transitive_crate_deps(&self.features, feature)
    }
}

/// Load a crate from any supported source into an immutable model.
///
/// Accepts any [`CrateSource`] — either a concrete source struct or the
/// [`Source`] enum newtype.
///
/// # Side effects
/// - [`RegistrySource`]: network access + cargo cache writes.
/// - [`LocalPathSource`]: packages the crate (may resolve dependencies).
/// - [`ManifestSource`]: writes a private tempdir (held alive by the model).
pub fn load_crate(source: impl CrateSource, opts: LoadOptions) -> Result<CrateModel> {
    let prepared = source.prepare()?;
    let (info, workspace) = match prepared {
        PreparedSource::Registry { name, version } => {
            log::debug!("loading registry crate {} {:?}", name, version);
            let dep = crate_name_ver_to_dep(&name, version.as_deref()).map_err(|e| {
                Error::backend(format!("failed to parse dependency for {name}: {e}"))
            })?;
            let info = CrateInfo::new_from_dependency(&dep, true)
                .map_err(|e| Error::backend(format!("failed to load crate {name}: {e}")))?;
            (info, None)
        }
        PreparedSource::LocalPath {
            name,
            version,
            path,
        } => {
            log::debug!("loading local crate path {name} from {:?}", path);
            let info =
                CrateInfo::new_with_local_crate(&name, version.as_deref(), &path).map_err(|e| {
                    Error::backend(format!(
                        "failed to load local crate path {name} from {}: {e}",
                        path.display()
                    ))
                })?;
            (info, None)
        }
        PreparedSource::Manifest {
            manifest,
            workspace,
        } => {
            log::debug!("loading manifest from {:?}", manifest);
            let info = CrateInfo::new_with_local_crate_from_path(&manifest).map_err(|e| {
                Error::backend(format!(
                    "failed to load manifest from {}: {e}",
                    manifest.display()
                ))
            })?;
            (info, Some(workspace))
        }
    };

    let model = project(info, opts, workspace)?;
    Ok(model)
}

/// Project a cargo-backed [`CrateInfo`] into a cargo-free [`CrateModel`].
///
/// This is the only place that reads cargo types; everything downstream is
/// pure. The `_workspace` tempdir is attached to the model so `Local` sources
/// stay valid for the model's lifetime.
fn project(
    info: CrateInfo,
    opts: LoadOptions,
    workspace: Option<tempfile::TempDir>,
) -> Result<CrateModel> {
    let name = info.crate_name().to_string();
    let version = info.version().clone();
    let full_version = version.to_string();
    let semver_compat = info.semver();
    let rust_version = info.rust_version();

    let metadata = {
        let m = info.metadata();
        CrateMetadata {
            description: m.description.clone(),
            license: m.license.clone(),
            license_file: m.license_file.clone(),
            readme: m.readme.clone(),
            homepage: m.homepage.clone(),
            repository: m.repository.clone(),
        }
    };

    let targets = info
        .targets()
        .iter()
        .map(|t| TargetModel {
            name: t.name().to_string(),
            kind: match t.kind() {
                CargoTargetKind::Lib(_) => TargetKind::Lib,
                CargoTargetKind::Bin => TargetKind::Bin,
                _ => TargetKind::Other,
            },
            src_path: t.src_path().path().map(|p| p.display().to_string()),
        })
        .collect::<Vec<_>>();

    let binary_targets = info
        .get_binary_targets()
        .into_iter()
        .map(String::from)
        .collect();

    let dependencies = info
        .dependencies()
        .iter()
        .map(|d| DepModel {
            toml_name: d.name_in_toml().to_string(),
            package_name: d.package_name().to_string(),
            version_req: d.version_req().to_string(),
            kind: match d.kind() {
                CargoDepKind::Normal => DepKind::Normal,
                CargoDepKind::Build => DepKind::Build,
                CargoDepKind::Development => DepKind::Dev,
            },
            optional: d.is_optional(),
            default_features: d.uses_default_features(),
            features: d.features().iter().map(|s| s.to_string()).collect(),
            platform: d.platform().map(|p| p.to_string()),
            runtime_candidate: dependency_is_runtime_candidate(d, opts.include_dev_dependencies),
        })
        .collect::<Vec<_>>();

    let features = {
        let raw =
            all_dependencies_and_features_filtered(info.manifest(), opts.include_dev_dependencies)
                .map_err(|e| Error::backend(format!("failed to compute feature graph: {e}")))?;
        raw.into_iter()
            .map(|(feature, (feature_deps, crate_deps))| {
                (
                    feature.to_string(),
                    FeatureNode {
                        feature_deps: feature_deps.into_iter().map(|s| s.to_string()).collect(),
                        crate_deps: crate_deps
                            .into_iter()
                            .map(|d| d.package_name().to_string())
                            .collect(),
                    },
                )
            })
            .collect::<FeatureGraph>()
    };

    let checksum = info.checksum().map(String::from);
    let is_lib = targets.iter().any(|t| t.kind == TargetKind::Lib);
    let sha256 = if opts.compute_sha256 {
        info.calculate_sha256().ok()
    } else {
        None
    };

    // `info` (and its cargo internals) is dropped here; the model holds only
    // the projected, owned data plus the materialized workspace.
    let _ = info;

    Ok(CrateModel {
        name,
        version,
        semver_compat,
        full_version,
        metadata,
        targets,
        is_lib,
        binary_targets,
        rust_version,
        checksum,
        sha256,
        dependencies,
        features,
        lockfile_deps: opts.lockfile_deps,
        _workspace: workspace,
    })
}
