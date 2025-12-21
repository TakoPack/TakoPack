use anyhow::{Context, Result};
use chrono::Local;
use clap::Parser;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use crate::package::{PackageExecuteArgs, PackageExtractArgs, PackageInitArgs, PackageProcess};

/// Arguments for recursive packaging command
#[derive(Debug, Clone, Parser)]
pub struct RecursivePackageArgs {
    /// Name of the crate to package.
    pub crate_name: String,
    /// Version of the crate to package; may contain dependency operators.
    /// If empty string or omitted, resolves to the latest version.
    pub version: Option<String>,
    /// TOML file providing package-specific options.
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Base output directory for all packages (timestamp as default).
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,
}

/// Information about a failed package
#[derive(Debug, Clone)]
pub struct FailedPackage {
    pub crate_name: String,
    pub version: String,
    pub error: String,
}

/// State for recursive package processing
pub struct RecursivePackager {
    /// Base output directory with timestamp
    pub base_dir: PathBuf,
    /// Set of successfully processed (crate_name, version) pairs
    pub processed: HashSet<(String, String)>,
    /// Set of crates that are currently being processed (to detect cycles)
    pub in_progress: HashSet<(String, String)>,
    /// List of failed packages
    pub failed: Vec<FailedPackage>,
    /// Statistics
    pub total_attempted: usize,
    /// Mapping from normalized name (with dashes) to real crate name
    /// Example: "parking-lot-core" -> "parking_lot_core"
    ///          "proc-macro2" -> "proc-macro2"
    pub crate_name_map: HashMap<String, String>,
}

impl RecursivePackager {
    /// Create a new recursive packager with timestamp-based directory
    pub fn new(base_path: Option<PathBuf>) -> Result<Self> {
        let base_dir = if let Some(path) = base_path {
            path
        } else {
            let timestamp = Local::now().format("%Y%m%d_%H%M%S").to_string();
            PathBuf::from(&timestamp)
        };

        fs::create_dir_all(&base_dir)
            .with_context(|| format!("Failed to create base directory: {:?}", base_dir))?;

        println!("Created output directory: {}", base_dir.display());

        Ok(RecursivePackager {
            base_dir,
            processed: HashSet::new(),
            in_progress: HashSet::new(),
            failed: Vec::new(),
            total_attempted: 0,
            crate_name_map: HashMap::new(),
        })
    }

    /// Process a crate and its dependencies recursively
    /// TODO: the crate_name must be the real crate name,or may fail to package.
    pub fn process_crate_recursive(
        &mut self,
        crate_name: &str,
        version: Option<&str>,
        config_path: Option<PathBuf>,
    ) -> Result<()> {
        println!("crate_name is {}", crate_name);
        let version_str = version.unwrap_or("latest");
        let key = (crate_name.to_string(), version_str.to_string());

        // Check if already processed or failed
        if self.processed.contains(&key) {
            println!(
                "Skipping {} {} (already processed)",
                crate_name, version_str
            );
            return Ok(());
        }

        // Check if currently in progress (circular dependency detection)
        if self.in_progress.contains(&key) {
            println!(
                "Circular dependency detected for {} {}, skipping",
                crate_name, version_str
            );
            return Ok(());
        }

        // Check if any version of this crate has already been processed OR is currently being processed
        // This prevents re-packaging and overwriting when a dependency requests a different version
        let crate_already_packaged = self.processed.iter().any(|(name, _)| name == crate_name);
        let crate_in_progress = self.in_progress.iter().any(|(name, _)| name == crate_name);

        if crate_already_packaged {
            println!(
                "Skipping {} {} (another version already packaged)",
                crate_name, version_str
            );
            return Ok(());
        }
        if crate_in_progress {
            println!(
                "Skipping {} {} (another version currently being processed)",
                crate_name, version_str
            );
            return Ok(());
        }

        // Check if already failed
        if self
            .failed
            .iter()
            .any(|f| f.crate_name == crate_name && f.version == version_str)
        {
            println!(
                "Skipping {} {} (previously failed)",
                crate_name, version_str
            );
            return Ok(());
        }

        // Mark as in progress
        self.in_progress.insert(key.clone());
        self.total_attempted += 1;
        println!("\nProcessing {} {}...", crate_name, version_str);

        // Try to package this crate
        // If crate_name contains '-', try both '-' and '_' versions
        let (_spec_path, _real_crate_name, dependencies) =
            match self.package_single_crate(crate_name, version, config_path.clone()) {
                Ok((path, real_name, deps)) => {
                    println!(
                        "Successfully packaged {} {} (real name: {})",
                        crate_name, version_str, real_name
                    );

                    // Store the mapping: normalized name (with dashes) -> real crate name
                    let normalized_name = crate_name.replace('_', "-");
                    self.crate_name_map
                        .insert(normalized_name, real_name.clone());

                    self.in_progress.remove(&key);
                    self.processed.insert(key.clone());
                    (path, real_name, deps)
                }
                Err(e) => {
                    let error_msg = format!("{:#}", e);

                    // If the crate name contains dashes and packaging failed,
                    // try with underscores (e.g., parking-lot-core -> parking_lot_core)
                    if crate_name.contains('-') {
                        let alt_name = crate_name.replace('-', "_");
                        println!(
                            "Failed with '{}', trying alternate name '{}'...",
                            crate_name, alt_name
                        );

                        match self.package_single_crate(&alt_name, version, config_path.clone()) {
                            Ok((path, real_name, deps)) => {
                                println!(
                                    "Successfully packaged {} {} (as {}, real name: {})",
                                    crate_name, version_str, alt_name, real_name
                                );

                                // Store the mapping: normalized name (with dashes) -> real crate name
                                let normalized_name = crate_name.replace('_', "-");
                                self.crate_name_map
                                    .insert(normalized_name, real_name.clone());

                                self.in_progress.remove(&key);
                                self.processed.insert(key.clone());
                                (path, real_name, deps)
                            }
                            Err(e2) => {
                                let error_msg2 = format!("{:#}", e2);
                                println!(
                                    "Failed to package {} {}: {} (also tried {})",
                                    crate_name, version_str, error_msg, alt_name
                                );
                                self.in_progress.remove(&key);
                                self.failed.push(FailedPackage {
                                    crate_name: crate_name.to_string(),
                                    version: version_str.to_string(),
                                    error: format!(
                                        "Both failed - '{}': {}, '{}': {}",
                                        crate_name, error_msg, alt_name, error_msg2
                                    ),
                                });
                                return Ok(());
                            }
                        }
                    } else {
                        println!(
                            "Failed to package {} {}: {}",
                            crate_name, version_str, error_msg
                        );
                        self.in_progress.remove(&key);
                        self.failed.push(FailedPackage {
                            crate_name: crate_name.to_string(),
                            version: version_str.to_string(),
                            error: error_msg,
                        });
                        return Ok(());
                    }
                }
            };

        println!(
            "Found {} runtime dependencies for {}",
            dependencies.len(),
            crate_name
        );

        // Map dependencies to their real names before processing
        // (dependencies already contain the real crate names from Cargo.toml)
        let deps_with_real_names: Vec<(String, Option<String>)> =
            dependencies.into_iter().collect();

        // Recursively process each dependency
        for (real_dep_name, dep_version) in deps_with_real_names {
            self.process_crate_recursive(
                &real_dep_name,
                dep_version.as_deref(),
                config_path.clone(),
            )?;
        }

        Ok(())
    }

