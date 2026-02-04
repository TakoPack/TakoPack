use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::crates::CrateInfo;
use crate::package::PackageExecuteArgs;
use crate::takopack::{self, DebInfo};

/// Process a local crate directory and generate spec file
pub fn process_local_package(
    path: &Path,
    output_dir: Option<PathBuf>,
    finish_args: PackageExecuteArgs,
) -> Result<()> {
    // Canonicalize the path first to get absolute path
    let path_abs =
        fs::canonicalize(path).with_context(|| format!("Failed to resolve path: {:?}", path))?;

    // Determine the crate directory and Cargo.toml path
    let cargo_toml = if path_abs.is_file() {
        // Path is a .toml file
        if !path_abs.extension().map(|e| e == "toml").unwrap_or(false) {
            anyhow::bail!("File must be a .toml file: {:?}", path_abs);
        }
        path_abs
    } else if path_abs.is_dir() {
        // Path is a directory
        let toml = path_abs.join("Cargo.toml");
        if !toml.exists() {
            anyhow::bail!("Cargo.toml not found in directory: {:?}", path_abs);
        }
        toml
    } else {
        anyhow::bail!(
            "Invalid path: must be a directory or Cargo.toml file: {:?}",
            path_abs
        );
    };

    log::info!("Processing local crate from: {:?}", cargo_toml);

    // Create a temporary directory with minimal crate structure
    // TODO: Enable user to set crate structure.
    // Or user changes toml at a crate root and then there is no need to crate.
    let temp_crate_dir =
        tempfile::tempdir().context("Failed to create temporary crate directory")?;

    // Copy the Cargo.toml to the temp directory
    let temp_cargo_toml = temp_crate_dir.path().join("Cargo.toml");
    fs::copy(&cargo_toml, &temp_cargo_toml)
        .with_context(|| format!("Failed to copy Cargo.toml to temp dir"))?;

    // Copy the Cargo.toml to the temp directory
    let temp_lib_rs = temp_crate_dir.path().join("lib.rs");
    fs::write(&temp_lib_rs, "// Placeholder for spec generation\n")
        .context("Failed to create lib.rs")?;

    // Create minimal src/ structure so Cargo APIs can work
    let src_dir = temp_crate_dir.path().join("src");
    fs::create_dir(&src_dir)?;

    // Create common source files to support various path configurations
    let placeholder_content = "// Placeholder for spec generation\n";

    // lib.rs - standard library entry point
    fs::write(src_dir.join("lib.rs"), placeholder_content).context("Failed to create lib.rs")?;

    // main.rs - standard binary entry point
    fs::write(src_dir.join("main.rs"), placeholder_content).context("Failed to create main.rs")?;

    // ffi.rs - common for FFI crates (like imagequant-sys)
    fs::write(src_dir.join("ffi.rs"), placeholder_content).context("Failed to create ffi.rs")?;

    // mod.rs - sometimes used as module root
    fs::write(src_dir.join("mod.rs"), placeholder_content).context("Failed to create mod.rs")?;

    // Create rust/ subdirectory for non-standard paths
    let rust_dir = temp_crate_dir.path().join("rust");
    fs::create_dir_all(&rust_dir).ok();

    // rust/build.rs - non-standard build script location (e.g., pngquant)
    fs::write(rust_dir.join("build.rs"), placeholder_content).ok();

    // rust/bin.rs - non-standard binary location (e.g., pngquant)
    fs::write(rust_dir.join("bin.rs"), placeholder_content).ok();

    // rust/lib.rs - non-standard library location
    fs::write(rust_dir.join("lib.rs"), placeholder_content).ok();

    // Create a dummy README.md if referenced in Cargo.toml
    let readme_path = temp_crate_dir.path().join("README.md");
    fs::write(&readme_path, "# Placeholder README\n").context("Failed to create README.md")?;

    // Standard build.rs location
    let build_rs = temp_crate_dir.path().join("build.rs");
    fs::write(&build_rs, "// Placeholder build script\n").context("Failed to create build.rs")?;

    // Create dummy LICENSE files if needed
    let license_mit = temp_crate_dir.path().join("LICENSE-MIT");
    fs::write(&license_mit, "Placeholder MIT license\n").ok();

    let license_apache = temp_crate_dir.path().join("LICENSE-APACHE");
    fs::write(&license_apache, "Placeholder Apache license\n").ok();

    log::info!(
        "Temporary crate structure created at: {:?}",
        temp_crate_dir.path()
    );

    // Now process this temporary complete crate with full takopack pipeline
    return process_complete_crate(
        temp_crate_dir.path(),
        &temp_cargo_toml,
        output_dir,
        finish_args,
    );
}

