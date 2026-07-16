use clap::Subcommand;

#[derive(Debug, Clone, Subcommand)]
pub enum PythonSubcommands {
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