    /// Package a single crate and return (spec_path, real_crate_name, dependencies)
    fn package_single_crate(
        &self,
        crate_name: &str,
        version: Option<&str>,
        config_path: Option<PathBuf>,
    ) -> Result<(PathBuf, String, Vec<(String, Option<String>)>)> {
        // Convert underscores to dashes for package naming
        let pkg_name = format!("rust-{}", crate_name.replace('_', "-"));

        // Create final output directory for this crate
        let final_pkg_dir = self.base_dir.join(&pkg_name);

        // If directory exists, remove it first to avoid conflicts
        if final_pkg_dir.exists() {
            if final_pkg_dir.is_dir() {
                fs::remove_dir_all(&final_pkg_dir).with_context(|| {
                    format!("Failed to remove existing directory: {:?}", final_pkg_dir)
                })?;
            } else {
                // It's a file, remove it
                fs::remove_file(&final_pkg_dir).with_context(|| {
                    format!("Failed to remove existing file: {:?}", final_pkg_dir)
                })?;
            }
        }

        fs::create_dir_all(&final_pkg_dir)
            .with_context(|| format!("Failed to create package directory: {:?}", final_pkg_dir))?;

        // Use a temporary directory for extraction and processing
        let temp_dir = tempfile::Builder::new()
            .prefix(&format!("takopack-{}-", pkg_name))
            .tempdir()
            .context("Failed to create temporary directory")?;

        let temp_pkg_dir = temp_dir.path().to_path_buf();

        // Setup package args
        let init_args = PackageInitArgs {
            crate_name: crate_name.to_string(),
            version: version.map(|s| s.to_string()),
            config: config_path,
        };

        let extract_args = PackageExtractArgs {
            directory: Some(temp_pkg_dir.clone()),
        };

        let execute_args = PackageExecuteArgs {
            changelog_ready: false,
            copyright_guess_harder: false,
            no_overlay_write_back: true,
        };

        // Execute packaging
        let mut process = PackageProcess::init(init_args)
            .with_context(|| format!("Failed to init package process for {}", crate_name))?;
        process
            .extract(extract_args)
            .with_context(|| format!("Failed to extract package for {}", crate_name))?;
        process
            .apply_overrides()
            .with_context(|| format!("Failed to apply overrides for {}", crate_name))?;
        process
            .prepare_orig_tarball()
            .with_context(|| format!("Failed to prepare tarball for {}", crate_name))?;
        process
            .prepare_takopack_folder(execute_args)
            .with_context(|| format!("Failed to prepare takopack folder for {}", crate_name))?;

        // Extract the real crate name from the package metadata
        let real_crate_name = process.crate_info.crate_name().to_string();

        // Extract runtime dependencies from the crate's Cargo.toml metadata
        // This is more reliable than parsing the generated spec file
        let dependencies =
            self.extract_dependencies_from_crate_info(&process.crate_info, crate_name)?;

        // Find and copy the generated spec file to final location
        let spec_name = format!("{}.spec", pkg_name);
        let temp_spec_path = temp_pkg_dir.join("takopack").join(&spec_name);
        let final_spec_path = final_pkg_dir.join(&spec_name);

        if temp_spec_path.exists() {
            fs::copy(&temp_spec_path, &final_spec_path).with_context(|| {
                format!(
                    "Failed to copy spec file from {:?} to {:?}",
                    temp_spec_path, final_spec_path
                )
            })?;
        } else {
            anyhow::bail!("Spec file not found: {:?}", temp_spec_path);
        }

        // temp_dir will be automatically cleaned up when dropped

        Ok((final_spec_path, real_crate_name, dependencies))
    }

