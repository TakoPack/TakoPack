use clap::Parser;
use nu_ansi_term::Color::Red;

use takopack::cli::{CargoOpt, Cli, Opt, PyOpt};
use takopack::crates::invalidate_crates_io_cache;
use takopack::errors::Result;
use takopack::package::*;
use takopack::range_audit::{self, RangeCapabilityPolicy};
use takopack::recursive_package::RecursivePackager;
use takopack::spec_from_toml::parse_dependencies_from_toml;

#[test]
fn verify_app() {
    use clap::CommandFactory;
    Cli::command().debug_assert()
}

fn real_main() -> Result<i32> {
    let m = Cli::parse();
    use Opt::*;
    match m.command {
        Cargo(cargo_opt) => {
            match cargo_opt {
                CargoOpt::Update => invalidate_crates_io_cache().map(|_| 0),
                CargoOpt::Package {
                    init,
                    mut extract,
                    finish,
                    range_capability_policy,
                } => {
                    use std::fs;

                    log::info!("preparing crate info");
                    let mut process = PackageProcess::init(init)?;

                    // Get crate name and version
                    let crate_name = process.crate_info().crate_name();
                    let version = process.crate_info().version();

                    let output_names = takopack::util::rust_crate_output_names(crate_name, version);
                    let final_output = takopack::util::package_final_output_dir(
                        extract.directory.as_deref(),
                        &output_names,
                    )?;
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
                    process.prepare_orig_tarball()?;
                    process.prepare_takopack_folder(finish)?;

                    // After prepare_takopack_folder, the spec file is in output_dir/takopack/
                    let output_path = process.output_dir.as_ref().unwrap();
                    log::debug!("output_path: {}", output_path.display());
                    log::debug!("output_dirname: {}", final_output.display());

                    let takopack_dir = output_path.join("takopack");
                    let source_spec = takopack_dir.join(&output_names.spec_file);

                    // Create final output directory and copy the spec plus normalized Cargo.toml.
                    fs::create_dir_all(&final_output)?;
                    let final_spec = final_output.join(&output_names.spec_file);

                    if source_spec.exists() {
                        fs::copy(&source_spec, &final_spec)?;
                        let final_cargo_toml = takopack::util::copy_normalized_cargo_toml_to_dir(
                            output_path,
                            &final_output,
                        )?;
                        log::info!("Spec file saved to: {}", final_spec.display());
                        println!("Spec file: {}", final_spec.display());

                        // Now cleanup: remove the extraction directory (which has the same name as final_output)
                        // We need to do this carefully to not delete the final spec file
                        if output_path == &final_output {
                            // They are the same directory, just remove everything except the spec file
                            if takopack_dir.exists() {
                                fs::remove_dir_all(&takopack_dir)?;
                            }
                            // Remove other files
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
                            // Different directories, safe to remove the whole output_path
                            fs::remove_dir_all(output_path)?;
                            log::info!("Cleaned up extraction directory");
                        }
                    } else {
                        log::warn!("Spec file not found at: {}", source_spec.display());
                        eprintln!("ERROR: Spec file not found!");
                    };

                    Ok(0)
                }
                CargoOpt::Vendor { args } => {
                    log::info!("starting vendor operation (recursive packaging)");
                    let mut packager = RecursivePackager::new(args.output)?;
                    packager.process_crate_recursive(
                        &args.crate_name,
                        args.version.as_deref(),
                        args.config,
                    )?;
                    packager.print_summary();
                    Ok(0)
                }
                CargoOpt::ParseToml { toml_path, output } => {
                    log::info!("parsing dependencies from Cargo.toml");
                    parse_dependencies_from_toml(&toml_path, output)?;
                    Ok(0)
                }
                CargoOpt::Batch { file, output } => {
                    log::info!("starting batch operation from file: {:?}", file);
                    takopack::batch_package::process_batch_file(&file, output)?;
                    Ok(0)
                }
                CargoOpt::LocalPackage {
                    path,
                    output,
                    finish,
                    range_capability_policy,
                } => {
                    log::info!("packaging from local directory: {:?}", path);
                    takopack::local_package::process_local_package(
                        &path,
                        output,
                        finish,
                        range_capability_policy,
                    )?;
                    Ok(0)
                }
            }
        }
        Opt::Py(py_opt) => match py_opt {
            PyOpt::Package {
                name,
                version,
                output,
            } => {
                log::info!("packaging Python package from PyPI");
                takopack::python_package::process_python_package(
                    &name,
                    version.as_deref(),
                    output,
                )?;
                Ok(0)
            }
        },
    }
}

fn main() {
    env_logger::init();
    match real_main() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("{}", Red.bold().paint(format!("takopack failed: {:?}", e)));
            std::process::exit(1);
        }
    }
}
