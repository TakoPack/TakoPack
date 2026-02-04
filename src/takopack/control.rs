use core::panic;
use std::collections::HashMap;
#[cfg(not(test))]
use std::env::{self, VarError};
use std::fmt::{self, Write};

#[cfg(not(test))]
use anyhow::{format_err, Error};
use cargo::core::Dependency;
use semver::Version;
use textwrap::fill;

use crate::config::{self, Config, PackageKey};
use crate::errors::*;

#[derive(Default, Debug)]
pub struct BuildDeps {
    pub(crate) build_depends: Vec<String>,
    pub(crate) build_depends_indep: Vec<String>,
    pub(crate) build_depends_arch: Vec<String>,
}

pub struct Source {
    name: String,
    version: String,
    full_version: String, // Full version including build metadata (e.g., "0.7.5+spec-1.1.0")
    section: String,
    priority: String,
    maintainer: String,
    uploaders: Vec<String>,
    standards: String,
    build_deps: BuildDeps,
    vcs_git: String,
    vcs_browser: String,
    homepage: String,
    crate_name: String,
    license: String,
    requires_root: Option<String>,
    download_url: String,
    sha256: Option<String>, // SHA256 hash of the downloaded crate file
}

pub struct Package {
    name: String,
    arch: String,
    multi_arch: Option<String>,
    section: Option<String>,
    depends: Vec<String>,
    crate_deps: Vec<CrateDep>, // Structured dependencies for crate() format
    recommends: Vec<String>,
    suggests: Vec<String>,
    provides: Vec<String>,
    breaks: Vec<String>,
    replaces: Vec<String>,
    conflicts: Vec<String>,
    summary: Description,
    description: Description,
    extra_lines: Vec<String>,
    feature: Option<String>, // Original feature name, None for base package
    crate_name: Option<String>, // Original crate name for proper feature extraction
    all_features: Vec<String>, // All features available in Cargo.toml (only for base package)
}

pub struct Description {
    pub prefix: String,
    pub suffix: String,
}

#[derive(Clone, Debug)]
pub struct CrateDep {
    pub crate_name: String,
    pub feature: Option<String>,
    pub version: Option<String>, // Version constraint like ">= 1.0.228"
}

impl CrateDep {
    pub fn new(crate_name: String, feature: Option<String>) -> Self {
        Self {
            crate_name,
            feature,
            version: None,
        }
    }

    pub fn new_with_version(
        crate_name: String,
        feature: Option<String>,
        version: Option<String>,
    ) -> Self {
        Self {
            crate_name,
            feature,
            version,
        }
    }

    pub fn to_crate_format(&self) -> String {
        let crate_base = self.crate_name.replace('_', "-");
        // Extract compatibility version from version constraint
        // E.g., ">= 0.6.2" -> "0.6", ">= 2.2.1" -> "2.0", ">= 1.13" -> "1.0"
        // For prerelease: ">= 0.26.0-beta.1" -> "0.26.0-beta.1" (full version with - separator)
        // log::debug!("before version_num: {} {:?}", crate_base, &self.version);

        let crate_with_compat = if let Some(version_str) = &self.version {
            // the option deps won't appear in here.
            // println!("Version crate_name string: {} {}", self.crate_name, version_str);
            // Clean version string first: remove wildcards and other invalid RPM chars
            // "0.4.*" -> "0.4.0", ">= 0.4.*" -> ">= 0.4.0"
            let cleaned_version_str = version_str.replace(".*", ".0").replace('*', "0");

            // Extract version number from constraint (e.g., ">= 0.6.2" -> "0.6.2", ">= 1.13" -> "1.13")
            let version_num = cleaned_version_str
                .trim()
                .trim_start_matches(">=")
                .trim_start_matches("=")
                .trim_start_matches(">")
                .trim_start_matches("<")
                .trim();
            // log::debug!("after version_num: {} {}", crate_base, version_num);
            // TODO: there the version_num maybe the full version like "0.7.5+spec-1.1.0" and "0.26.0-beta.1"
            // But it depends on how the author writes the dependencies in Cargo.toml
            // Remove build metadata (+xxx) for version string
            // "0.7.5+spec-1.1.0" -> "0.7.5", "1.0.1+wasi-0.2.4" -> "1.0.1"
            let version_without_build = version_num.split('+').next().unwrap_or(version_num);
            // Check if version has prerelease (AFTER removing build metadata)
            // Build metadata should not affect the crate name - only prerelease should
            if version_without_build.contains('-') {
                // For prerelease versions, use full version
                format!("{}-{}", crate_base, version_without_build)
            } else {
                // For regular versions (including those with build metadata), use compatibility version
                // Normalize version_num: if only major.minor (like "1.4"), add .0 for patch
                let version_num = if version_without_build.split('.').count() == 2 {
                    format!("{}.0", version_without_build)
                } else {
                    version_without_build.to_string()
                };
                if let Ok(ver) = Version::parse(&version_num) {
                    let compat_version = crate::util::calculate_compat_version(&ver);
                    format!("{}-{}", crate_base, compat_version)
                } else {
                    // bitflags-2.0 the bytemuck is 1.4,so parse error.
                    // so we add ".0" on the other branch. this code must't be reached.
                    panic!(
                        "Failed to parse version '{}' for crate '{}'",
                        version_num, crate_base
                    );
                    // format!("{}-{}", crate_base, version_without_build)
                }
            }
        } else {
            crate_base
        };
        let crate_part = if let Some(feature) = &self.feature {
            let feature_base = feature.replace('_', "-").to_lowercase();
            // imagequant-sys-4.0.3 the feature starts with _
            let feature_base_trimmed = feature_base.trim_start_matches('-');
            format!("crate({}/{})", crate_with_compat, feature_base_trimmed)
        } else {
            format!("crate({})", crate_with_compat)
        };

        if let Some(version) = &self.version {
            // Clean version string for output: remove wildcards, build metadata, and other invalid RPM chars
            // "0.4.*" -> "0.4.0", ">= 0.4.*" -> ">= 0.4.0"
            // "0.7.5+spec-1.1.0" -> "0.7.5"
            let cleaned_version = version
                .replace(".*", ".0")
                .replace('*', "0")
                .split('+')
                .next()
                .unwrap_or(version)
                .to_string();
            format!("{} {}", crate_part, cleaned_version)
        } else {
            crate_part
        }
    }
}

impl Description {
    pub fn new(prefix: String, suffix: String) -> Self {
        Self { prefix, suffix }
    }
}

