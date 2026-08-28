//! Source normalization: turn a high-level crate *source* into something the
//! cargo backend can load.
//!
//! A source describes *where* a crate comes from. The trait [`CrateSource`]
//! makes any source loadable; the concrete structs ([`RegistrySource`],
//! [`LocalPathSource`], [`ManifestSource`]) implement it; [`Source`] is an enum
//! newtype over those structs for cases where you need a single owned value
//! that can be any of them.
//!
//! The only side effect here is writing to a private [`tempfile::TempDir`]
//! (for [`ManifestSource`]) that is returned to the caller and kept alive by
//! the resulting [`CrateModel`]. Nothing is written to the user's project and
//! no output is printed.

use std::path::{Path, PathBuf};

use crate::local::materialize_manifest_backed_temp_crate;

use super::error::{Error, Result};

/// A crate source: describes *where* a crate comes from.
///
/// Object-safe so sources can be composed behind the [`Source`] enum newtype.
pub trait CrateSource {
    /// Normalize this source into a form the loader can consume.
    fn prepare(&self) -> Result<PreparedSource>;
    /// A short human-readable description for diagnostics.
    fn describe(&self) -> String;
}

// ---------------------------------------------------------------------------
// Concrete sources
// ---------------------------------------------------------------------------

/// A crate from the crates.io registry, optionally pinned by a version
/// requirement.
///
/// NOTE: loading this source performs network access and writes to the cargo
/// cache.
#[derive(Debug, Clone)]
pub struct RegistrySource {
    pub name: String,
    pub version: Option<String>,
}

impl CrateSource for RegistrySource {
    fn prepare(&self) -> Result<PreparedSource> {
        Ok(PreparedSource::Registry {
            name: self.name.clone(),
            version: self.version.clone(),
        })
    }

    fn describe(&self) -> String {
        match &self.version {
            Some(v) => format!("{} {}", self.name, v),
            None => format!("{} (latest)", self.name),
        }
    }
}

/// A local directory containing a complete crate (with source code), loaded
/// through cargo's path source (which packages the crate into a real `.crate`).
#[derive(Debug, Clone)]
pub struct LocalPathSource {
    pub path: PathBuf,
    /// Crate name; read from `Cargo.toml` when `None`.
    pub name: Option<String>,
    /// Optional pinned version; `None` resolves via the path source.
    pub version: Option<String>,
}

impl CrateSource for LocalPathSource {
    fn prepare(&self) -> Result<PreparedSource> {
        let path = canonicalize(&self.path)?;
        let manifest_path = manifest_in_dir(&path)?;
        let name = match &self.name {
            Some(n) => n.clone(),
            None => read_package_name(&manifest_path)?,
        };
        Ok(PreparedSource::LocalPath {
            name,
            version: self.version.clone(),
            path,
        })
    }

    fn describe(&self) -> String {
        self.path.display().to_string()
    }
}

/// A single `Cargo.toml` file (possibly manifest-only, without source code),
/// materialized with placeholder files so cargo can load it.
#[derive(Debug, Clone)]
pub struct ManifestSource {
    pub path: PathBuf,
}

impl CrateSource for ManifestSource {
    fn prepare(&self) -> Result<PreparedSource> {
        let cargo_toml = canonicalize(&self.path)?;
        if !is_toml_file(&cargo_toml) {
            return Err(Error::message(format!(
                "manifest source must be a Cargo.toml file: {}",
                cargo_toml.display()
            )));
        }
        let workspace = tempfile::tempdir().map_err(|e| {
            Error::message(format!("failed to create temporary crate directory: {e}"))
        })?;
        let manifest = materialize_manifest_backed_temp_crate(&cargo_toml, workspace.path())
            .map_err(|e| {
                Error::backend(format!(
                    "failed to materialize manifest from {}: {e}",
                    cargo_toml.display()
                ))
            })?;
        Ok(PreparedSource::Manifest {
            manifest,
            workspace,
        })
    }

    fn describe(&self) -> String {
        self.path.display().to_string()
    }
}

// ---------------------------------------------------------------------------
// Enum newtype for dispatch
// ---------------------------------------------------------------------------

/// The set of crate sources as an enum, forwarding to the underlying struct's
/// [`CrateSource`] impl. Use this when you need a single owned value that can
/// be one of any source (e.g. from a CLI flag).
#[derive(Debug, Clone)]
pub enum Source {
    Registry(RegistrySource),
    LocalPath(LocalPathSource),
    Manifest(ManifestSource),
}

impl CrateSource for Source {
    fn prepare(&self) -> Result<PreparedSource> {
        match self {
            Source::Registry(s) => s.prepare(),
            Source::LocalPath(s) => s.prepare(),
            Source::Manifest(s) => s.prepare(),
        }
    }

    fn describe(&self) -> String {
        match self {
            Source::Registry(s) => s.describe(),
            Source::LocalPath(s) => s.describe(),
            Source::Manifest(s) => s.describe(),
        }
    }
}

impl From<RegistrySource> for Source {
    fn from(s: RegistrySource) -> Self {
        Source::Registry(s)
    }
}

impl From<LocalPathSource> for Source {
    fn from(s: LocalPathSource) -> Self {
        Source::LocalPath(s)
    }
}

impl From<ManifestSource> for Source {
    fn from(s: ManifestSource) -> Self {
        Source::Manifest(s)
    }
}

// ---------------------------------------------------------------------------
// Prepared form
// ---------------------------------------------------------------------------

/// The normalized form of a [`CrateSource`] that the loader can hand to cargo.
pub enum PreparedSource {
    /// A crates.io crate to resolve and download.
    Registry {
        name: String,
        version: Option<String>,
    },
    /// A complete local crate directory, packaged through cargo's path source.
    LocalPath {
        name: String,
        version: Option<String>,
        path: PathBuf,
    },
    /// A manifest materialized into a temporary workspace, with placeholder
    /// files for any manifest-referenced paths so cargo can load it.
    Manifest {
        manifest: PathBuf,
        workspace: tempfile::TempDir,
    },
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn canonicalize(path: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(path)
        .map_err(|e| Error::message(format!("failed to resolve path {}: {e}", path.display())))
}

fn is_toml_file(path: &Path) -> bool {
    path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("toml")
}

fn manifest_in_dir(dir: &Path) -> Result<PathBuf> {
    let manifest = dir.join("Cargo.toml");
    if !manifest.exists() {
        return Err(Error::message(format!(
            "Cargo.toml not found in: {}",
            dir.display()
        )));
    }
    Ok(manifest)
}

fn read_package_name(manifest_path: &Path) -> Result<String> {
    let content = std::fs::read_to_string(manifest_path)
        .map_err(|e| Error::message(format!("failed to read {}: {e}", manifest_path.display())))?;
    let value: toml::Value = toml::from_str(&content)
        .map_err(|e| Error::message(format!("failed to parse {}: {e}", manifest_path.display())))?;
    value
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(String::from)
        .ok_or_else(|| {
            Error::message(format!(
                "no [package].name found in {}",
                manifest_path.display()
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_describe_includes_version() {
        let src = RegistrySource {
            name: "foo".into(),
            version: Some("1.2".into()),
        };
        assert_eq!(src.describe(), "foo 1.2");
    }

    #[test]
    fn source_newtype_forwards_describe() {
        let src = Source::Registry(RegistrySource {
            name: "foo".into(),
            version: None,
        });
        assert_eq!(src.describe(), "foo (latest)");
    }
}
