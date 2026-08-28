//! CLI subcommands for Rust/Cargo packaging.
//!
//! Mirrors the pattern in `takopack-python::cli`: a language crate owns its
//! subcommand enum, and the top-level `takopack` binary wires it in the same
//! way it wires `takopack_python::cli::PythonSubcommands`.

use clap::Subcommand;

use crate::package::{PackageExecuteArgs, PackageExtractArgs, PackageInitArgs};
use crate::range_audit::RangeCapabilityPolicy;

/// Rust/Cargo package operations.
#[derive(Debug, Clone, Subcommand)]
pub enum RustSubcommands {
    /// Package a single Rust crate and generate RPM spec file
    #[command(alias = "pkg")]
    Package {
        #[command(flatten)]
        init: PackageInitArgs,
        #[command(flatten)]
        extract: PackageExtractArgs,
        #[command(flatten)]
        finish: PackageExecuteArgs,
        /// Policy for range-capability warnings (warn|error|allow)
        #[arg(long, value_enum, default_value_t = RangeCapabilityPolicy::Warn)]
        range_capability_policy: RangeCapabilityPolicy,
    },
    /// Package from a local crate directory (with Cargo.toml)
    #[command(name = "localpkg", alias = "local")]
    LocalPackage {
        /// Path to directory containing Cargo.toml (or path to Cargo.toml itself)
        #[arg(value_name = "PATH")]
        path: std::path::PathBuf,

        /// Output root directory. The package directory is created under this root.
        #[arg(
            short = 'o',
            long = "directory",
            alias = "output",
            value_name = "OUT_ROOT"
        )]
        output: Option<std::path::PathBuf>,

        #[command(flatten)]
        finish: PackageExecuteArgs,

        /// Policy for range-capability warnings (warn|error|allow)
        #[arg(long, value_enum, default_value_t = RangeCapabilityPolicy::Warn)]
        range_capability_policy: RangeCapabilityPolicy,
    },
    /// Sync Rust crate providers from ruyispec to local Cargo directory registry
    #[command(name = "registry-sync")]
    RegistrySync {
        /// Only print the sync plan without making changes
        #[arg(long)]
        dry_run: bool,

        /// Number of concurrent crate downloads/extractions
        #[arg(short = 'j', long, default_value_t = 8, value_name = "N")]
        jobs: usize,
    },
    /// Check whether a single crate can resolve against the local TakoPack registry
    #[command(name = "resolve-check")]
    ResolveCheck {
        /// Path to a directory containing Cargo.toml, or a Cargo.toml file
        #[arg(value_name = "PATH")]
        path: std::path::PathBuf,

        /// Local Cargo directory registry. Overrides [registry].local_path in takopack.toml
        #[arg(long, value_name = "DIR")]
        registry: Option<std::path::PathBuf>,
    },
    /// Generate BuildRequires from a single-crate dynamic local-registry resolve
    #[command(name = "buildreqs")]
    BuildReqs {
        /// Path to a directory containing Cargo.toml, or a Cargo.toml file
        #[arg(value_name = "PATH")]
        path: std::path::PathBuf,

        /// Local Cargo directory registry. Overrides [registry].local_path in takopack.toml
        #[arg(long, value_name = "DIR")]
        registry: Option<std::path::PathBuf>,
    },
    /// Load a crate through the new model pipeline and print a summary
    /// (experimental; exercises the refactor API)
    #[command(name = "inspect")]
    Inspect {
        /// A crates.io crate name (with --registry), a local crate directory,
        /// or a Cargo.toml file
        #[arg(value_name = "TARGET")]
        target: String,
        /// Treat `target` as a crates.io crate name (registry source)
        #[arg(long)]
        registry: bool,
        /// Version requirement to use with --registry (e.g. "1", "=1.2.3")
        #[arg(long)]
        version: Option<String>,
    },
}