pub struct PkgTest {
    name: String,
    crate_name: String,
    feature: String,
    version: String,
    extra_test_args: Vec<String>,
    depends: Vec<String>,
    extra_restricts: Vec<String>,
    architecture: Vec<String>,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // Package name uses hyphens instead of underscores
        let pkg_name = self.crate_name.replace('_', "-");

        // Check if full_version contains build metadata
        let has_build_metadata = self.full_version.contains('+');

        // Calculate compatibility version following Rust semver rules
        // 0.x.y -> 0.x, 1.x.y -> 1.0
        // BUT: if version has build metadata, use full version instead
        let compat_version = if let Ok(ver) = Version::parse(&self.version) {
            crate::util::calculate_compat_version(&ver)
        } else {
            self.version.clone()
        };

        // For RPM Version field, strip prerelease suffix (RPM doesn't allow '-' in Version)
        // e.g., "0.26.0-beta.1" -> "0.26.0"
        let rpm_version = if let Ok(ver) = Version::parse(&self.version) {
            if !ver.pre.is_empty() {
                // Strip prerelease part
                format!("{}.{}.{}", ver.major, ver.minor, ver.patch)
            } else {
                self.version.clone()
            }
        } else {
            self.version.clone()
        };

        // Define macro with original crate name (may contain underscores)
        writeln!(f, "%global crate_name {}", self.crate_name)?;
        writeln!(f, "%global full_version {}", self.full_version)?;
        writeln!(f, "%global pkgname {}-{}", pkg_name, compat_version)?;

        writeln!(f)?;

        writeln!(f, "Name:           rust-{}-{}", pkg_name, compat_version)?;
        writeln!(f, "Version:        {}", rpm_version)?;
        writeln!(f, "Release:        %autorelease")?;
        writeln!(f, "Summary:        Rust crate \"{}\"", self.crate_name)?;
        writeln!(
            f,
            "License:        {}",
            if !self.license.is_empty() {
                &self.license
            } else {
                "FIXME"
            }
        )?;
        writeln!(
            f,
            "URL:            {}",
            if !self.homepage.is_empty() {
                &self.homepage
            } else {
                "FIXME"
            }
        )?;
        // url is already git repo.
        // if !self.vcs_git.is_empty() {
        //     writeln!(f, "VCS:            {}", self.vcs_git)?;
        // } else {
        //     writeln!(f, "# No git repo found.")?;
        // }
        if let Some(ref hash) = self.sha256 {
            writeln!(f, "#!RemoteAsset:  sha256:{}", hash)?;
        } else {
            writeln!(f, "#!RemoteAsset:  sha256:")?;
        }
        // Use full version (including build metadata) in Source URL
        // This is needed for crates like toml_datetime with versions like "0.7.5+spec-1.1.0"
        writeln!(f, "Source:         https://crates.io/api/v1/crates/%{{crate_name}}/%{{full_version}}/download#/%{{name}}-%{{version}}.tar.gz")?;
        writeln!(f, "BuildSystem:    rustcrates")?;
        writeln!(f, "")?;
        writeln!(f, "BuildRequires:  rust-rpm-macros")?;
        writeln!(f)?;
        Ok(())
    }
}

fn clean_package_name(pkg_name: &str) -> String {
    // Convert old format to new format and remove version numbers
    // librust-proc-macro2-1+default-dev -> rust-proc-macro2-default
    // librust-heck-0.5+default-devel -> rust-heck-default

    let mut name = pkg_name.to_string();

    // Remove -devel or -dev suffix
    if name.ends_with("-devel") {
        name = name[..name.len() - 6].to_string();
    } else if name.ends_with("-dev") {
        name = name[..name.len() - 4].to_string();
    }

    // Replace librust- with rust-
    if name.starts_with("librust-") {
        name = name.replacen("librust-", "rust-", 1);
    }

    // Replace + with -
    name = name.replace('+', "-");

    // Remove version numbers from package name
    let parts: Vec<&str> = name.split('-').collect();
    let cleaned_parts: Vec<&str> = parts
        .into_iter()
        .filter(|part| {
            // Keep part if it doesn't look like a version number
            // Version numbers are: pure digits, or digits with dots (like 0.5, 1.0, etc)
            if part.is_empty() {
                return false;
            }
            // Check if it's a version number: starts with digit and only contains digits/dots
            if part.chars().next().map_or(false, |c| c.is_ascii_digit()) {
                if part.chars().all(|c| c.is_ascii_digit() || c == '.') {
                    return false; // This is a version number, filter it out
                }
            }
            true
        })
        .collect();

    cleaned_parts.join("-")
}

fn convert_to_crate_format(pkg_name: &str) -> String {
    // Convert rust-{crate}-{feature} to crate({crate}/{feature})
    // Convert rust-{crate} to crate({crate})
    // Examples:
    //   rust-serde-core-result -> crate(serde-core/result)
    //   rust-serde -> crate(serde)
    //   rust-serde-derive-default -> crate(serde-derive/default)

    let cleaned = clean_package_name(pkg_name);

    // Remove rust- prefix
    let without_prefix = if cleaned.starts_with("rust-") {
        &cleaned[5..]
    } else {
        &cleaned
    };

    // Try to find the last component as feature
    // We need to identify crate name vs feature name
    // Pattern: {crate}-{feature} where feature is typically a single word
    // Common features: default, alloc, std, core, etc.

    let parts: Vec<&str> = without_prefix.split('-').collect();
    if parts.len() > 1 {
        // Check if last part looks like a feature name
        // Common feature patterns: default, alloc, std, core, result, rc, etc.
        let last = parts[parts.len() - 1];
        let common_features = [
            "default", "alloc", "std", "core", "result", "rc", "unstable", "derive", "nightly",
            "serde", "tokio", "async", "sync",
        ];

        // If it's a common feature or all parts together don't form a known crate
        // assume last part is a feature
        if common_features.contains(&last) || parts.len() >= 3 {
            let crate_parts = &parts[..parts.len() - 1];
            let crate_name = crate_parts.join("-");
            format!("crate({}/{})", crate_name, last)
        } else {
            // No feature, just crate name
            format!("crate({})", without_prefix)
        }
    } else {
        // Single part, just crate name
        format!("crate({})", without_prefix)
    }
}

