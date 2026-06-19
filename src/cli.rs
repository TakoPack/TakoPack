use clap::{builder::styling::AnsiColor, builder::Styles, Parser, Subcommand};

use crate::{
    package::{PackageExecuteArgs, PackageExtractArgs, PackageInitArgs},
    range_audit::RangeCapabilityPolicy,
    recursive_package::RecursivePackageArgs,
};

const CLI_STYLE: Styles = Styles::styled()
    .header(AnsiColor::Yellow.on_default())
    .usage(AnsiColor::Green.on_default())
    .literal(AnsiColor::Green.on_default())
    .placeholder(AnsiColor::Green.on_default());

#[derive(Debug, Clone, Parser)]
#[command(name = "takopack", about = "Package Rust crates for takopack")]
#[command(version)]
#[command(styles = CLI_STYLE)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Opt,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Opt {
    /// Rust/Cargo package operations
    #[command(subcommand)]
    Cargo(CargoOpt),
    /// Python package operations
    #[command(subcommand)]
    Py(PyOpt),
}

#[derive(Debug, Clone, Subcommand)]
pub enum CargoOpt {
    /// Update the crates.io index cache
    #[command(alias = "u")]
    Update,
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
    /// Recursively package a crate and all its dependencies (vendor mode)
    #[command(alias = "v")]
    Vendor {
        #[command(flatten)]
        args: RecursivePackageArgs,
    },
    /// Parse Cargo.toml dependencies and recursively generate spec files for all
    #[command(name = "parsetoml", alias = "parse")]
    ParseToml {
        /// Path to Cargo.toml file
        #[arg(value_name = "CARGO_TOML")]
        toml_path: std::path::PathBuf,

        /// Output root directory. Each package is generated under this root.
        #[arg(short, long, value_name = "OUT_ROOT")]
        output: Option<std::path::PathBuf>,
    },
    /// Batch process multiple crates from a text file (one crate per line: "crate_name version")
    #[command(name = "batch")]
    Batch {
        /// Path to text file containing crate list (one per line: "name version")
        #[arg(value_name = "FILE")]
        file: std::path::PathBuf,

        /// Output root directory. Each package is generated under this root.
        #[arg(short, long, value_name = "OUT_ROOT")]
        output: Option<std::path::PathBuf>,
    },
    /// Package from a local crate directory (with Cargo.toml)
    #[command(name = "localpkg", alias = "local")]
    LocalPackage {
        /// Path to directory containing Cargo.toml (or path to Cargo.toml itself)
        #[arg(value_name = "PATH")]
        path: std::path::PathBuf,

        /// Final output package directory. Files are written directly into this directory.
        #[arg(
            short = 'o',
            long = "directory",
            alias = "output",
            value_name = "OUT_DIR"
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
}

#[derive(Debug, Clone, Subcommand)]
pub enum PyOpt {
    /// Package a Python package from PyPI and generate RPM spec file
    #[command(alias = "pkg")]
    Package {
        /// PyPI package name
        #[arg(value_name = "NAME")]
        name: String,

        /// Package version (optional, latest if omitted)
        #[arg(value_name = "VERSION")]
        version: Option<String>,

        /// Output directory for generated spec folder (default: current directory)
        #[arg(short, long, value_name = "DIR")]
        output: Option<std::path::PathBuf>,
    },
}