/// Process a complete crate directory (with src/) using full takopack pipeline
fn process_complete_crate(
    temp_crate_dir: &Path,
    cargo_toml: &Path,
    output_dir: Option<PathBuf>,
    finish_args: PackageExecuteArgs,
) -> Result<()> {
    if false {
        // Backup the original Cargo.toml FIRST (before any cleaning or processing)
        // Need to parse just to get name and version for backup filename
        // TODO: may not necessary, keep the code temporarily.
        let backup_content = fs::read_to_string(&cargo_toml)
            .with_context(|| format!("Failed to read Cargo.toml: {:?}", cargo_toml))?;

        let backup_manifest: toml::Value = toml::from_str(&backup_content)
            .with_context(|| format!("Failed to parse Cargo.toml: {:?}", cargo_toml))?;

        if let Some(package) = backup_manifest.get("package") {
            if let (Some(name), Some(version)) = (
                package.get("name").and_then(|n| n.as_str()),
                package.get("version").and_then(|v| v.as_str()),
            ) {
                // Backup original to ~/cargo_back/patch/origin/
                if let Err(e) =
                    crate::util::backup_cargo_toml(&cargo_toml, name, version, Some("patch/origin"))
                {
                    log::warn!("Failed to backup original Cargo.toml: {:?}", e);
                }
            }
        }
    }

    // Load config if available
    let config_path = temp_crate_dir.join("takopack.toml");
    let (config_path, config) = if config_path.exists() {
        let config = Config::parse(&config_path).context("failed to parse takopack.toml")?;
        (Some(config_path), config)
    } else {
        (None, Config::default())
    };

    // Create CrateInfo from local crate (now it has src/ so Cargo APIs will work)
    let mut crate_info = CrateInfo::new_with_local_crate_from_path(cargo_toml)
        .with_context(|| format!("Failed to load crate from: {:?}", cargo_toml))?;

    let crate_name = crate_info.crate_name();
    // It's a full version,like "0.9.11+spec-1.1.0"
    let version = crate_info.version();

    log::info!("Crate: {} {}", crate_name, version);

    // Create DebInfo
    let deb_info = DebInfo::new(&crate_info, env!("CARGO_PKG_VERSION"), config.semver_suffix);

    // Calculate compatibility version following Rust semver rules
    let compat_version = crate::util::calculate_compat_version(version);

    // Determine output directory
    let output_base = output_dir.unwrap_or_else(|| PathBuf::from("."));
    let output_dirname = format!("rust-{}-{}", crate_name.replace('_', "-"), compat_version);
    let final_output = output_base.join(&output_dirname);

    fs::create_dir_all(&final_output)
        .with_context(|| format!("Failed to create output directory: {:?}", final_output))?;

    // Create a temporary directory for takopack processing
    let tempdir =
        tempfile::tempdir_in(&temp_crate_dir).context("Failed to create temporary directory")?;

    log::info!("Tempdir created at: {:?}", tempdir.path());
    log::info!("Preparing takopack folder");

    // Apply overrides and generate spec file
    let prepare_result = takopack::prepare_takopack_folder(
        &mut crate_info,
        &deb_info,
        config_path.as_deref(),
        &config,
        &temp_crate_dir,
        &tempdir,
        finish_args.changelog_ready,
        finish_args.copyright_guess_harder,
        !finish_args.no_overlay_write_back,
        None, // TODO: sha256: local packages don't have downloaded crate files, maybe consider record the sha256 when use pkg.
        finish_args.lockfile_deps, // Pass lockfile dependencies if available
    );

    if let Err(e) = &prepare_result {
        log::error!("prepare_takopack_folder failed: {:?}", e);
    }
    prepare_result?;

    // Note: prepare_takopack_folder renames tempdir to output_dir/takopack
    let takopack_dir = temp_crate_dir.join("takopack");
    log::info!("Takopack folder should be at: {:?}", takopack_dir);
    log::info!("Takopack dir exists: {}", takopack_dir.exists());

    // Copy spec file to output directory
    let spec_filename = format!("rust-{}.spec", crate_name.replace('_', "-"));
    let source_spec = takopack_dir.join(&spec_filename);
    let final_spec = final_output.join(&spec_filename);

    // List files in takopack dir for debugging
    log::debug!("Listing files in takopack dir: {:?}", takopack_dir);
    match fs::read_dir(&takopack_dir) {
        Ok(entries) => {
            for entry in entries {
                if let Ok(entry) = entry {
                    log::debug!("  - {:?}", entry.file_name());
                }
            }
        }
        Err(e) => {
            log::error!("Failed to read takopack dir: {:?}", e);
        }
    }

    if source_spec.exists() {
        fs::copy(&source_spec, &final_spec)
            .with_context(|| format!("Failed to copy spec file to: {:?}", final_spec))?;

        log::info!("Spec file saved to: {}", final_spec.display());
        println!("Spec file: {}", final_spec.display());
    } else {
        anyhow::bail!("Spec file not found at: {:?}", source_spec);
    }

    Ok(())
}