fn extract_version_from_pkg_name(pkg_name: &str) -> Option<String> {
    // Extract version from package names like:
    // "rust-pyo3-build-config-0.26+default-dev" -> Some(">= 0.26.0")
    // "rust-serde-1.0+default-dev" -> Some(">= 1.0.0")

    let mut name = pkg_name.trim().to_string();

    // Remove -dev suffix
    if name.ends_with("-dev") {
        name = name[..name.len() - 4].to_string();
    }

    // Remove rust- or librust- prefix
    if name.starts_with("librust-") {
        name = name[8..].to_string();
    } else if name.starts_with("rust-") {
        name = name[5..].to_string();
    }

    // Remove feature part (after +)
    if let Some(idx) = name.find('+') {
        name = name[..idx].to_string();
    }

    // Now we have something like "pyo3-build-config-0.26" or "serde-1.0"
    // Find the last part that looks like a version number
    let parts: Vec<&str> = name.split('-').collect();
    if let Some(last_part) = parts.last() {
        // Check if it's a version number (starts with digit)
        if last_part
            .chars()
            .next()
            .map_or(false, |c| c.is_ascii_digit())
        {
            // Assume it's a major.minor version, add .0 for patch
            if last_part.contains('.') {
                return Some(format!(">= {}.0", last_part));
            } else {
                return Some(format!(">= {}.0.0", last_part));
            }
        }
    }

    None
}

/// Parse semver VersionReq string to extract lower bound version
/// Examples:
///   "^0.9" -> Some("0.9.0")
///   ">=1.21, <2.0" -> Some("1.21.0")  
///   "^0.2.62" -> Some("0.2.62")
///   "*" -> None
fn parse_version_req_to_lower_bound(version_req: &str) -> Option<String> {
    let req_str = version_req.trim();

    // Handle wildcard
    if req_str == "*" || req_str.is_empty() {
        return None;
    }

    // Split by comma for multiple requirements, take the first one (usually the lower bound)
    let first_req = req_str.split(',').next()?.trim();

    // Remove operators: ^, ~, >=, >, =
    let version_part = if first_req.starts_with(">=") {
        &first_req[2..].trim()
    } else if first_req.starts_with('>') || first_req.starts_with('=') || first_req.starts_with('~')
    {
        &first_req[1..].trim()
    } else if first_req.starts_with('^') {
        &first_req[1..].trim()
    } else {
        first_req
    };

    // Parse version and normalize it
    let parts: Vec<&str> = version_part.split('.').collect();
    match parts.len() {
        1 => Some(format!("{}.0.0", parts[0])),
        2 => Some(format!("{}.{}.0", parts[0], parts[1])),
        _ => Some(version_part.to_string()),
    }
}

fn parse_deb_package_to_crate_dep(pkg_name: &str) -> Option<CrateDep> {
    parse_package_name_simple(pkg_name)
}

/// 简化的包名解析函数
/// 规则：
/// 1. 去掉开头的 rust- 或 librust-
/// 2. 去掉末尾的 -dev 或 -devel
/// 3. + 号后面是 feature 名称
/// 4. + 号左边，最右边的 - 后面如果是版本号（数字和点组成），则为版本
/// 5. 版本号前面的是 crate 名称
///
/// 示例：
///   rust-md-5-0.10+default-dev -> CrateDep { crate_name: "md-5", feature: Some("default") }
///   rust-serde-1.0+derive-dev -> CrateDep { crate_name: "serde", feature: Some("derive") }
///   rust-utf-8-0.7-dev -> CrateDep { crate_name: "utf-8", feature: None }
///   rust-proc-macro2-1-dev -> CrateDep { crate_name: "proc-macro2", feature: None }
fn parse_package_name_simple(pkg_name: &str) -> Option<CrateDep> {
    let mut name = pkg_name.trim();

    // 1. 去掉开头的 rust- 或 librust-
    if name.starts_with("librust-") {
        name = &name[8..];
    } else if name.starts_with("rust-") {
        name = &name[5..];
    } else {
        return None;
    }

    // 2. 去掉末尾的 -dev 或 -devel
    if name.ends_with("-devel") {
        name = &name[..name.len() - 6];
    } else if name.ends_with("-dev") {
        name = &name[..name.len() - 4];
    }

    // 3. 按 + 分割，提取 feature
    let (crate_and_version, feature) = if let Some(plus_idx) = name.find('+') {
        let left = &name[..plus_idx];
        let right = &name[plus_idx + 1..];
        (left, Some(right.to_string()))
    } else {
        (name, None)
    };

    // 4. 从右往左找最后一个看起来像版本号的段
    // 版本号特征：只包含数字和点，且以数字开头
    let parts: Vec<&str> = crate_and_version.split('-').collect();

    // 找到最后一个版本号段的位置
    let version_idx = parts.iter().rposition(|part| {
        !part.is_empty()
            && part.chars().next().map_or(false, |c| c.is_ascii_digit())
            && part.chars().all(|c| c.is_ascii_digit() || c == '.')
    });

    // 5. 提取 crate 名称（版本号前面的所有部分）
    let crate_name = if let Some(idx) = version_idx {
        if idx > 0 {
            parts[..idx].join("-")
        } else {
            // 如果版本号在第一个位置，这不太可能，但保险起见
            crate_and_version.to_string()
        }
    } else {
        // 没有找到版本号，整个就是 crate 名称
        crate_and_version.to_string()
    };

    if crate_name.is_empty() {
        return None;
    }

    Some(CrateDep::new(crate_name, feature))
}

fn extract_feature_from_package_name(pkg_name: &str, crate_base: &str) -> Option<String> {
    // Extract feature name from package names like:
    //   "rust-serde-default" with crate_base "serde" -> Some("default")
    //   "rust-serde-std" with crate_base "serde" -> Some("std")
    //   "rust-serde" with crate_base "serde" -> None (no feature)

    // Do NOT clean package name - it's already in the right format from deb_feature_name()
    let pkg = pkg_name;

    // Remove rust- prefix
    let without_prefix = if pkg.starts_with("rust-") {
        &pkg[5..]
    } else {
        return None;
    };

    // Check if it starts with our crate name
    let crate_with_dash = format!("{}-", crate_base);
    if without_prefix == crate_base {
        // Just the crate name, no feature
        None
    } else if without_prefix.starts_with(&crate_with_dash) {
        // Has a feature suffix
        Some(without_prefix[crate_with_dash.len()..].to_string())
    } else {
        None
    }
}

impl fmt::Display for Package {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // Determine relative name for package naming
        // If self.feature is Some, this IS a feature package - always treat as such
        let relative_name = if let Some(feature) = &self.feature {
            feature // Always use feature name when feature is present
        } else {
            "" // Base package (no feature)
        };

