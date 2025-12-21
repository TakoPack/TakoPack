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

/// Generate spec file from local Cargo.toml without downloading the crate
pub fn generate_spec_from_toml(toml_path: &Path, output_dir: Option<PathBuf>) -> Result<()> {
    // Read and parse Cargo.toml
    let cargo_toml_content = fs::read_to_string(toml_path)
        .with_context(|| format!("Failed to read Cargo.toml: {:?}", toml_path))?;

    let manifest: Value =
        toml::from_str(&cargo_toml_content).with_context(|| "Failed to parse Cargo.toml")?;

    let package = manifest
        .get("package")
        .and_then(|p| p.as_table())
        .context("No [package] section in Cargo.toml")?;

    let name = package
        .get("name")
        .and_then(|n| n.as_str())
        .context("No package name")?;

    let version = package
        .get("version")
        .and_then(|v| v.as_str())
        .context("No package version")?;

    let default_description = format!("Rust crate {}", name);
    let description = package
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or(&default_description);

    let license = package
        .get("license")
        .and_then(|l| l.as_str())
        .unwrap_or("MIT OR Apache-2.0");

    let homepage = package
        .get("homepage")
        .or_else(|| package.get("repository"))
        .and_then(|h| h.as_str())
        .unwrap_or("");

    // Parse dependencies
    let dependencies = manifest.get("dependencies").and_then(|d| d.as_table());

    // Generate spec file content
    let pkg_name = format!("rust-{}", name);
    let spec_content =
        generate_spec_content(name, version, description, license, homepage, dependencies)?;

    // Determine output path
    let output_path = if let Some(dir) = output_dir {
        fs::create_dir_all(&dir)?;
        dir.join(format!("{}.spec", pkg_name))
    } else {
        PathBuf::from(format!("{}.spec", pkg_name))
    };

    // Write spec file
    fs::write(&output_path, spec_content)
        .with_context(|| format!("Failed to write spec file: {:?}", output_path))?;

    println!("Generated spec file: {:?}", output_path);
    Ok(())
}

/// // TODO: It's experimental and doesn't handle all features yet.
fn generate_spec_content(
    name: &str,
    version: &str,
    description: &str,
    license: &str,
    homepage: &str,
    dependencies: Option<&toml::map::Map<String, Value>>,
) -> Result<String> {
    let pkg_name = format!("rust-{}", name);

    let mut spec = String::new();

    // Header
    spec.push_str(&format!("%global crate_name {}\n\n", name));

    // Basic package info
    spec.push_str(&format!("Name:           {}\n", pkg_name));
    spec.push_str(&format!("Version:        {}\n", version));
    spec.push_str("Release:        %autorelease\n");
    spec.push_str(&format!("Summary:        Rust crate \"{}\"\n", name));
    spec.push_str(&format!("License:        {}\n", license));
    if !homepage.is_empty() {
        spec.push_str(&format!("URL:            {}\n", homepage));
    }
    spec.push_str("#!RemoteAsset\n");
    spec.push_str(&format!("Source:         https://crates.io/api/v1/crates/%{{crate_name}}/%{{version}}/download#/%{{name}}-%{{version}}.tar.gz\n"));
    spec.push_str("BuildSystem:    autotools\n\n");

    // Dependencies
    if let Some(deps) = dependencies {
        for (dep_name, dep_value) in deps {
            // Skip optional dependencies and build/dev dependencies
            if let Some(table) = dep_value.as_table() {
                if table
                    .get("optional")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    continue;
                }
            }

            // Parse version requirement
            let version_req = if let Some(v) = dep_value.as_str() {
                Some(v)
            } else if let Some(table) = dep_value.as_table() {
                table.get("version").and_then(|v| v.as_str())
            } else {
                None
            };

            // Convert dependency name (underscore to dash)
            let crate_dep_name = dep_name.replace('_', "-");

            if let Some(ver) = version_req {
                // Parse version requirement (e.g., "1.0", "^1.0", ">=1.0")
                let clean_ver = ver
                    .trim_start_matches('^')
                    .trim_start_matches('=')
                    .trim_start_matches('~');
                spec.push_str(&format!(
                    "Requires:       crate({}/default) >= {}\n",
                    crate_dep_name, clean_ver
                ));
            } else {
                spec.push_str(&format!(
                    "Requires:       crate({}/default)\n",
                    crate_dep_name
                ));
            }
        }
    }

    spec.push_str(&format!("Provides:       crate({})\n", name));
    spec.push_str(&format!("Provides:       crate({}/default)\n\n", name));

    // Description
    spec.push_str("%description\n");
    spec.push_str(description);
    spec.push_str("\n\n");

    // Build sections
    spec.push_str("%conf\n");
    spec.push_str("# Library package - no configure needed.\n\n");

    spec.push_str("%build\n");
    spec.push_str("# Library package - no build needed.\n\n");

    spec.push_str("%install\n");
    spec.push_str("# Install source code for library package.\n");
    spec.push_str("rm -f Cargo.lock\n");
    spec.push_str("install -d %{buildroot}%{_datadir}/cargo/registry/%{crate_name}-%{version}\n");
    spec.push_str("cp -a * %{buildroot}%{_datadir}/cargo/registry/%{crate_name}-%{version}/\n");
    spec.push_str("# Remove old cargo-checksum.json and create new .cargo-checksum.json\n");
    spec.push_str(
        "rm -f %{buildroot}%{_datadir}/cargo/registry/%{crate_name}-%{version}/*checksum.json\n",
    );
    spec.push_str("echo '{\"files\":{},\"package\":null}' > %{buildroot}%{_datadir}/cargo/registry/%{crate_name}-%{version}/.cargo-checksum.json\n\n");

    spec.push_str("# No tests here.\n");
    spec.push_str("%check\n\n");

    spec.push_str("%files\n");
    spec.push_str("%{_datadir}/cargo/registry/%{crate_name}-%{version}/\n\n");

    spec.push_str("%changelog\n");
    spec.push_str("%{?autochangelog}\n");

    Ok(spec)
}
