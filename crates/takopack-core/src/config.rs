use anyhow::Context;
use serde::Deserialize;
use serde::de::IgnoredAny;
use toml;

use crate::errors::*;

use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Debug, Clone)]
#[serde(default)]
pub struct Config {
    pub semver_suffix: bool,
    pub overlay: Option<PathBuf>,
    pub excludes: Option<Vec<String>>,
    pub whitelist: Option<Vec<String>>,
    pub crate_src_path: Option<PathBuf>,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub collapse_features: bool,

    pub source: Option<SourceOverride>,
    pub packages: HashMap<String, PackageOverride>,

    #[serde(rename = "ruyispec")]
    _ruyispec: Option<toml::Value>,
    #[serde(rename = "registry")]
    _registry: Option<toml::Value>,

    #[serde(flatten)]
    pub unknown_fields: HashMap<String, IgnoredAny>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct SourceOverride {
    homepage: Option<String>,

    #[serde(flatten)]
    pub unknown_fields: HashMap<String, IgnoredAny>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct PackageOverride {
    summary: Option<String>,
    description: Option<String>,
    extra_lines: Option<Vec<String>>,

    #[serde(flatten)]
    pub unknown_fields: HashMap<String, IgnoredAny>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            semver_suffix: false,
            overlay: None,
            excludes: None,
            whitelist: None,
            crate_src_path: None,
            summary: None,
            description: None,
            collapse_features: false,
            source: None,
            packages: HashMap::new(),
            _ruyispec: None,
            _registry: None,
            unknown_fields: HashMap::new(),
        }
    }
}

impl Config {
    pub fn load() -> Result<(Option<PathBuf>, Config)> {
        let path = find_takopack_toml();
        match path {
            Some(path) => {
                let config = Config::parse(&path)
                    .with_context(|| format!("failed to parse {}", path.display()))?;
                Ok((Some(path), config))
            }
            None => Ok((None, Config::default())),
        }
    }

    pub fn parse(src: &Path) -> Result<Config> {
        let mut config_file = File::open(src)?;
        let mut content = String::new();
        config_file.read_to_string(&mut content)?;

        let config: Config = toml::from_str(&content)?;

        let mut unknown_fields = Vec::new();

        for field in config.unknown_fields.keys() {
            unknown_fields.push(field.clone());
        }

        if let Some(ref source) = config.source {
            for field in source.unknown_fields.keys() {
                unknown_fields.push(format!("source.{}", field));
            }
        }

        for field in config.packages.keys() {
            if PackageKey::from_key(field).is_none() {
                unknown_fields.push(format!("packages.{}", field));
            }
        }

        for (name, package) in &config.packages {
            for field in package.unknown_fields.keys() {
                unknown_fields.push(format!("packages.{}.{}", name, field));
            }
        }

        if !unknown_fields.is_empty() {
            takopack_warn!(
                "Warning: Unknown fields in {}: {:?}",
                src.display(),
                unknown_fields
            );
            takopack_warn!("         These fields will be ignored. Please check for typos.");
        }

        Ok(config)
    }

    pub fn overlay_dir(&self, config_path: Option<&Path>) -> Option<PathBuf> {
        Some(config_path?.parent()?.join(self.overlay.as_ref()?))
    }

    pub fn crate_src_path(&self, config_path: Option<&Path>) -> Option<PathBuf> {
        Some(config_path?.parent()?.join(self.crate_src_path.as_ref()?))
    }

    pub fn source_archive_excludes(&self) -> Option<&Vec<String>> {
        self.excludes.as_ref()
    }

    pub fn source_archive_whitelist(&self) -> Option<&Vec<String>> {
        self.whitelist.as_ref()
    }

    pub fn homepage(&self) -> Option<&str> {
        Some(self.source.as_ref()?.homepage.as_ref()?)
    }

    pub fn configured_packages(&'_ self) -> impl Iterator<Item = PackageKey<'_>> {
        self.packages.keys().flat_map(|k| PackageKey::from_key(k))
    }

    fn with_package<'a, T, F: FnOnce(&'a PackageOverride) -> Option<T>>(
        &'a self,
        key: PackageKey,
        f: F,
    ) -> Option<T> {
        self.packages.get(&key.key_string()[..]).and_then(f)
    }

    pub fn package_summary(&self, key: PackageKey) -> Option<&str> {
        self.with_package(key, |pkg| pkg.summary.as_deref())
    }

