use anyhow::{Context, Result};
use chrono::Local;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use crate::package::{PackageExecuteArgs, PackageExtractArgs, PackageInitArgs, PackageProcess};

/// Information about a failed package
#[derive(Debug, Clone)]
pub struct FailedPackage {
    pub crate_name: String,
    pub version: String,
    pub error: String,
}

/// Process batch file with crate list
pub fn process_batch_file(file_path: &PathBuf, output_base: Option<PathBuf>) -> Result<()> {
    // Create output directory (timestamp or specified)
    let base_dir = if let Some(path) = output_base {
        path
    } else {
        let timestamp = Local::now().format("%Y%m%d_%H%M%S").to_string();
        PathBuf::from(&timestamp)
    };

    fs::create_dir_all(&base_dir)
        .with_context(|| format!("Failed to create output directory: {:?}", base_dir))?;

    log::info!("Created output directory: {}", base_dir.display());

    // Read file and collect all crate entries first
    let file = fs::File::open(file_path)
        .with_context(|| format!("Failed to open file: {:?}", file_path))?;
    let reader = BufReader::new(file);

    let mut crate_list: Vec<(String, String)> = Vec::new();
    for (line_num, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("Failed to read line {}", line_num + 1))?;
        let line = line.trim();

        // Skip empty lines and comments
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Parse line: "crate_name version [clean_flag]"
        // clean_flag is optional, defaults to true
        // now,the clean_flag has been removed.
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            eprintln!(
                "Warning: Invalid line format (expected 'crate_name version'): {}",
                line
            );
            continue;
        }

        let crate_name = parts[0].to_string();
        let version = parts[1].to_string();

        crate_list.push((crate_name, version));
    }

    let total_count = crate_list.len();
    log::info!("Found {} crates to process\n", total_count);

    let mut succeeded = 0;
    let mut failed_packages: Vec<FailedPackage> = Vec::new();

    for (idx, (crate_name, version)) in crate_list.iter().enumerate() {
        log::info!(
            "[{}/{}] Processing: {} {}",
            idx + 1,
            total_count,
            crate_name,
            version
        );

        // Process this crate
        match crate::util::process_single_crate(crate_name, version, &base_dir, None) {
            Ok(_) => {
                succeeded += 1;
                println!("✓ Successfully packaged {} {}", crate_name, version);
            }
            Err(e) => {
                let error_msg = format!("{:?}", e);
                log::error!(
                    "✗ Failed to package {} {}: {}",
                    crate_name,
                    version,
                    error_msg
                );
                failed_packages.push(FailedPackage {
                    crate_name: crate_name.to_string(),
                    version: version.to_string(),
                    error: error_msg,
                });
            }
        }
    }

    // Print summary
    println!("\n{}", "=".repeat(60));
    println!("Batch Processing Summary");
    println!("{}", "=".repeat(60));
    println!("Total packages attempted: {}", total_count);
    println!("Successfully packaged:    {}", succeeded);
    println!("Failed:                   {}", failed_packages.len());

    if !failed_packages.is_empty() {
        println!("\nFailed packages:");
        for pkg in &failed_packages {
            println!("  - {} {}: {}", pkg.crate_name, pkg.version, pkg.error);
        }
    }

    println!("\nOutput directory: {}", base_dir.display());
    println!("{}", "=".repeat(60));

    Ok(())
}
