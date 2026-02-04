use clap::{builder::styling::AnsiColor, builder::Styles, Parser, Subcommand};

use crate::{
    package::{PackageExecuteArgs, PackageExtractArgs, PackageInitArgs},
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
    },
    /// Recursively package a crate and all its dependencies (vendor mode)
    #[command(alias = "v")]
    Vendor {
        #[command(flatten)]
        args: RecursivePackageArgs,
    },
    /// Generate spec file from a local Cargo.toml (without downloading)
    #[command(name = "fromtoml", alias = "from")]
    FromToml {
        /// Path to Cargo.toml file
        #[arg(value_name = "CARGO_TOML")]
        toml_path: std::path::PathBuf,

        /// Output directory for generated spec file
        #[arg(short, long, value_name = "DIR")]
        output: Option<std::path::PathBuf>,
    },
    /// Parse Cargo.toml dependencies and recursively generate spec files for all
    #[command(name = "parsetoml", alias = "parse")]
    ParseToml {
        /// Path to Cargo.toml file
        #[arg(value_name = "CARGO_TOML")]
        toml_path: std::path::PathBuf,

        /// Output directory for generated spec files (default: timestamped directory)
        #[arg(short, long, value_name = "DIR")]
        output: Option<std::path::PathBuf>,
    },
    /// Batch process multiple crates from a text file (one crate per line: "crate_name version")
    #[command(name = "batch")]
    Batch {
        /// Path to text file containing crate list (one per line: "name version")
        #[arg(value_name = "FILE")]
        file: std::path::PathBuf,

        /// Output directory for generated spec files (default: timestamped directory)
        #[arg(short, long, value_name = "DIR")]
        output: Option<std::path::PathBuf>,
    },
    /// Package from a local crate directory (with Cargo.toml)
    #[command(name = "localpkg", alias = "local")]
    LocalPackage {
        /// Path to directory containing Cargo.toml (or path to Cargo.toml itself)
        #[arg(value_name = "PATH")]
        path: std::path::PathBuf,

        /// Output directory for generated spec file (default: current directory)
        #[arg(short, long, value_name = "DIR")]
        output: Option<std::path::PathBuf>,

        #[command(flatten)]
        finish: PackageExecuteArgs,
    },
    /// Track dependencies from a crate and generate action list
    #[command(name = "track")]
    #[command(group(
        clap::ArgGroup::new("source")
            .required(true)
            .args(&["crate_name", "from_file"]),
    ))]
    Track {
        /// Crate name
        #[arg(value_name = "CRATE")]
        crate_name: Option<String>,

        /// Crate version (optional, uses latest if not specified)
        #[arg(value_name = "VERSION")]
        version: Option<String>,

        /// Path to Cargo.toml or Cargo.lock file
        #[arg(short = 'f', long, value_name = "FILE")]
        from_file: Option<std::path::PathBuf>,

        /// Output directory for generated specs (default: track_TIMESTAMP/)
        #[arg(short = 'o', long, value_name = "DIR")]
        output: Option<std::path::PathBuf>,

        /// Database file (default: ~/.config/takopack/crate_db.txt)
        #[arg(long, value_name = "FILE")]
        database: Option<std::path::PathBuf>,

        /// Output file for crates that need action (default: needs_action.txt)
        #[arg(long, value_name = "FILE")]
        action_file: Option<std::path::PathBuf>,
    },
}