    pub fn package_description(&self, key: PackageKey) -> Option<&str> {
        self.with_package(key, |pkg| pkg.description.as_deref())
    }

    pub fn package_extra_lines(&self, key: PackageKey) -> Option<&Vec<String>> {
        self.with_package(key, |pkg| pkg.extra_lines.as_ref())
    }
}

#[derive(Clone, Copy)]
pub enum PackageKey<'a> {
    BareLib,
    FeatureLib(&'a str),
    Extra(&'a str),
}

impl<'a> PackageKey<'a> {
    pub fn feature(f: &'a str) -> PackageKey<'a> {
        use self::PackageKey::*;
        if f.is_empty() { BareLib } else { FeatureLib(f) }
    }

    pub fn from_key(k: &'a str) -> Option<PackageKey<'a>> {
        use self::PackageKey::*;
        Some(match k {
            "lib" => BareLib,
            _ => {
                if let Some(feature) = k.strip_prefix("lib+") {
                    FeatureLib(feature)
                } else if let Some(package) = k.strip_prefix("extra+") {
                    Extra(package)
                } else {
                    return None;
                }
            }
        })
    }

    fn key_string(&self) -> Cow<'static, str> {
        use self::PackageKey::*;
        match self {
            BareLib => "lib".into(),
            FeatureLib(feature) => format!("lib+{}", feature).into(),
            Extra(package) => format!("extra+{}", package).into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TakopackToml {
    pub ruyispec: Option<RuyispecConfig>,
    pub registry: Option<RegistryConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuyispecConfig {
    pub local_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegistryConfig {
    pub local_path: Option<PathBuf>,
}

pub fn resolve_ruyispec_dir(explicit: Option<&Path>, use_config: bool) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return require_directory(path, "explicit ruyispec path");
    }
    if !use_config {
        takopack_bail!("ruyispec directory is required unless --ruyispec is used");
    }

    let (config_path, config) = load_takopack_toml()?.ok_or_else(|| {
        anyhow::anyhow!(
            "missing takopack.toml; create one with [ruyispec].local_path or pass an explicit path"
        )
    })?;
    let local_path = config
        .ruyispec
        .and_then(|ruyispec| ruyispec.local_path)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{} does not define [ruyispec].local_path",
                config_path.display()
            )
        })?;
    let local_path = if local_path.is_absolute() {
        local_path
    } else {
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(local_path)
    };

    require_directory(&local_path, "ruyispec.local_path")
}

pub fn resolve_registry_dir(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }

    if let Some((config_path, config)) = load_takopack_toml()? {
        if let Some(local_path) = config.registry.and_then(|registry| registry.local_path) {
            return Ok(resolve_config_relative_path(&config_path, local_path));
        }
    }

    default_registry_dir()
}

pub fn default_registry_dir() -> Result<PathBuf> {
    let data_dir = dirs::data_dir().ok_or_else(|| {
        anyhow::anyhow!("cannot determine XDG_DATA_HOME / home directory for default registry path")
    })?;
    Ok(data_dir.join("takopack").join("cargo-registry"))
}

pub fn ruyispec_package_root(ruyispec_dir: &Path) -> PathBuf {
    let specs_dir = ruyispec_dir.join("SPECS");
    if specs_dir.is_dir() {
        specs_dir
    } else {
        ruyispec_dir.to_path_buf()
    }
}

pub fn load_takopack_toml() -> Result<Option<(PathBuf, TakopackToml)>> {
    let Some(path) = find_takopack_toml() else {
        return Ok(None);
    };
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let config =
        toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some((path, config)))
}

fn find_takopack_toml() -> Option<PathBuf> {
    let current = PathBuf::from("takopack.toml");
    if current.is_file() {
        return Some(current);
    }

    dirs::config_dir()
        .map(|dir| dir.join("takopack").join("takopack.toml"))
        .filter(|path| path.is_file())
}

fn resolve_config_relative_path(config_path: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(path)
    }
}

fn require_directory(path: &Path, label: &str) -> Result<PathBuf> {
    if !path.is_dir() {
        takopack_bail!("{} is not a directory: {}", label, path.display());
    }
    Ok(path.to_path_buf())
}

pub fn testing_ruzt() -> bool {
    std::env::var_os("takopack_TESTING_RUZT").as_deref() == Some(OsStr::new("1"))
}
