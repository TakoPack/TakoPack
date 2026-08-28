use std::path::Path;

use clap::Parser;
use nu_ansi_term::Color::Red;

use takopack_core::errors::Result;
use takopack_python::cli::PythonSubcommands;
use takopack_python::pypi::PypiFetcher;
use takopack_rust::RustSubcommands;
use takopack_rust::package::*;
use takopack_rust::range_audit::{self, RangeCapabilityPolicy};

use super::options::{Cli, Opt};

pub fn run() {
    env_logger::init();
    match real_main() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("{}", Red.bold().paint(format!("takopack failed: {:?}", e)));
            std::process::exit(1);
        }
    }
}

fn real_main() -> Result<i32> {
    let m = Cli::parse();
    use Opt::*;
    match m.command {
        Cargo(cargo_opt) => match cargo_opt {
            RustSubcommands::Package {
                init,
                extract,
                finish,
                range_capability_policy,
            } => package_crate(init, extract, finish, range_capability_policy),
            RustSubcommands::LocalPackage {
                path,
                output,
                finish,
                range_capability_policy,
            } => {
                log::info!("packaging from local directory: {:?}", path);
                takopack_rust::local::process_local_package(
                    &path,
                    output,
                    finish,
                    range_capability_policy,
                )?;
                Ok(0)
            }
            RustSubcommands::RegistrySync { dry_run, jobs } => {
                log::info!("starting registry sync");
                takopack_rust::registry_sync::run_registry_sync(dry_run, jobs)
            }
            RustSubcommands::ResolveCheck { path, registry } => {
                log::info!("starting resolve check");
                takopack_rust::resolve_check::run_resolve_check(&path, registry.as_deref())
            }
            RustSubcommands::BuildReqs { path, registry } => {
                log::info!("generating dynamic BuildRequires");
                takopack_rust::buildreqs::run_buildreqs(&path, registry.as_deref())
            }
            RustSubcommands::Inspect {
                target,
                registry,
                version,
            } => inspect_crate(&target, registry, version),
        },
        Py(py_opt) => match py_opt {
            PythonSubcommands::Package { name, version, .. } => {
                let package = PypiFetcher::new(&name, version.as_deref()).fetch().unwrap();

                package.render(&Path::new(".")).unwrap();

                Ok(0)
            }
        },
    }
}

fn inspect_crate(target: &str, registry: bool, version: Option<String>) -> Result<i32> {
    use std::path::Path;
    use takopack_rust::crates::{
        CrateSource, LoadOptions, LocalPathSource, ManifestSource, RegistrySource, Source,
        load_crate,
    };

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
    let model = load_crate(source, LoadOptions::default())?;

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

fn package_crate(
    init: PackageInitArgs,
    mut extract: PackageExtractArgs,
    finish: PackageExecuteArgs,
    range_capability_policy: RangeCapabilityPolicy,
) -> Result<i32> {
    use std::fs;

    log::info!("preparing crate info");
    let mut process = PackageProcess::init(init)?;

    let crate_name = process.crate_info().crate_name();
    let version = process.crate_info().version();

    let output_names = takopack_core::util::rust_crate_output_names(crate_name, version);
    let final_output =
        takopack_core::util::package_final_output_dir(extract.directory.as_deref(), &output_names)?;
    extract.directory = Some(final_output.clone());

    process.extract(extract)?;
    process.apply_overrides()?;
    if range_capability_policy != RangeCapabilityPolicy::Allow {
        let warnings = range_audit::audit_cargo_dependencies(
            process.crate_info().dependencies(),
            Some(&output_names.directory),
        );
        if range_audit::emit_warnings(&warnings, range_capability_policy) {
            anyhow::bail!("range capability audit failed (policy: error)");
        }
    }
    process.prepare_source_archive()?;
    process.prepare_takopack_folder(finish)?;

    let output_path = process
        .output_dir
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("package extraction did not produce an output directory"))?;
    log::debug!("output_path: {}", output_path.display());
    log::debug!("output_dirname: {}", final_output.display());

    let takopack_dir = output_path.join("takopack");
    let source_spec = takopack_dir.join(&output_names.spec_file);

    fs::create_dir_all(&final_output)?;
    let final_spec = final_output.join(&output_names.spec_file);

    if !source_spec.exists() {
        anyhow::bail!("Spec file not found at: {}", source_spec.display());
    }

    fs::copy(&source_spec, &final_spec)?;
    let final_cargo_toml =
        takopack_core::util::copy_normalized_cargo_toml_to_dir(output_path, &final_output)?;
    log::info!("Spec file saved to: {}", final_spec.display());
    println!("Spec file: {}", final_spec.display());

    if output_path == &final_output {
        if takopack_dir.exists() {
            fs::remove_dir_all(&takopack_dir)?;
        }
        for entry in fs::read_dir(output_path)? {
            let entry = entry?;
            let path = entry.path();
            if path != final_spec && path != final_cargo_toml {
                if path.is_dir() {
                    fs::remove_dir_all(&path)?;
                } else {
                    fs::remove_file(&path)?;
                }
            }
        }
        log::info!("Cleaned up extraction files, kept spec file");
    } else {
        fs::remove_dir_all(output_path)?;
        log::info!("Cleaned up extraction directory");
    }

    Ok(0)
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::Cli;

    #[test]
    fn verify_app() {
        Cli::command().debug_assert()
    }
}
