//! Refactored crate-loading pipeline (`CrateSource -> CrateModel`).
//!
//! This is an additive, self-contained reimplementation of the crate
//! normalization layer. It exposes a cargo-free, immutable [`CrateModel`] via
//! the unified [`load_crate`] entry point, keeping all network / filesystem I/O
//! inside the load step and never printing to stdout/stderr.
//!
//! The existing modules (`crates`, `local`, `package`, `rpm`, ...) are left
//! untouched; this module can be adopted incrementally.

pub mod crateinfo;
pub mod crates;
pub mod error;
pub mod model;
pub mod source;

// Re-export the legacy CrateInfo backend (the heavy loading implementation),
// plus the cargo-free model types and the pipeline entry point.
pub use crateinfo::{
    CrateInfo, all_dependencies_and_features, all_dependencies_and_features_filtered,
};
pub use crates::{CrateModel, LoadOptions, load_crate};
pub use error::{Error, Result};
pub use model::{
    CrateMetadata, DepKind, DepModel, FeatureGraph, FeatureNode, LockPackage, TargetKind,
    TargetModel, transitive_crate_deps,
};
pub use source::{CrateSource, LocalPathSource, ManifestSource, RegistrySource, Source};
