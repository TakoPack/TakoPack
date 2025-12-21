use clap::Parser;
use nu_ansi_term::Color::Red;

use takopack::cli::{Cli, Opt};
use takopack::crates::invalidate_crates_io_cache;
use takopack::errors::Result;
use takopack::package::*;
use takopack::recursive_package::RecursivePackager;
use takopack::spec_from_toml::{generate_spec_from_toml, parse_dependencies_from_toml};

#[test]
fn verify_app() {
    use clap::CommandFactory;
    Cli::command().debug_assert()
}

fn real_main() -> Result<()> {
    let m = Cli::parse();
    use Opt::*;
    match m.command {
        Cargo(cargo_opt) => {
            use takopack::cli::CargoOpt;
            match cargo_opt {
                CargoOpt::Update => invalidate_crates_io_cache(),
                CargoOpt::Package {
                    init,
                    extract,
                    finish,
                } => {
                    use std::fs;
                    use std::path::PathBuf;

                    log::info!("preparing crate info");
                    let mut process = PackageProcess::init(init)?;

                    // Get crate name and version
                    let crate_name = process.crate_info().crate_name();
                    let version = process.crate_info().version();
                    let output_dirname =
                        format!("rust-{}-{}", crate_name.replace('_', "-"), version);
                    let spec_filename = format!("rust-{}.spec", crate_name.replace('_', "-"));
                    let final_output = PathBuf::from(&output_dirname);

                    log::info!("extracting crate");
                    process.extract(extract)?;
                    log::info!("applying overlay and patches");
                    process.apply_overrides()?;
                    log::info!("preparing orig tarball");
                    process.prepare_orig_tarball()?;
                    log::info!("preparing takopack folder");
                    process.prepare_takopack_folder(finish)?;

                    // After prepare_takopack_folder, the spec file is in output_dir/takopack/
                    let output_path = process.output_dir.as_ref().unwrap();
                    let takopack_dir = output_path.join("takopack");
                    let source_spec = takopack_dir.join(&spec_filename);

                    // Create final output directory and copy only the spec file
                    fs::create_dir_all(&final_output)?;
                    let final_spec = final_output.join(&spec_filename);

                    if source_spec.exists() {
                        fs::copy(&source_spec, &final_spec)?;
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
                                if path != final_spec {
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

                    Ok(())
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
                    Ok(())
                }
                CargoOpt::FromToml { toml_path, output } => {
                    log::info!("generating spec file from Cargo.toml");
                    generate_spec_from_toml(&toml_path, output)?;
                    Ok(())
                }
                CargoOpt::ParseToml { toml_path, output } => {
                    log::info!("parsing dependencies from Cargo.toml");
                    parse_dependencies_from_toml(&toml_path, output)?;
                    Ok(())
                }
            }
        }
    }
}

fn main() {
    env_logger::init();
    if let Err(e) = real_main() {
        eprintln!("{}", Red.bold().paint(format!("takopack failed: {:?}", e)));
        std::process::exit(1);
    }
}