        if relative_name.is_empty() {
            // Base package - no %package directive, no Summary (already in Source)
        } else {
            // Feature package - use relative name
            writeln!(f)?;
            let feature_name = base_deb_name(relative_name);
            let feature_base_trimmed = feature_name.trim_start_matches('-');
            writeln!(f, "%package     -n %{{name}}+{}", feature_base_trimmed)?;
            writeln!(f, "Summary:        {}", self.summary)?;
        }

        if !self.crate_deps.is_empty() {
            // Output dependencies in crate() format using to_crate_format()
            // Deduplicate: if same crate appears multiple times, keep only the one with version
            use std::collections::BTreeMap;
            let mut dep_map: BTreeMap<String, String> = BTreeMap::new();

            for dep in &self.crate_deps {
                let formatted = dep.to_crate_format();
                // Extract just the crate(...) part as key for deduplication
                let key = if let Some(space_pos) = formatted.find(' ') {
                    formatted[..space_pos].to_string()
                } else {
                    formatted.clone()
                };
                // Keep the one with version constraint (longer string usually means it has version)
                match dep_map.get(&key) {
                    Some(existing) if formatted.len() > existing.len() => {
                        dep_map.insert(key, formatted);
                    }
                    None => {
                        dep_map.insert(key, formatted);
                    }
                    _ => {
                        // Keep existing (already has version)
                    }
                }
            }

            // Output deduplicated dependencies
            for formatted in dep_map.values() {
                writeln!(f, "Requires:       {}", formatted)?;
            }
        }
        // Add Provides in crate() format
        // Main package: Provides: crate(serde)
        // Feature package: Provides: crate(serde/alloc)
        // Also parse self.provides for additional features (e.g., std provides default)
        if let Some(crate_name) = &self.crate_name {
            let crate_base = crate_name.replace('_', "-");
            use std::collections::HashSet;
            let mut provided_features = HashSet::new();

            if relative_name.is_empty() {
                // Main package provides crate(%{pkgname})
                // println!("{:?}", self.crate_name);
                writeln!(f, "Provides:       crate(%{{pkgname}})")?;
                // if self.crate_name.as_deref() == Some("md-5")
                //     || self.crate_name.as_deref() == Some("utf-8")
                // {
                //     writeln!(f, "Provides:       crate(%{{pkgname}}/default)")?;
                // }

                // For the main package, output all features from Cargo.toml
                // This ensures all features are listed even if they don't have dependencies
                // Convert to lowercase and normalize (underscore to hyphen)
                if !self.all_features.is_empty() {
                    let mut sorted_features: Vec<_> = self.all_features.iter().collect();
                    sorted_features.sort();
                    for feature in sorted_features {
                        // Skip empty feature (represents main package)
                        if !feature.is_empty() {
                            let feature_normalized = feature.replace('_', "-").to_lowercase();
                            // println!("Feature provided: {}", feature_normalized);
                            provided_features.insert(feature_normalized.clone());
                            writeln!(
                                f,
                                "Provides:       crate(%{{pkgname}}/{})",
                                feature_normalized.trim_start_matches('-')
                            )?;
                        }
                    }
                }

                // Also parse self.provides for additional features this main package provides
                // (in case there are any edge cases not covered by all_features)
                for provide in &self.provides {
                    // Extract feature name from package like "rust-clap-unstable-derive-ui-tests"
                    if let Some(additional_feature) =
                        extract_feature_from_package_name(provide, &crate_base)
                    {
                        let additional_feature_lower = additional_feature.to_lowercase();
                        provided_features.insert(additional_feature_lower);
                    }
                }

                // Output any additional unique provides that weren't in all_features (already lowercase)
                let all_features_lower: HashSet<_> = self
                    .all_features
                    .iter()
                    .map(|f| f.replace('_', "-").to_lowercase())
                    .collect();
                let mut extra_features: Vec<_> = provided_features
                    .into_iter()
                    .filter(|f| !all_features_lower.contains(f))
                    .collect();
                extra_features.sort();
                for feature in extra_features {
                    // Check if not already in all_features
                    if !self.all_features.contains(&feature)
                        && !self.all_features.contains(&feature.replace('-', "_"))
                    {
                        let feature_base_trimmed = feature.trim_start_matches('-');
                        writeln!(
                            f,
                            "Provides:       crate(%{{pkgname}}/{})",
                            feature_base_trimmed
                        )?;
                    }
                }
            } else {
                // Feature package provides crate(name/feature)
                // Normalize feature name to use hyphens
                let feature_base = base_deb_name(relative_name);
                provided_features.insert(feature_base.clone());

                // Parse self.provides for additional features
                // e.g., "rust-serde-default" means this package also provides the "default" feature
                for provide in &self.provides {
                    // Extract feature name from package like "rust-serde-default"
                    if let Some(additional_feature) =
                        extract_feature_from_package_name(provide, &crate_base)
                    {
                        provided_features.insert(additional_feature);
                    }
                }

                // Output all unique provides in sorted order for consistency
                let mut features: Vec<_> = provided_features.into_iter().collect();
                features.sort();
                for feature in features {
                    let feature_base_trimmed = feature.trim_start_matches('-');
                    writeln!(
                        f,
                        "Provides:       crate(%{{pkgname}}/{})",
                        feature_base_trimmed
                    )?;
                }
            }
        }
        if !self.replaces.is_empty() {
            for rep in &self.replaces {
                let cleaned = rep.split('(').next().unwrap_or(rep).trim();
                let clean_name = clean_package_name(cleaned);
                writeln!(f, "Obsoletes:      {}", clean_name)?;
            }
        }
        if !self.breaks.is_empty() {
            for brk in &self.breaks {
                let cleaned = brk.split('(').next().unwrap_or(brk).trim();
                let clean_name = clean_package_name(cleaned);
                writeln!(f, "Conflicts:      {}", clean_name)?;
            }
        }

        for line in &self.extra_lines {
            writeln!(f, "{}", line)?;
        }

        // Use same logic to determine relative name for description
        // If self.feature is Some, this IS a feature package - always treat as such
        let relative_name = if let Some(feature) = &self.feature {
            feature // Always use feature name when feature is present
        } else {
            "" // Base package (no feature)
        };

        writeln!(f)?;
        if relative_name.is_empty() {
            writeln!(f, "%description")?;
        } else {
            let feature_name = base_deb_name(relative_name);
            let feature_base_trimmed = feature_name.trim_start_matches('-');
            writeln!(f, "%description -n %{{name}}+{}", feature_base_trimmed)?;
        }
        let description = format!("{}", &self.description);
        for line in description.lines() {
            writeln!(f, "{}", line.trim())?;
        }

