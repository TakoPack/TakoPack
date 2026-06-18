use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use toml::Value;

use crate::recursive_package::RecursivePackager;

/// Parse dependencies from Cargo.toml and recursively generate spec files for all dependencies
pub fn parse_dependencies_from_toml(toml_path: &Path, output_dir: Option<PathBuf>) -> Result<()> {
    // Read and parse Cargo.toml
    let cargo_toml_content = fs::read_to_string(toml_path)
        .with_context(|| format!("Failed to read Cargo.toml: {:?}", toml_path))?;

    let manifest: Value =
        toml::from_str(&cargo_toml_content).with_context(|| "Failed to parse Cargo.toml")?;

    // Parse dependencies
    let dependencies = manifest
        .get("dependencies")
        .and_then(|d| d.as_table())
        .context("No [dependencies] section in Cargo.toml")?;

    // Determine output directory: use provided or generate timestamped directory
    let output_dir = output_dir.unwrap_or_else(|| {
        let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
        PathBuf::from(timestamp)
    });

    // Create output directory
    fs::create_dir_all(&output_dir)?;
    println!("Output directory: {:?}", output_dir);

    // Create a recursive packager to handle dependency resolution
    let mut packager = RecursivePackager::new(Some(output_dir))?;

    println!("Found {} dependencies in Cargo.toml", dependencies.len());

    // Process each dependency recursively
    for (dep_name, dep_value) in dependencies {
        println!("recursive processing dependency: {}", dep_name);
        // Skip optional dependencies
        if let Some(table) = dep_value.as_table() {
            if table
                .get("optional")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                println!("Skipping optional dependency: {}", dep_name);
                continue;
            }
        }

        // Parse version requirement
        let version = if let Some(v) = dep_value.as_str() {
            Some(v.to_string())
        } else if let Some(table) = dep_value.as_table() {
            table
                .get("version")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        } else {
            None
        };

        // Use the dependency name as-is (keep dashes, don't convert to underscores)
        // Cargo.toml uses the actual crate name which matches crates.io

        println!(
            "\nProcessing dependency: {} (version: {:?})",
            dep_name, version
        );

        // Process this crate and all its dependencies recursively
        if let Err(e) = packager.process_crate_recursive(
            dep_name, // Use the original name with dashes
            version.as_deref(),
            None,
        ) {
            eprintln!("Failed to process {}: {:#}", dep_name, e);
        }
    }

    // Print summary
    packager.print_summary();

    Ok(())
}
