//! resolve-check subcommand.
//!
//! This verifies that a single Cargo crate can be resolved using only the
//! TakoPack local directory registry.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use cargo::core::{Shell, Workspace};
use cargo::ops;
use cargo::util::GlobalContext;
use semver::Version;
use serde_derive::Serialize;
use toml::Value;

use crate::errors::Result;
use crate::local_package::materialize_manifest_backed_temp_crate;

#[derive(Debug, Clone)]
pub struct ResolveReport {
    pub manifest: PathBuf,
    pub registry_dir: PathBuf,
    pub lock_packages: Vec<LockPackage>,
}

#[derive(Debug, Clone)]
pub struct LockPackage {
    pub name: String,
    pub version: Version,
    pub source: Option<String>,
}

struct PreparedSingleCrate {
    manifest: PathBuf,
    registry_dir: PathBuf,
    temp_manifest: PathBuf,
    _temp_project: tempfile::TempDir,
}

/// Run the `resolve-check` subcommand.
///
/// Returns an exit code: 0 means Cargo resolved successfully, 1 means the
/// dependency resolver failed against the selected local registry.
pub fn run_resolve_check(path: &Path, registry: Option<&Path>) -> Result<i32> {
    let prepared = prepare_single_crate(path, registry)?;

    println!("Resolve check");
    println!("  manifest: {}", prepared.manifest.display());
    println!("  registry: {}", prepared.registry_dir.display());
    println!();
    io::stdout()
        .flush()
        .context("failed to flush resolve-check header")?;

    match resolve_prepared_single_crate(&prepared, false) {
        Ok(_) => {
            println!("Result: ok");
            Ok(0)
        }
        Err(err) => {
            println!("Result: failed");
            eprintln!("{err:?}");
            Ok(1)
        }
    }
}

pub fn resolve_single_crate(path: &Path, registry: Option<&Path>) -> Result<ResolveReport> {
    let prepared = prepare_single_crate(path, registry)?;
    resolve_prepared_single_crate(&prepared, true)
}

fn prepare_single_crate(path: &Path, registry: Option<&Path>) -> Result<PreparedSingleCrate> {
    let manifest = resolve_manifest(path)?;
    validate_single_crate_manifest(&manifest)?;

    let registry_dir = crate::config::resolve_registry_dir(registry)?;
    if !registry_dir.is_dir() {
        takopack_bail!(
            "local registry directory does not exist: {}\n\
             Run `takopack cargo registry-sync` first, or pass --registry DIR.",
            registry_dir.display()
        );
    }
    let registry_dir = registry_dir
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", registry_dir.display()))?;

    let temp_project =
        tempfile::tempdir().context("failed to create temporary resolve-check crate")?;
    let temp_manifest = materialize_manifest_backed_temp_crate(&manifest, temp_project.path())?
        .canonicalize()
        .context("failed to canonicalize temporary Cargo.toml")?;

    Ok(PreparedSingleCrate {
        manifest,
        registry_dir,
        temp_manifest,
        _temp_project: temp_project,
    })
}

fn resolve_prepared_single_crate(
    prepared: &PreparedSingleCrate,
    quiet: bool,
) -> Result<ResolveReport> {
    let lockfile = cargo_resolve(&prepared.temp_manifest, &prepared.registry_dir, quiet)?;
    let lock_packages = lock_packages_from_lockfile(&lockfile)?;
    Ok(ResolveReport {
        manifest: prepared.manifest.clone(),
        registry_dir: prepared.registry_dir.clone(),
        lock_packages,
    })
}

fn resolve_manifest(path: &Path) -> Result<PathBuf> {
    let path = path
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", path.display()))?;

    if path.is_dir() {
        let manifest = path.join("Cargo.toml");
        if !manifest.is_file() {
            takopack_bail!("Cargo.toml not found in directory: {}", path.display());
        }
        return Ok(manifest);
    }

    if path.is_file() {
        return Ok(path);
    }

    takopack_bail!(
        "path is neither a directory nor a Cargo.toml file: {}",
        path.display()
    );
}

fn validate_single_crate_manifest(manifest: &Path) -> Result<()> {
    let content = fs::read_to_string(manifest)
        .with_context(|| format!("failed to read {}", manifest.display()))?;
    let parsed: Value = toml::from_str(&content)
        .with_context(|| format!("failed to parse {}", manifest.display()))?;

    if parsed.get("workspace").is_some() {
        takopack_bail!(
            "resolve-check currently supports only a single crate; workspace manifests are not supported: {}",
            manifest.display()
        );
    }

    if parsed.get("package").is_none() {
        takopack_bail!(
            "Cargo.toml does not define a [package]: {}",
            manifest.display()
        );
    }

    Ok(())
}

