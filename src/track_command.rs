use anyhow::{Context, Result};
use chrono::Local;
use std::fs;
use std::path::{Path, PathBuf};

use crate::crate_database::CrateDatabase;
use crate::crates::CrateInfo;
use crate::lockfile_parser::parse_lockfile;

/// Execute the track command
/// Supports three modes:
/// 1. From crate name + version (downloads from crates.io)
/// 2. From Cargo.toml file (content format, any filename)
/// 3. From Cargo.lock file (content format, any filename)
pub fn execute_track(
    crate_name: Option<String>,
    version: Option<String>,
    from_file: Option<PathBuf>,
    output_dir: Option<PathBuf>,
    database_path: Option<PathBuf>,
    _action_file_path: Option<PathBuf>,
) -> Result<()> {
    // Use unified database path in ~/.config/takopack/
    let db_path =
        database_path.unwrap_or_else(|| crate::crate_database::get_default_database_path());

    // Determine which mode to use
    let lockfile_path = if let Some(file_path) = from_file {
        // Mode 2 or 3: From file (detect format by content)
        track_from_file(file_path)?
    } else if let Some(name) = crate_name {
        // Mode 1: From crate name + version
        track_from_crate(&name, version)?
    } else {
        anyhow::bail!("Either crate_name or --from-file must be specified");
    };

    // From here, the logic is the same for all modes
    process_lockfile(&lockfile_path, &db_path, output_dir)
}

/// Mode 1: Track from crate name + version (download from crates.io)
fn track_from_crate(crate_name: &str, version: Option<String>) -> Result<PathBuf> {
    log::info!(
        "Tracking dependencies for: {} {}",
        crate_name,
        version.as_deref().unwrap_or("(latest)")
    );
    log::info!("{}", "=".repeat(60));

    // Download and create CrateInfo
    log::info!(
        "Downloading crate: {} {}",
        crate_name,
        version.as_deref().unwrap_or("latest")
    );
    let crate_info = CrateInfo::new(crate_name, version.as_deref())?;

    let actual_version = crate_info.version().to_string();
    log::info!("✓ Resolved to version: {}", actual_version);

    // Extract to temporary directory
    log::info!("Extracting crate...");
    let temp_dir = tempfile::Builder::new()
        .prefix("takopack-track-")
        .tempdir()?;
    let extract_path = temp_dir
        .path()
        .join(format!("{}-{}", crate_name, actual_version));

    let mut crate_info_mut = crate_info;
    crate_info_mut.extract_crate(&extract_path)?;
    log::info!("✓ Extracted to: {}", extract_path.display());

    // Generate Cargo.lock
    // TODO: some crates may do not obey the semver rules, so may use the alreay exist Cargo.lock if present.
    log::info!("Generating Cargo.lock...");
    if !crate_info_mut.generate_cargo_lock(&extract_path)? {
        anyhow::bail!("Failed to generate Cargo.lock");
    }

    let lockfile_path = extract_path.join("Cargo.lock");
    log::info!("✓ Generated Cargo.lock");

    // Backup Cargo.lock to ~/cargo_back/origin/
    let backup_lockfile_path = crate::util::backup_cargo_lock(
        &lockfile_path,
        crate_name,
        &actual_version,
        Some("origin"),
    )?;

    Ok(backup_lockfile_path)
}