        Ok(())
    }
}

impl fmt::Display for PkgTest {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let extra_args = if self.extra_test_args.is_empty() {
            "".into()
        } else {
            format!(" {}", self.extra_test_args.join(" "))
        };
        writeln!(
            f,
            "Test-Command: /usr/share/cargo/bin/cargo-auto-test {} {} --all-targets{}",
            self.crate_name, self.version, extra_args,
        )?;
        writeln!(f, "Features: test-name={}:{}", &self.name, &self.feature)?;
        // TODO: drop the below workaround when rust-lang/cargo#5133 is fixed.
        // The downside of our present work-around is that more dependencies
        // must be installed, which makes it harder to actually run the tests
        let cargo_bug_fixed = false;
        let default_deps = if cargo_bug_fixed { &self.name } else { "@" };

        let depends = if self.depends.is_empty() {
            "".into()
        } else {
            format!(", {}", self.depends.join(", "))
        };
        writeln!(f, "Depends: dh-cargo (>= 31){}, {}", depends, default_deps)?;

        let restricts = if self.extra_restricts.is_empty() {
            "".into()
        } else {
            format!(", {}", self.extra_restricts.join(", "))
        };
        writeln!(
            f,
            "Restrictions: allow-stderr, skip-not-installable{}",
            restricts,
        )?;
        if !self.architecture.is_empty() {
            writeln!(f, "Architecture: {}", self.architecture.join(" "))?;
        }
        Ok(())
    }
}