fn cargo_resolve(manifest: &Path, registry_dir: &Path, quiet: bool) -> Result<PathBuf> {
    let cargo_home = make_cargo_home(registry_dir)?;
    let gctx = make_global_context(cargo_home.path(), quiet)?;
    let ws = Workspace::new(manifest, &gctx)
        .with_context(|| format!("failed to open Cargo workspace at {}", manifest.display()))?;

    ops::generate_lockfile(&ws).context("cargo resolve failed")?;
    let lockfile = ws.root().join("Cargo.lock");

    // Keep cargo_home alive until after Cargo has finished resolving.
    drop(gctx);
    drop(cargo_home);
    Ok(lockfile)
}

fn lock_packages_from_lockfile(lockfile: &Path) -> Result<Vec<LockPackage>> {
    let content = fs::read_to_string(lockfile)
        .with_context(|| format!("failed to read generated {}", lockfile.display()))?;
    let doc: Value = toml::from_str(&content)
        .with_context(|| format!("failed to parse generated {}", lockfile.display()))?;
    let packages = doc
        .get("package")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("generated Cargo.lock has no package array"))?;

    let mut parsed = Vec::new();
    for package in packages {
        let Some(package) = package.as_table() else {
            continue;
        };
        let Some(name) = package.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(version) = package.get("version").and_then(Value::as_str) else {
            continue;
        };
        let version = Version::parse(version)
            .with_context(|| format!("failed to parse lockfile version {}", version))?;
        let source = package
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_string);
        parsed.push(LockPackage {
            name: name.to_string(),
            version,
            source,
        });
    }

    Ok(parsed)
}

fn make_cargo_home(registry_dir: &Path) -> Result<tempfile::TempDir> {
    let cargo_home = tempfile::tempdir().context("failed to create temporary CARGO_HOME")?;
    let config = CargoHomeConfig::new(registry_dir);
    let config_content = toml::to_string(&config).context("failed to render Cargo config")?;
    fs::write(cargo_home.path().join("config.toml"), config_content)
        .context("failed to write temporary Cargo config")?;
    Ok(cargo_home)
}

fn make_global_context(cargo_home: &Path, quiet: bool) -> Result<GlobalContext> {
    let cwd = std::env::current_dir().context("failed to resolve current directory")?;
    let mut gctx = GlobalContext::new(Shell::new(), cwd, cargo_home.to_path_buf());
    let target_dir: Option<PathBuf> = None;
    gctx.configure(
        0,     // verbose
        quiet, // quiet
        None,  // color
        false, // frozen
        false, // locked
        true,  // offline
        &target_dir,
        &[], // unstable flags
        &[], // cli config
    )?;
    Ok(gctx)
}

#[derive(Debug, Serialize)]
struct CargoHomeConfig {
    source: BTreeMap<String, CargoSourceConfig>,
    net: CargoNetConfig,
}

impl CargoHomeConfig {
    fn new(registry_dir: &Path) -> Self {
        let mut source = BTreeMap::new();
        source.insert(
            "crates-io".to_string(),
            CargoSourceConfig {
                replace_with: Some("takopack-local".to_string()),
                directory: None,
            },
        );
        source.insert(
            "takopack-local".to_string(),
            CargoSourceConfig {
                replace_with: None,
                directory: Some(registry_dir.display().to_string()),
            },
        );

        Self {
            source,
            net: CargoNetConfig { offline: true },
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
struct CargoSourceConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    replace_with: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    directory: Option<String>,
}

#[derive(Debug, Serialize)]
struct CargoNetConfig {
    offline: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_workspace_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = tmp.path().join("Cargo.toml");
        fs::write(
            &manifest,
            r#"
[workspace]
members = ["member"]
"#,
        )
        .unwrap();

        let err = validate_single_crate_manifest(&manifest).unwrap_err();
        assert!(err
            .to_string()
            .contains("workspace manifests are not supported"));
    }

    #[test]
    fn resolves_against_explicit_directory_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = tmp.path().join("registry");
        let foo = registry.join("foo-1.0.0");
        fs::create_dir_all(foo.join("src")).unwrap();
        fs::write(
            foo.join("Cargo.toml"),
            r#"
[package]
name = "foo"
version = "1.0.0"
edition = "2021"
"#,
        )
        .unwrap();
        fs::write(foo.join("src/lib.rs"), "").unwrap();
        fs::write(
            foo.join(".cargo-checksum.json"),
            r#"{"files":{},"package":null}"#,
        )
        .unwrap();

        let project = tmp.path().join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join("Cargo.toml"),
            r#"
[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
foo = "1"
"#,
        )
        .unwrap();

        let code = run_resolve_check(&project, Some(&registry)).unwrap();
        assert_eq!(code, 0);

        let report = resolve_single_crate(&project, Some(&registry)).unwrap();
        assert_eq!(report.lock_packages.len(), 2);
        assert!(report.lock_packages.iter().any(|package| {
            package.name == "foo"
                && package
                    .source
                    .as_deref()
                    .is_some_and(|source| source.starts_with("registry+"))
        }));
    }
}
