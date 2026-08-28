//! Refactored crate-loading pipeline (`CrateSource -> CrateModel`).
//!
//! This is an additive, self-contained reimplementation of the crate
//! normalization layer. It exposes a cargo-free, immutable [`CrateModel`] via
//! the unified [`load_crate`] entry point, keeping all network / filesystem I/O
//! inside the load step and never printing to stdout/stderr.
//!
//! All Rust-specific functionality now lives under this module:
//! - [`crates`] — the new pipeline (`crateinfo`, `error`, `model`, `source`, `crates`).
//! - [`rpm`], [`package`], [`local`], [`registry_sync`], [`resolve_check`],
//!   [`buildreqs`], [`range_audit`] — the original feature modules, moved here
//!   with minimal path adaptation.
//! - [`cli`] — CLI subcommand definitions, gated behind the `cli` feature and
//!   wired by the `takopack` binary (mirrors `takopack-python::cli`).

pub mod buildreqs;
#[cfg(feature = "cli")]
pub mod cli;
pub mod crates;
pub mod local;
pub mod package;
pub mod range_audit;
pub mod registry_sync;
pub mod resolve_check;
pub mod rpm;

// Legacy CrateInfo backend now lives inside `crates::crateinfo`; re-export it
// here so existing call sites can keep using `refactor::crateinfo::CrateInfo`.
pub use crates::crateinfo;

// New pipeline API.
#[cfg(feature = "cli")]
pub use cli::RustSubcommands;
pub use crates::{
    CrateMetadata, CrateModel, CrateSource, DepKind, DepModel, Error, FeatureGraph, FeatureNode,
    LoadOptions, LocalPathSource, LockPackage, ManifestSource, RegistrySource, Result, Source,
    TargetKind, TargetModel, load_crate, transitive_crate_deps,
};
