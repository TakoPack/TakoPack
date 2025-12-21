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
}