    /// Extract runtime dependencies from CrateInfo (from Cargo.toml metadata)
    /// This is more reliable than parsing the generated spec file
    fn extract_dependencies_from_crate_info(
        &self,
        crate_info: &crate::crates::CrateInfo,
        current_crate: &str,
    ) -> Result<Vec<(String, Option<String>)>> {
        use cargo::core::dependency::DepKind;

        let mut dependencies = Vec::new();
        let mut seen = HashSet::new();
        let current_crate_normalized = current_crate.replace('-', "_");

        // List of crates to skip (internal Rust workspace crates, etc.)
        let skip_crates = [
            "rustc_std_workspace_core",
            "rustc_std_workspace_alloc",
            "rustc_std_workspace_std",
            "compiler_builtins",
        ];

        // Common proc-macro crate suffixes to skip
        let proc_macro_suffixes = ["-derive", "-macro", "-macros"];

        // Iterate through all dependencies from Cargo.toml
        for dep in crate_info.dependencies() {
            // Skip dev dependencies and build dependencies
            // We only want runtime dependencies
            if dep.kind() == DepKind::Development {
                println!("⏭️  Skipping dev dependency: {}", dep.package_name());
                continue;
            }

            // Get the real crate name from the dependency
            // This is the actual package name on crates.io
            let dep_crate_name = dep.package_name().to_string();

            // For comparison with current crate, normalize both
            let dep_crate_name_normalized = dep_crate_name.replace('-', "_");
            let current_crate_normalized_cmp = current_crate_normalized.replace('-', "_");

            // Skip if it's the current crate itself
            if dep_crate_name_normalized == current_crate_normalized_cmp {
                continue;
            }

            // Skip internal Rust workspace crates
            if skip_crates.contains(&dep_crate_name_normalized.as_str()) {
                println!("⏭️  Skipping internal crate: {}", dep_crate_name);
                continue;
            }

            // Skip proc-macro crates (they are build-time dependencies)
            if proc_macro_suffixes
                .iter()
                .any(|suffix| dep_crate_name.ends_with(suffix))
            {
                println!("⏭️  Skipping proc-macro crate: {}", dep_crate_name);
                continue;
            }

            // Skip optional dependencies
            if dep.is_optional() {
                println!("⏭️  Skipping optional dependency: {}", dep_crate_name);
                continue;
            }

            // Extract version requirement
            // We'll use a simplified version - just take the version requirement as-is
            let version_req = dep.version_req();
            let version_str = if version_req.to_string() == "*" {
                None
            } else {
                // Convert semver requirement to a simple version string
                // For now, we'll just use the version requirement as-is
                Some(version_req.to_string())
            };

            // Deduplicate dependencies
            if !seen.contains(&dep_crate_name) {
                seen.insert(dep_crate_name.clone());
                dependencies.push((dep_crate_name, version_str));
            }
        }

        Ok(dependencies)
    }

    /// Print summary of the packaging process
    pub fn print_summary(&self) {
        println!("\n{}", "=".repeat(62));
        println!("📊 Packaging Summary");
        println!("{}", "=".repeat(62));
        println!("Total attempted:    {}", self.total_attempted);
        println!("Successfully built: {}", self.processed.len());
        println!("Failed:             {}", self.failed.len());
        println!("{}", "=".repeat(62));

        if !self.failed.is_empty() {
            println!("\n❌ Failed Packages:");
            println!("{}", "-".repeat(62));
            for (i, failed) in self.failed.iter().enumerate() {
                println!("{}. {} {}", i + 1, failed.crate_name, failed.version);
                println!("   Error: {}", failed.error);
                println!();
            }
        }

        println!("📁 Output directory: {}", self.base_dir.display());
        println!("{}\n", "=".repeat(62));
    }
}