/// Mode 2/3: Track from local file (auto-detect format by content)
fn track_from_file(file_path: PathBuf) -> Result<PathBuf> {
    log::info!("Tracking dependencies from file: {}", file_path.display());
    log::info!("{}", "=".repeat(60));
    if !file_path.exists() {
        anyhow::bail!("File not found: {}", file_path.display());
    }

    // Read file content to determine type by format
    let content = fs::read_to_string(&file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

    // Detect file type by content format (not filename)
    if is_cargo_lock_format(&content) {
        // Mode 3: Cargo.lock format
        println!("✓ Detected Cargo.lock format (by content)");
        println!("✓ Using existing lockfile");
        Ok(file_path)
    } else if is_cargo_toml_format(&content) {
        // Mode 2: Cargo.toml format
        println!("✓ Detected Cargo.toml format (by content)");
        println!("✓ Generating Cargo.lock...");

        // Create a temporary directory to work in
        let temp_dir = tempfile::Builder::new()
            .prefix("takopack-track-toml-")
            .tempdir()?;

        let temp_toml = temp_dir.path().join("Cargo.toml");
        fs::copy(&file_path, &temp_toml)?;

        // Generate lockfile in temp directory
        generate_lockfile_for_toml(temp_dir.path())?;

        let lockfile_path = temp_dir.path().join("Cargo.lock");
        if !lockfile_path.exists() {
            anyhow::bail!("Failed to generate Cargo.lock");
        }
        let backup_lockfile_path =
            crate::util::backup_cargo_lock(&lockfile_path, "no_name", "latest", Some("temp"))?;

        Ok(backup_lockfile_path)
    } else {
        anyhow::bail!(
            "File format not recognized. Expected Cargo.toml or Cargo.lock format.\n\
             - Cargo.toml should contain [package] or [dependencies] sections\n\
             - Cargo.lock should contain [[package]] entries"
        );
    }
}

/// Detect if content is in Cargo.lock format
/// Checks for characteristic patterns in Cargo.lock files
fn is_cargo_lock_format(content: &str) -> bool {
    // Cargo.lock contains [[package]] entries
    // Also typically has version = 3 or similar at the top
    content.contains("[[package]]")
        || (content.contains("version =")
            && content.contains("name =")
            && content.contains("checksum ="))
}

/// Detect if content is in Cargo.toml format
/// Checks for characteristic sections in Cargo.toml files
fn is_cargo_toml_format(content: &str) -> bool {
    // Cargo.toml typically contains section headers
    content.contains("[package]")
        || content.contains("[dependencies]")
        || content.contains("[dev-dependencies]")
        || content.contains("[workspace]")
        || content.contains("[build-dependencies]")
}

/// Generate Cargo.lock for a Cargo.toml in a directory
fn generate_lockfile_for_toml(project_dir: &Path) -> Result<()> {
    use cargo::core::Workspace;
    use cargo::ops;
    use cargo::util::GlobalContext;

    let cargo_toml_path = project_dir.join("Cargo.toml");
    if !cargo_toml_path.exists() {
        anyhow::bail!("Cargo.toml not found in: {}", project_dir.display());
    }

    let gctx = GlobalContext::default()?;
    let ws = Workspace::new(&cargo_toml_path, &gctx)
        .with_context(|| format!("Failed to create workspace from {:?}", cargo_toml_path))?;

    ops::generate_lockfile(&ws)?;

    Ok(())
}

/// Common processing logic for all modes
fn process_lockfile(
    lockfile_path: &Path,
    db_path: &Path,
    output_dir: Option<PathBuf>,
) -> Result<()> {
    // Parse dependencies
    log::info!("Parsing dependencies...");
    let dep_graph = parse_lockfile(lockfile_path)?;
    println!(
        "✓ Parsed {} packages from dependency graph",
        dep_graph.len()
    );

    // Load or create database
    let mut db = if db_path.exists() {
        log::info!("Loading existing database: {}", db_path.display());
        CrateDatabase::from_file(db_path)?
    } else {
        log::info!("Creating new database");
        CrateDatabase::new()
    };

    let db_size_before = db.len();
    println!("✓ Database has {} entries", db_size_before);

    // Merge dependencies
    log::info!("Merging dependencies into database...");
    let needs_action = db.merge_dependency_graph(&dep_graph);

    println!("\n📊 Analysis Results:");
    println!(
        "  - Total packages in dependency graph: {}",
        dep_graph.len()
    );
    println!("  - Database entries before: {}", db_size_before);
    println!("  - Database entries after: {}", db.len());
    println!("  - New entries added: {}", db.len() - db_size_before);
    println!("  - Crates needing processing: {}", needs_action.len());

    // Save updated database
    db.to_file(db_path)?;
    println!("\n💾 Database saved to: {}", db_path.display());

    // Git commit the database changes (if back_db feature enabled)
    let crate_name = lockfile_path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("batch");
    let commit_msg = format!("add some crates for {}", crate_name);
    CrateDatabase::git_commit(db_path, &commit_msg);

    // Batch package crates that need action
    if !needs_action.is_empty() {
        println!("\n🆕 Crates that will be processed:");

        for (i, entry) in needs_action.iter().enumerate() {
            let marker = if entry.compatible { "✓" } else { "⚠" };
            println!(
                "  {:3}) {} {} v{}",
                i + 1,
                marker,
                entry.name,
                entry.version
            );
        }

        println!("\n🚀 Starting batch package...");
        println!("{}", "=".repeat(60));

        // Use provided output directory or create one with timestamp
        let output_dir = if let Some(dir) = output_dir {
            dir
        } else {
            let timestamp = Local::now().format("%Y%m%d_%H%M%S").to_string();
            PathBuf::from(format!("track_{}", timestamp))
        };

        fs::create_dir_all(&output_dir)
            .with_context(|| format!("Failed to create output directory: {:?}", output_dir))?;

        println!("Output directory: {}\n", output_dir.display());

        // Batch package all crates in needs_action
        let mut succeeded = 0;
        let mut failed = 0;

        for (idx, entry) in needs_action.iter().enumerate() {
            println!(
                "[{}/{}] Processing: {} {}",
                idx + 1,
                needs_action.len(),
                entry.name,
                entry.version
            );

            match crate::util::process_single_crate(
                &entry.name,
                &entry.version.to_string(),
                &output_dir,
                Some(&dep_graph), // Pass dep_graph for lockfile dependencies
            ) {
                Ok(_) => {
                    succeeded += 1;
                    println!("  ✓ Successfully packaged {} {}", entry.name, entry.version);
                }
                Err(e) => {
                    failed += 1;
                    eprintln!(
                        "  ✗ Failed to package {} {}: {:?}",
                        entry.name, entry.version, e
                    );
                }
            }
        }

        // Print summary
        println!("\n{}", "=".repeat(60));
        println!("Batch Processing Summary");
        println!("{}", "=".repeat(60));
        println!("Total packages processed: {}", needs_action.len());
        println!("Successfully packaged:    {}", succeeded);
        println!("Failed:                   {}", failed);
        println!("\nOutput directory: {}", output_dir.display());
        println!("{}", "=".repeat(60));
    } else {
        println!("\n✅ No new crates need to be processed!");
        println!("All dependencies are already in the database.");
    }

    Ok(())
}