impl Source {
    pub fn pkg_prefix() -> &'static str {
        if config::testing_ruzt() {
            // avoid accidentally installing official packages during tests
            "ruzt"
        } else {
            "rust"
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        basename: &str,
        version: &str,
        name_suffix: Option<&str>,
        crate_name: &str,
        home: &str,
        repository: &str,
        license: &str,
        lib: bool,
        maintainer: String,
        uploaders: Vec<String>,
        build_deps: BuildDeps,
        requires_root: Option<String>,
        download_url: String,
        full_version: String,   // Full version including build metadata
        sha256: Option<String>, // SHA256 hash of downloaded crate file
    ) -> Result<Source> {
        let pkgbase = match name_suffix {
            None => basename.to_string(),
            Some(suf) => format!("{}{}", basename, suf),
        };
        let section = if lib {
            "rust"
        } else {
            "FIXME-IN-THE-SOURCE-SECTION"
        };
        let priority = "optional".to_string();
        let vcs_browser = format!(
            "https://salsa.debian.org/rust-team/takopack-conf/tree/master/src/{}",
            pkgbase
        );
        // Use repository from Cargo.toml if available
        let vcs_git = if !repository.is_empty() {
            if repository.starts_with("http://") || repository.starts_with("https://") {
                format!("git:{}", repository)
            } else {
                repository.to_string()
            }
        } else {
            String::new()
        };
        Ok(Source {
            name: dsc_name(&pkgbase),
            version: version.to_string(),
            full_version,
            section: section.to_string(),
            priority,
            maintainer,
            uploaders,
            standards: "4.7.2".to_string(),
            build_deps,
            vcs_git,
            vcs_browser,
            homepage: home.to_string(),
            crate_name: crate_name.to_string(),
            license: license.to_string(),
            requires_root,
            download_url,
            sha256,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn apply_overrides(&mut self, config: &Config) {
        if let Some(section) = config.section() {
            self.section = section.to_string();
        }

        if let Some(policy) = config.policy_version() {
            self.standards = policy.to_string();
        }

        self.build_deps.build_depends.extend(
            config
                .build_depends()
                .into_iter()
                .flatten()
                .map(String::to_string),
        );
        self.build_deps.build_depends_arch.extend(
            config
                .build_depends_arch()
                .into_iter()
                .flatten()
                .map(String::to_string),
        );
        self.build_deps.build_depends_indep.extend(
            config
                .build_depends_indep()
                .into_iter()
                .flatten()
                .map(String::to_string),
        );
        let bdeps_ex = config
            .build_depends_excludes()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        self.build_deps
            .build_depends
            .retain(|x| !bdeps_ex.contains(x));

        self.build_deps
            .build_depends_arch
            .retain(|x| !bdeps_ex.contains(x));

        self.build_deps
            .build_depends_indep
            .retain(|x| !bdeps_ex.contains(x));

        if let Some(homepage) = config.homepage() {
            self.homepage = homepage.to_string();
        }

        if let Some(vcs_git) = config.vcs_git() {
            self.vcs_git = vcs_git.to_string();
        }

        if let Some(vcs_browser) = config.vcs_browser() {
            self.vcs_browser = vcs_browser.to_string();
        }
    }
}

impl Package {
    pub fn pkg_prefix() -> &'static str {
        if config::testing_ruzt() {
            // avoid accidentally installing official packages during tests
            "ruzt"
        } else {
            "rust"
        }
    }

    /// Apply lockfile dependencies
    pub fn apply_lockfile_deps(&mut self, lockfile_deps: &HashMap<String, semver::Version>) {
        for dep in &mut self.crate_deps {
            let name_dash = dep.crate_name.replace('_', "-");
            if let Some(ver) = lockfile_deps
                .get(&dep.crate_name)
                .or_else(|| lockfile_deps.get(&name_dash))
            {
                // some optionnal deps won't appear into the lockfile,like bytemuck in bitflags of alacrittty
                // println!("Applying lockfile version for {}: {}", dep.crate_name, ver);
                // Version handling:
                // - Regular versions (no prerelease, no build): use as-is for later compat calculation
                // - Prerelease versions (with -): use full version
                // - Build metadata versions (with +): use full version including build metadata
                let version_str = if !ver.build.is_empty() {
                    // Has build metadata: include it (e.g., "0.9.11+spec-1.1.0")
                    format!("{}.{}.{}+{}", ver.major, ver.minor, ver.patch, ver.build)
                } else if !ver.pre.is_empty() {
                    // Has prerelease: include it (e.g., "0.26.0-beta.1")
                    format!("{}.{}.{}-{}", ver.major, ver.minor, ver.patch, ver.pre)
                } else {
                    // Regular version (e.g., "1.0.228")
                    format!("{}.{}.{}", ver.major, ver.minor, ver.patch)
                };
                dep.version = Some(format!(">= {}", version_str));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        basename: &str,
        name_suffix: Option<&str>,
        version: &Version,
        summary: Description,
        description: Description,
        feature: Option<&str>,
        f_deps: Vec<&str>,
        o_deps: Vec<String>,
        ori_deps: Vec<Dependency>,
        f_provides: Vec<&str>,
        f_recommends: Vec<&str>,
        f_suggests: Vec<&str>,
        all_features: Vec<String>, // All features from Cargo.toml (only used for base package)
    ) -> Result<Package> {
        // for d in &o_deps {
        //     println!("dep: {}", d);
        //     // dep: rust-winapi-x86-64-pc-windows-gnu-0.4+default-dev
        // }
        let pkgbase = match name_suffix {
            None => basename.to_string(),
            Some(suf) => format!("{}{}", basename, suf),
        };
        let deb_feature2 = &|p: &str, f: &str| match f {
            "" => deb_name(p),
            _ => deb_feature_name(p, f),
        };
        let deb_feature = &|f: &str| deb_feature2(&pkgbase, f);

        let filter_provides = &|x: Vec<&str>| {
            x.into_iter()
                .filter(|f| !f_provides.contains(f))
                .map(deb_feature)
                .collect()
        };
        let (recommends, suggests) = match feature {
            Some(_) => (vec![], vec![]),
            None => (filter_provides(f_recommends), filter_provides(f_suggests)),
        };

        // Provides for all possible versions, see:
        // https://bugs.debian.org/cgi-bin/bugreport.cgi?bug=901827#35
        // https://wiki.debian.org/Teams/RustPackaging/Policy#Package_provides
        let mut provides = vec![];
        // Only provide unversioned package names for RPM spec format
        let version_suffixes = ["".to_string()];
        for suffix in version_suffixes.iter() {
            // don't provide unversioned variants in semver-suffix packages
            if name_suffix.is_some() && suffix.is_empty() {
                continue;
            };

            let p = format!("{}{}", basename, suffix);
            provides.push(deb_feature2(&p, feature.unwrap_or("")));
            provides.extend(f_provides.iter().map(|f| deb_feature2(&p, f)));
        }
        let provides_self = deb_feature(feature.unwrap_or(""));
        // rust dropped Vec::remove_item for annoying reasons, the below is
        // an unofficialy recommended replacement from the RFC #40062
        let i = provides.iter().position(|x| *x == *provides_self);
        i.map(|i| provides.remove(i));

        let mut depends = vec![];
        let mut crate_deps = vec![];

        if feature.is_some() && !f_deps.contains(&"") {
            // in dh-cargo we symlink /usr/share/doc/{$feature => $main} pkg
            // so we always need this direct dependency, even if the feature
            // only indirectly depends on the bare library via another
            depends.push(deb_feature(""));
            crate_deps.push(CrateDep::new("%{pkgname}".to_string(), None));
        }

        // Build crate_deps from f_deps (internal feature dependencies, no version)
        for f in &f_deps {
            depends.push(deb_feature(f));
            if f.is_empty() {
                // Empty feature means dependency on base crate
                crate_deps.push(CrateDep::new("%{pkgname}".to_string(), None));
            } else {
                // Feature dependency
                crate_deps.push(CrateDep::new("%{pkgname}".to_string(), Some(f.to_string())));
            }
        }

        // Parse o_deps (external crate dependencies) into CrateDep format
        // These are external crates, so they get version constraints
        // Use a map to collect all constraints for each crate+feature combination
        use std::collections::HashMap;
        let mut temp_deps: HashMap<(String, Option<String>), Vec<String>> = HashMap::new();

        for o_dep in o_deps.iter() {
            depends.push(o_dep.clone());

            // Parse package name and version from strings like:
            // "rust-serde-core-1.0+result-dev (>= 1.0.228-~~)"
            // "rust-proc-macro2-1-dev (>= 1.0-~~)"
            // "rust-clippy-lints-0.0+default-dev (>= 0.0.112-~~)" and (<< 0.0.113-~~)
            // Note: RPM spec only supports ">=" constraints, so we skip "<< " constraints
            let (pkg_name, version_constraint) = if let Some(idx) = o_dep.find(" (") {
                let pkg = o_dep[..idx].trim();
                let ver_part = &o_dep[idx + 2..]; // Skip " ("

                // Only extract ">=" constraints, ignore "<<" (upper bound)
                // RPM spec format only supports lower bounds with ">="
                let version = if let Some(start_idx) = ver_part.find(">= ") {
                    let ver_str = &ver_part[start_idx + 3..];
                    if let Some(end_idx) = ver_str.find(|c| c == '-' || c == ')') {
                        Some(format!(">= {}", &ver_str[..end_idx]))
                    } else {
                        None
                    }
                } else if ver_part.contains("<< ") {
                    // Skip upper bound constraints - not supported in RPM spec
                    continue;
                } else {
                    None
                };
                (pkg, version)
            } else {
                // No version in parentheses, will get version from ori_deps later
                (o_dep.trim(), None)
            };
            // println!("pkg_name: {}", pkg_name);
            // Extract crate name and feature from package name
            if let Some(mut crate_dep) = parse_deb_package_to_crate_dep(pkg_name) {
                // The parsed crate name may not be accurate (especially with numeric parts like x86-64, base64, sha2, etc.)
                // Find the real crate name and version from ori_deps by matching normalized names
                let normalized_parsed_name = crate_dep.crate_name.replace('-', "_");
                // println!("normalized_parsed_name: {}", normalized_parsed_name);
                // Search for matching dependency in ori_deps
                if let Some(matching_dep) = ori_deps.iter().find(|dep| {
                    let dep_name = dep.package_name().replace('-', "_");
                    // println!("dep_name: {}", dep_name);
                    dep_name == normalized_parsed_name
                }) {
                    // Use the real crate name from Cargo metadata
                    let real_crate_name = matching_dep.package_name().to_string();
                    // println!("real: {real_crate_name}");
                    crate_dep.crate_name = real_crate_name;

                    // If no version constraint from takopack package string, get it from ori_deps
                    if version_constraint.is_none() {
                        let version_req = matching_dep.version_req();
                        // Convert semver VersionReq to our format
                        // For simplicity, extract the minimum version from the requirement
                        let version_str = format!("{}", version_req);
                        if !version_str.is_empty() && version_str != "*" {
                            // Parse version requirement like "^0.9" or ">=1.0, <2.0"
                            // For now, extract the first number sequence as minimum version
                            if let Some(version) = parse_version_req_to_lower_bound(&version_str) {
                                crate_dep.version = Some(format!(">= {}", version));
                            }
                        }
                    } else {
                        crate_dep.version = version_constraint.clone();
                    }
                } else if let Some(ver) = version_constraint {
                    // Couldn't find in ori_deps, use the version from takopack package
                    crate_dep.version = Some(ver);
                }
                let dep_crate_base = crate_dep.crate_name.replace('_', "-");
                let self_crate_base = basename.replace('_', "-");
                if dep_crate_base != self_crate_base {
                    // Collect all version constraints for this crate+feature
                    let key = (crate_dep.crate_name.clone(), crate_dep.feature.clone());
                    let entry = temp_deps.entry(key).or_insert_with(Vec::new);
                    if let Some(ver) = &crate_dep.version {
                        entry.push(ver.clone());
                    }
                }
            }
        }

        // Now merge the constraints and create CrateDep entries
        for ((crate_name, feature), constraints) in temp_deps {
            // Separate >= and < constraints
            let mut lower_bounds = vec![];
            let mut upper_bounds = vec![];

            for constraint in constraints {
                if constraint.starts_with(">=") {
                    // Extract version from ">= x.y.z"
                    if let Some(ver_str) = constraint.strip_prefix(">= ") {
                        lower_bounds.push(ver_str.trim().to_string());
                    }
                } else if constraint.starts_with('<') {
                    // Extract version from "< x.y.z"
                    if let Some(ver_str) = constraint.strip_prefix("< ") {
                        upper_bounds.push(ver_str.trim().to_string());
                    }
                }
            }

            // Helper function to compare version strings
            let parse_version = |v: &str| -> Vec<u64> {
                v.split('.').filter_map(|s| s.parse::<u64>().ok()).collect()
            };

            let compare_versions = |a: &str, b: &str| -> std::cmp::Ordering {
                let va = parse_version(a);
                let vb = parse_version(b);
                va.cmp(&vb)
            };

            // Find maximum lower bound
            let lower_bound = if !lower_bounds.is_empty() {
                lower_bounds.sort_by(|a, b| compare_versions(a, b));
                Some(format!(">= {}", lower_bounds.last().unwrap()))
            } else {
                None
            };

            // Find minimum upper bound
            let upper_bound = if !upper_bounds.is_empty() {
                upper_bounds.sort_by(|a, b| compare_versions(a, b));
                Some(format!("< {}", upper_bounds.first().unwrap()))
            } else {
                None
            };

            // Combine constraints
            let version = match (lower_bound, upper_bound) {
                (Some(l), Some(u)) => Some(format!("{}, {}", l, u)),
                (Some(l), None) => Some(l),
                (None, Some(u)) => Some(u),
                (None, None) => None,
            };

            crate_deps.push(CrateDep {
                crate_name,
                feature,
                version,
            });
        }
        let mut breaks = vec![];
        let mut replaces = vec![];
        if name_suffix.is_some() && feature.is_none() {
            // B+R needs to be set on "real" package, not virtual ones
            // constrain by "next" version, so that it is possible to install a newer,
            // non-suffixed package at the same time
            let mut next_version = version.clone();
            next_version.patch += 1;
            breaks.push(format!("{} (<< {}~)", deb_name(basename), next_version));
            replaces.push(format!("{} (<< {}~)", deb_name(basename), next_version));
        }
        let conflicts = vec![];

        Ok(Package {
            name: match feature {
                None => deb_name(&pkgbase),
                Some(f) => deb_feature_name(&pkgbase, f),
            },
            arch: "any".to_string(),
            // This is the best but not ideal option for us.
            //
            // Currently takopack M-A spec has a deficiency where a package X that
            // build-depends on a (M-A:foreign+arch:all) package that itself
            // depends on an arch:any package Z, will pick up the BUILD_ARCH of
            // package Z instead of the HOST_ARCH. This is because we currently
            // have no way of telling dpkg to use HOST_ARCH when checking that the
            // dependencies of Y are satisfied, which is done at install-time
            // without any knowledge that we're about to do a cross-compile. It
            // is also problematic to tell dpkg to "accept any arch" because of
            // the presence of non-M-A:same packages in the archive, that are not
            // co-installable - different arches of Z might be depended-upon by
            // two conflicting chains. (dpkg has so far chosen not to add an
            // exception for the case where package Z is M-A:same co-installable).
            //
            // The recommended work-around for now from the dpkg developers is to
            // make our packages arch:any M-A:same even though this results in
            // duplicate packages in the takopack archive. For very large crates we
            // will eventually want to make takopack generate -data packages that
            // are arch:all and have the arch:any -dev packages depend on it.
            multi_arch: Some("same".to_string()),
            section: None,
            depends,
            crate_deps,
            recommends,
            suggests,
            provides,
            breaks,
            replaces,
            conflicts,
            summary,
            description,
            extra_lines: vec![],
            feature: feature.map(|s| s.to_string()),
            crate_name: Some(basename.to_string()),
            all_features,
        })
    }

    pub fn new_bin(
        basename: &str,
        name_suffix: Option<&str>,
        section: Option<&str>,
        summary: Description,
        description: Description,
    ) -> Self {
        let (name, mut provides) = match name_suffix {
            None => (basename.to_string(), vec![]),
            Some(suf) => (
                format!("{}{}", basename, suf),
                vec![format!("{} (= ${{binary:Version}})", basename)],
            ),
        };
        provides.push("${cargo:Provides}".to_string());
        Package {
            name,
            arch: "any".to_string(),
            multi_arch: None,
            section: section.map(|s| s.to_string()),
            depends: vec![
                "${misc:Depends}".to_string(),
                "${shlibs:Depends}".to_string(),
                "${cargo:Depends}".to_string(),
            ],
            crate_deps: vec![],
            recommends: vec!["${cargo:Recommends}".to_string()],
            suggests: vec!["${cargo:Suggests}".to_string()],
            provides,
            breaks: vec![],
            replaces: vec![],
            conflicts: vec![],
            summary,
            description,
            extra_lines: vec![
                "Built-Using: ${cargo:Built-Using}".to_string(),
                "Static-Built-Using: ${cargo:Static-Built-Using}".to_string(),
            ],
            feature: None,
            crate_name: None,
            all_features: vec![],
        }
    }

    pub fn new_extra(name: String) -> Self {
        Package {
            name,
            arch: Default::default(),
            multi_arch: Default::default(),
            section: Default::default(),
            depends: Default::default(),
            crate_deps: Default::default(),
            recommends: Default::default(),
            suggests: Default::default(),
            provides: Default::default(),
            breaks: Default::default(),
            replaces: Default::default(),
            conflicts: Default::default(),
            summary: Description::new(Default::default(), Default::default()),
            description: Description::new(Default::default(), Default::default()),
            extra_lines: Default::default(),
            feature: None,
            crate_name: None,
            all_features: vec![],
        }
    }

    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    #[allow(dead_code)]
    fn write_description(&self, out: &mut fmt::Formatter) -> fmt::Result {
        writeln!(out, "Description: {}", &self.summary)?;
        let description = format!("{}", &self.description);
        for line in fill(description.trim(), 79).lines() {
            let line = line.trim_end();
            if line.is_empty() {
                writeln!(out, " .")?;
            } else if line.starts_with("- ") {
                writeln!(out, "  {}", line)?;
            } else {
                writeln!(out, " {}", line)?;
            }
        }
        Ok(())
    }

    #[allow(clippy::result_unit_err)]
    pub fn summary_check_len(&self) -> std::result::Result<(), ()> {
        if self.summary.prefix.len() <= 80 {
            Ok(())
        } else {
            Err(())
        }
    }

    pub fn apply_overrides(&mut self, config: &Config, key: PackageKey, f_provides: Vec<&str>) {
        if let Some(section) = config.package_section(key) {
            self.section = Some(section.to_string());
        }
        self.summary
            .apply_overrides(&config.summary, config.package_summary(key));
        self.description
            .apply_overrides(&config.description, config.package_description(key));

        self.depends.extend(config::package_field_for_feature(
            |x| config.package_depends(x),
            key,
            &f_provides,
        ));
        self.recommends.extend(config::package_field_for_feature(
            |x| config.package_recommends(x),
            key,
            &f_provides,
        ));
        self.suggests.extend(config::package_field_for_feature(
            |x| config.package_suggests(x),
            key,
            &f_provides,
        ));
        self.provides.extend(config::package_field_for_feature(
            |x| config.package_provides(x),
            key,
            &f_provides,
        ));
        self.breaks.extend(config::package_field_for_feature(
            |x| config.package_breaks(x),
            key,
            &f_provides,
        ));
        self.replaces.extend(config::package_field_for_feature(
            |x| config.package_replaces(x),
            key,
            &f_provides,
        ));
        self.conflicts.extend(config::package_field_for_feature(
            |x| config.package_conflicts(x),
            key,
            &f_provides,
        ));
        self.extra_lines.extend(
            config
                .package_extra_lines(key)
                .into_iter()
                .flatten()
                .map(|s| s.to_string()),
        );
        if let Some(architecture) = config.package_architecture(key) {
            self.arch = architecture.join(" ");
        }
        if let Some(multi_arch) = config.package_multi_arch(key) {
            self.multi_arch = Some(multi_arch.to_owned());
        }
    }
}

impl Description {
    fn apply_overrides(&mut self, global: &Option<String>, per_package: Option<&str>) {
        if let Some(per_package) = per_package {
            self.prefix = per_package.to_string();
            self.suffix = "".to_string();
        } else if let Some(global) = &global {
            self.prefix = global.into();
        }
    }
}
impl fmt::Display for Description {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}{}", &self.prefix, self.suffix)
    }
}

impl PkgTest {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: &str,
        crate_name: &str,
        feature: &str,
        version: &str,
        extra_test_args: Vec<&str>,
        depends: &[String],
        extra_restricts: Vec<&str>,
        architecture: &[&str],
    ) -> Result<PkgTest> {
        Ok(PkgTest {
            name: name.to_string(),
            crate_name: crate_name.to_string(),
            feature: feature.to_string(),
            version: version.to_string(),
            extra_test_args: extra_test_args.iter().map(|x| x.to_string()).collect(),
            depends: depends.to_vec(),
            extra_restricts: extra_restricts.iter().map(|x| x.to_string()).collect(),
            architecture: architecture.iter().map(|x| x.to_string()).collect(),
        })
    }
}

/// Translates a semver into a takopack-format upstream version.
/// Omits the build metadata, and uses a ~ before the prerelease version so it
/// compares earlier than the subsequent release.
pub fn deb_upstream_version(v: &Version) -> String {
    let mut s = format!("{}.{}.{}", v.major, v.minor, v.patch);
    if !v.pre.is_empty() {
        // Use '-' instead of '~' for prerelease versions in RPM spec
        write!(s, "-{}", v.pre.as_str()).unwrap();
    }
    s
}

pub fn base_deb_name(crate_name: &str) -> String {
    crate_name.replace('_', "-").to_lowercase()
}

pub fn dsc_name(name: &str) -> String {
    format!("{}-{}", Source::pkg_prefix(), base_deb_name(name))
}

pub fn deb_name(name: &str) -> String {
    format!("{}-{}", Package::pkg_prefix(), base_deb_name(name))
}

pub fn deb_feature_name(name: &str, feature: &str) -> String {
    format!(
        "{}-{}-{}",
        Package::pkg_prefix(),
        base_deb_name(name),
        base_deb_name(feature)
    )
}

/// Retrieve one of a series of environment variables, and provide a friendly error message for
/// non-UTF-8 values.
#[cfg(not(test))]
fn get_envs(keys: &[&str]) -> Result<Option<String>> {
    for key in keys {
        match env::var(key) {
            Ok(val) => {
                return Ok(Some(val));
            }
            Err(e @ VarError::NotUnicode(_)) => {
                return Err(Error::from(e)
                    .context(format!("Environment variable ${} not valid UTF-8", key)));
            }
            Err(VarError::NotPresent) => {}
        }
    }
    Ok(None)
}

#[cfg(test)]
pub(crate) fn get_deb_author() -> Result<String> {
    Ok("takopack Test <takopack@example.com>".to_string())
}

/// Determine a name and email address from environment variables.
#[cfg(not(test))]
pub fn get_deb_author() -> Result<String> {
    let name = get_envs(&["DEBFULLNAME", "NAME"])?.ok_or_else(|| {
        format_err!("Unable to determine your name; please set $DEBFULLNAME or $NAME")
    })?;
    let email = get_envs(&["DEBEMAIL", "EMAIL"])?.ok_or_else(|| {
        format_err!("Unable to determine your email; please set $DEBEMAIL or $EMAIL")
    })?;
    Ok(format!("{} <{}>", name, email))
}
