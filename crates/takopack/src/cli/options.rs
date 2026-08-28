use clap::{Parser, Subcommand, builder::Styles, builder::styling::AnsiColor};

use takopack_python::cli::PythonSubcommands;
use takopack_rust::RustSubcommands;

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
    Cargo(RustSubcommands),
    /// Python package operations
    #[command(subcommand)]
    Py(PythonSubcommands),
}
