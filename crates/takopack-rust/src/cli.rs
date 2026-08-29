//! CLI subcommands for Rust/Cargo packaging.
//!
//! Mirrors the pattern in `takopack-python::cli`: a language crate owns its
//! subcommand enum, and the top-level `takopack` binary wires it in the same
//! way it wires `takopack_python::cli::PythonSubcommands`.

use clap::Subcommand;

use crate::package::{PackageExecuteArgs, PackageExtractArgs, PackageInitArgs, PackageProcess};
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

pub fn inspect_crate(
    target: &str,
    registry: bool,
    version: Option<String>,
) -> Result<i32, CliError> {
    use crate::crates::{
        CrateSource, LoadOptions, LocalPathSource, ManifestSource, RegistrySource, Source,
        load_crate,
    };
    use std::path::Path;

    // Pick a source based on how `target` was passed.
    let source = if registry {
        Source::Registry(RegistrySource {
            name: target.to_string(),
            version,
        })
    } else {
        let p = Path::new(target);
        let is_manifest = p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("toml");
        if is_manifest {
            Source::Manifest(ManifestSource {
                path: p.to_path_buf(),
            })
        } else {
            Source::LocalPath(LocalPathSource {
                path: p.to_path_buf(),
                name: None,
                version: None,
            })
        }
    };

    log::info!(
        "inspecting crate via new model pipeline: {}",
        source.describe()
    );
    let model = load_crate(source, LoadOptions::default()).map_err(|_| CliError::Inspect)?;

    println!("Crate: {} {}", model.name, model.full_version);
    println!("compat: {}", model.semver_compat);
    println!("is_lib: {}", model.is_lib);
    println!("binary targets: {:?}", model.binary_targets);
    println!("targets:");
    for t in &model.targets {
        println!(
            "  - {} ({:?}) {}",
            t.name,
            t.kind,
            t.src_path.as_deref().unwrap_or("(no source path)")
        );
    }
    println!("dependencies ({}):", model.dependencies.len());
    for d in &model.dependencies {
        println!(
            "  - {} = \"{}\" kind={:?} runtime={}",
            d.toml_name, d.version_req, d.kind, d.runtime_candidate
        );
    }
    println!("features ({}):", model.features.len());
    for (feature, node) in &model.features {
        println!(
            "  - {:?}: feature_deps={:?} crate_deps={:?}",
            feature, node.feature_deps, node.crate_deps
        );
    }
    println!(
        "default transitive deps: {:?}",
        model.transitive_deps("default")
    );

    Ok(0)
}

pub fn package_crate(
    init: PackageInitArgs,
    mut extract: PackageExtractArgs,
    finish: PackageExecuteArgs,
    range_capability_policy: RangeCapabilityPolicy,
) -> Result<i32, CliError> {
    use std::fs;

    log::info!("preparing crate info");
    let mut process = PackageProcess::init(init).map_err(|_| CliError::Package)?;

    let crate_name = process.crate_info().crate_name();
    let version = process.crate_info().version();

    let output_names = takopack_core::util::rust_crate_output_names(crate_name, version);
    let final_output =
        takopack_core::util::package_final_output_dir(extract.directory.as_deref(), &output_names)
            .map_err(|_| CliError::Package)?;
    extract.directory = Some(final_output.clone());

    process.extract(extract).map_err(|_| CliError::Package)?;
    process.apply_overrides().map_err(|_| CliError::Package)?;
    if range_capability_policy != RangeCapabilityPolicy::Allow {
        let warnings = crate::range_audit::audit_cargo_dependencies(
            process.crate_info().dependencies(),
            Some(&output_names.directory),
        );
        if crate::range_audit::emit_warnings(&warnings, range_capability_policy) {
            // anyhow::bail!("range capability audit failed (policy: error)");
            return Err(CliError::Package);
        }
    }
    process
        .prepare_source_archive()
        .map_err(|_| CliError::Package)?;
    process
        .prepare_takopack_folder(finish)
        .map_err(|_| CliError::Package)?;

    let output_path = process
        .output_dir
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("package extraction did not produce an output directory"))
        .map_err(|_| CliError::Package)?;
    log::debug!("output_path: {}", output_path.display());
    log::debug!("output_dirname: {}", final_output.display());

    let takopack_dir = output_path.join("takopack");
    let source_spec = takopack_dir.join(&output_names.spec_file);

    fs::create_dir_all(&final_output).map_err(|_| CliError::Package)?;
    let final_spec = final_output.join(&output_names.spec_file);

    if !source_spec.exists() {
        // anyhow::bail!("Spec file not found at: {}", source_spec.display());
        return Err(CliError::Package);
    }

    fs::copy(&source_spec, &final_spec).map_err(|_| CliError::Package)?;
    let final_cargo_toml =
        takopack_core::util::copy_normalized_cargo_toml_to_dir(output_path, &final_output)
            .map_err(|_| CliError::Package)?;
    log::info!("Spec file saved to: {}", final_spec.display());
    println!("Spec file: {}", final_spec.display());

    if output_path == &final_output {
        if takopack_dir.exists() {
            fs::remove_dir_all(&takopack_dir).map_err(|_| CliError::Package)?;
        }
        for entry in fs::read_dir(output_path).map_err(|_| CliError::Package)? {
            let entry = entry.map_err(|_| CliError::Package)?;
            let path = entry.path();
            if path != final_spec && path != final_cargo_toml {
                if path.is_dir() {
                    fs::remove_dir_all(&path).map_err(|_| CliError::Package)?;
                } else {
                    fs::remove_file(&path).map_err(|_| CliError::Package)?;
                }
            }
        }
        log::info!("Cleaned up extraction files, kept spec file");
    } else {
        fs::remove_dir_all(output_path).map_err(|_| CliError::Package)?;
        log::info!("Cleaned up extraction directory");
    }

    Ok(0)
}

#[derive(Debug)]
pub enum CliError {
    Package,
    Inspect,
}
