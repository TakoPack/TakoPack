use clap::Parser;
use nu_ansi_term::Color::Red;

use takopack_core::errors::Result;
use takopack_rust::package::*;
use takopack_rust::range_audit::{self, RangeCapabilityPolicy};

use super::options::{CargoOpt, Cli, Opt, PyOpt};

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
            CargoOpt::Package {
                init,
                extract,
                finish,
                range_capability_policy,
            } => package_crate(init, extract, finish, range_capability_policy),
            CargoOpt::LocalPackage {
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
            CargoOpt::RegistrySync { dry_run, jobs } => {
                log::info!("starting registry sync");
                takopack_rust::registry_sync::run_registry_sync(dry_run, jobs)
            }
            CargoOpt::ResolveCheck { path, registry } => {
                log::info!("starting resolve check");
                takopack_rust::resolve_check::run_resolve_check(&path, registry.as_deref())
            }
            CargoOpt::BuildReqs { path, registry } => {
                log::info!("generating dynamic BuildRequires");
                takopack_rust::buildreqs::run_buildreqs(&path, registry.as_deref())
            }
        },
        Py(py_opt) => match py_opt {
            PyOpt::Package {
                name,
                version,
                output,
            } => {
                log::info!("packaging Python package from PyPI");
                takopack_python::process_python_package(&name, version.as_deref(), output)?;
                Ok(0)
            }
        },
    }
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
