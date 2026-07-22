use std::collections::HashMap;
use std::fmt::{self, Write};

use cargo::{core::Dependency, util::OptVersionReq};
use semver::Version;
use takopack_core::{
    config::{self, Config, PackageKey},
    errors::*,
};

use crate::{
    crates::dependency_is_runtime_candidate,
    rpm::spec::{
        self, CrateCapability, CrateRequirement, RequirementVersion, SpecPackage, SpecSource,
    },
};

pub struct Source {
    name: String,
    version: String,
    full_version: String, // Full version including build metadata (e.g., "0.7.5+spec-1.1.0")
    homepage: String,
    crate_name: String,
    license: String,
    sha256: Option<String>, // SHA256 hash of the downloaded crate file
    with_spdx: bool,
}

pub struct Package {
    name: String,
    crate_deps: Vec<CrateDep>, // Structured dependencies for crate() format
    crate_requires: Vec<CrateRequirement>, // Structured external crate requirements from Cargo.toml
    feature_provides: Vec<String>, // Structured Cargo feature aliases provided by this package
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
        spec::render_crate_requirement(&self.to_crate_requirement())
    }

    fn to_crate_requirement(&self) -> CrateRequirement {
        let crate_name = self.crate_name_with_compat();
        let requirement = if crate_name == "%{pkgname}" {
            RequirementVersion::Exact("%{version}".to_string())
        } else if let Some(version) = self.cleaned_version_requirement() {
            RequirementVersion::Range(version)
        } else {
            RequirementVersion::None
        };

        CrateRequirement {
            crate_name,
            feature: self.feature.clone(),
            requirement,
        }
    }

    fn crate_name_with_compat(&self) -> String {
        let crate_base = self.crate_name.replace('_', "-");
        // Extract compatibility version from version constraint
        // E.g., ">= 0.6.2" -> "0.6", ">= 2.2.1" -> "2", ">= 1.13" -> "1"
        // For prerelease: ">= 0.26.0-beta.1" -> "0.26.0-beta.1" (full version with - separator)
        // log::debug!("before version_num: {} {:?}", crate_base, &self.version);

        if let Some(version_str) = &self.version {
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
                    let compat_version = takopack_core::util::calculate_compat_version(&ver);
                    format!("{}-{}", crate_base, compat_version)
                } else {
                    // Legacy fallback only: structured Cargo requirements use
                    // cargo_dep_crate_name instead. If this old path sees a
                    // shape it cannot parse, keep an unversioned capability
                    // rather than aborting spec rendering.
                    log::warn!(
                        "failed to parse legacy crate dependency version '{}' for crate '{}'",
                        version_num,
                        crate_base
                    );
                    crate_base
                }
            }
        } else {
            crate_base
        }
    }

    fn cleaned_version_requirement(&self) -> Option<String> {
        self.version.as_ref().map(|version| {
            // Clean version string for output: remove wildcards, build metadata, and other invalid RPM chars
            // "0.4.*" -> "0.4.0", ">= 0.4.*" -> ">= 0.4.0"
            // "0.7.5+spec-1.1.0" -> "0.7.5"
            version
                .replace(".*", ".0")
                .replace('*', "0")
                .split('+')
                .next()
                .unwrap_or(version)
                .to_string()
        })
    }
}

impl Description {
    pub fn new(prefix: String, suffix: String) -> Self {
        Self { prefix, suffix }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // Package name uses hyphens instead of underscores
        let pkg_name = self.crate_name.replace('_', "-");

        let (pkgname, rpm_name) = if let Ok(ver) = Version::parse(&self.version) {
            let output_names = takopack_core::util::rust_crate_output_names(&self.crate_name, &ver);
            let pkgname = output_names
                .directory
                .strip_prefix("rust-")
                .unwrap_or(&output_names.directory)
                .to_string();
            (pkgname, output_names.directory)
        } else {
            let compat_version = self.version.clone();
            (
                format!("{}-{}", pkg_name, compat_version),
                format!("rust-{}-{}", pkg_name, compat_version),
            )
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

        let source = SpecSource {
            crate_name: self.crate_name.clone(),
            full_version: self.full_version.clone(),
            pkgname,
            rpm_name,
            rpm_version,
            summary: format!("Rust crate \"{}\"", self.crate_name),
            license: if !self.license.is_empty() {
                self.license.clone()
            } else {
                "FIXME".to_string()
            },
            url: if !self.homepage.is_empty() {
                self.homepage.clone()
            } else {
                "FIXME".to_string()
            },
            // Use full version (including build metadata) in Source URL.
            source_url: "https://static.crates.io/crates/%{crate_name}/%{full_version}/download#/%{name}-%{version}.tar.gz".to_string(),
            sha256: self.sha256.clone(),
            build_requires: vec!["rust-rpm-macros".to_string()],
            with_spdx: self.with_spdx,
        };

        spec::render_header_section(f, &source)?;
        spec::render_source_requirements_section(f, &source)?;
        Ok(())
    }
}

fn crate_requirements_from_cargo_deps(
    deps: &[Dependency],
    current_crate_name: &str,
) -> Vec<CrateRequirement> {
    use cargo::core::dependency::DepKind;

    let mut requirements = std::collections::BTreeMap::new();
    let current_crate_base = spec::normalize_crate_name(current_crate_name);

    for dep in deps {
        if !dependency_is_runtime_candidate(dep, false) {
            continue;
        }

        let dep_crate_base = spec::normalize_crate_name(dep.package_name().as_str());
        if dep_crate_base == current_crate_base {
            continue;
        }

        // Optional dependencies are already selected by the feature graph before
        // they reach this helper, so the optional flag is intentionally not a filter.
        let _is_optional = dep.is_optional();
        let lower_bound = lower_bound_from_opt_version_req(dep.version_req());
        let crate_name = cargo_dep_crate_name(dep.package_name().as_str(), lower_bound.as_deref());
        let requirement = lower_bound
            .map(|version| RequirementVersion::Range(format!(">= {}", version)))
            // A wildcard dependency such as "*" has no meaningful lower bound.
            // Keep the crate requirement unversioned rather than inventing one.
            .unwrap_or(RequirementVersion::None);

        let mut features = std::collections::BTreeSet::new();
        if dep.kind() == DepKind::Build && !dep.is_optional() {
            features.insert(None);
            for feature in dep.features() {
                features.insert(Some(feature.as_str().to_string()));
            }
        } else {
            if dep.uses_default_features() {
                features.insert(Some("default".to_string()));
            }
            for feature in dep.features() {
                features.insert(Some(feature.as_str().to_string()));
            }
            if features.is_empty() {
                features.insert(None);
            }
        }

        for feature in features {
            let requirement = CrateRequirement {
                crate_name: crate_name.clone(),
                feature,
                requirement: requirement.clone(),
            };
            requirements.insert(crate_requirement_key(&requirement), requirement);
        }
    }

    requirements.into_values().collect()
}

fn cargo_dep_crate_name(crate_name: &str, lower_bound: Option<&str>) -> String {
    let crate_base = spec::normalize_crate_name(crate_name);

    if let Some(version) = lower_bound {
        if version.contains('-') {
            format!("{}-{}", crate_base, version)
        } else if let Ok(version) = Version::parse(version) {
            format!(
                "{}-{}",
                crate_base,
                takopack_core::util::calculate_compat_version(&version)
            )
        } else {
            crate_base
        }
    } else {
        crate_base
    }
}

fn lower_bound_from_opt_version_req(version_req: &OptVersionReq) -> Option<String> {
    match version_req {
        OptVersionReq::Any => None,
        OptVersionReq::Req(req) if req.to_string() == "*" => None,
        OptVersionReq::Req(req) => req
            .comparators
            .iter()
            .filter_map(lower_bound_from_comparator)
            .max_by(compare_version_strings),
        OptVersionReq::Locked(version, _) | OptVersionReq::Precise(version, _) => {
            Some(version_without_build_metadata(version))
        }
    }
}

fn lower_bound_from_comparator(comparator: &semver::Comparator) -> Option<String> {
    use semver::Op;

    match comparator.op {
        Op::Exact | Op::GreaterEq | Op::Tilde | Op::Caret => {
            Some(comparator_lower_bound(comparator))
        }
        Op::Greater => Some(comparator_strict_lower_bound(comparator)),
        Op::Wildcard if comparator.minor.is_some() || comparator.patch.is_some() => {
            Some(comparator_lower_bound(comparator))
        }
        Op::Wildcard | Op::Less | Op::LessEq => None,
        _ => None,
    }
}

fn comparator_lower_bound(comparator: &semver::Comparator) -> String {
    let mut version = format!(
        "{}.{}.{}",
        comparator.major,
        comparator.minor.unwrap_or(0),
        comparator.patch.unwrap_or(0)
    );
    if !comparator.pre.is_empty() {
        version.push('-');
        version.push_str(comparator.pre.as_str());
    }
    version
}

fn comparator_strict_lower_bound(comparator: &semver::Comparator) -> String {
    if !comparator.pre.is_empty() {
        // TODO: model strict prerelease bounds more precisely when RPM crate
        // requirements grow beyond simple lower bounds.
        return comparator_lower_bound(comparator);
    }

    match (comparator.minor, comparator.patch) {
        (Some(minor), Some(patch)) => format!("{}.{}.{}", comparator.major, minor, patch + 1),
        (Some(minor), None) => format!("{}.{}.0", comparator.major, minor + 1),
        (None, None) => format!("{}.0.0", comparator.major + 1),
        (None, Some(patch)) => format!("{}.0.{}", comparator.major, patch + 1),
    }
}

fn version_without_build_metadata(version: &Version) -> String {
    if !version.pre.is_empty() {
        format!(
            "{}.{}.{}-{}",
            version.major, version.minor, version.patch, version.pre
        )
    } else {
        format!("{}.{}.{}", version.major, version.minor, version.patch)
    }
}

fn compare_version_strings(a: &String, b: &String) -> std::cmp::Ordering {
    match (Version::parse(a), Version::parse(b)) {
        (Ok(a), Ok(b)) => a.cmp(&b),
        _ => a.cmp(b),
    }
}

impl fmt::Display for Package {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let spec_package = SpecPackage {
            feature: self.feature.clone(),
            summary: format!("{}", self.summary),
            description: format!("{}", self.description),
            requires: self.spec_requires(),
            provides: self.spec_provides(),
            extra_lines: self.extra_lines.clone(),
        };

        if self.feature.is_some() {
            spec::render_feature_package_section(f, &spec_package)
        } else {
            spec::render_main_package_section(f, &spec_package)
        }
    }
}

fn crate_requirement_key(requirement: &CrateRequirement) -> String {
    let rendered = spec::render_crate_requirement(requirement);
    rendered
        .split(' ')
        .next()
        .unwrap_or(rendered.as_str())
        .to_string()
}

fn insert_crate_requirement(
    dep_map: &mut std::collections::BTreeMap<String, CrateRequirement>,
    requirement: CrateRequirement,
) {
    let key = crate_requirement_key(&requirement);
    match dep_map.get(&key) {
        Some(existing)
            if requirement_has_version(&requirement) && !requirement_has_version(existing) =>
        {
            dep_map.insert(key, requirement);
        }
        Some(existing) => {
            let existing_len = spec::render_crate_requirement(existing).len();
            let new_len = spec::render_crate_requirement(&requirement).len();
            if new_len > existing_len {
                dep_map.insert(key, requirement);
            }
        }
        None => {
            dep_map.insert(key, requirement);
        }
    }
}

fn requirement_has_version(requirement: &CrateRequirement) -> bool {
    !matches!(requirement.requirement, RequirementVersion::None)
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
        license: &str,
        full_version: String,   // Full version including build metadata
        sha256: Option<String>, // SHA256 hash of downloaded crate file
    ) -> Result<Source> {
        let pkgbase = match name_suffix {
            None => basename.to_string(),
            Some(suf) => format!("{}{}", basename, suf),
        };
        Ok(Source {
            name: rpm_source_name(&pkgbase),
            version: version.to_string(),
            full_version,
            homepage: home.to_string(),
            crate_name: crate_name.to_string(),
            license: license.to_string(),
            sha256,
            with_spdx: false,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn apply_overrides(&mut self, config: &Config, with_spdx: bool) {
        if let Some(homepage) = config.homepage() {
            self.homepage = homepage.to_string();
        }

        self.with_spdx = with_spdx;
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

    fn spec_requires(&self) -> Vec<CrateRequirement> {
        // Deduplicate by the crate(...) key, preferring versioned requirements.
        let mut dep_map: std::collections::BTreeMap<String, CrateRequirement> =
            std::collections::BTreeMap::new();

        for requirement in &self.crate_requires {
            insert_crate_requirement(&mut dep_map, requirement.clone());
        }

        for dep in &self.crate_deps {
            let requirement = dep.to_crate_requirement();
            insert_crate_requirement(&mut dep_map, requirement);
        }

        dep_map.into_values().collect()
    }

    fn spec_provides(&self) -> Vec<CrateCapability> {
        if self.crate_name.is_none() {
            return vec![];
        }

        let mut capabilities = vec![];
        let mut features = std::collections::BTreeSet::new();

        if let Some(feature) = &self.feature {
            if !feature.is_empty() {
                features.insert(spec::normalize_feature_name(feature));
            }
            for feature in &self.feature_provides {
                if !feature.is_empty() {
                    features.insert(spec::normalize_feature_name(feature));
                }
            }
        } else {
            capabilities.push(CrateCapability::package_feature(None));
            for feature in self.all_features.iter().chain(self.feature_provides.iter()) {
                if !feature.is_empty() {
                    features.insert(spec::normalize_feature_name(feature));
                }
            }
        }

        capabilities.extend(
            features
                .into_iter()
                .map(|feature| CrateCapability::package_feature(Some(feature))),
        );
        capabilities
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
        summary: Description,
        description: Description,
        feature: Option<&str>,
        f_deps: Vec<&str>,
        ori_deps: Vec<Dependency>,
        f_provides: Vec<&str>,
        all_features: Vec<String>, // All features from Cargo.toml (only used for base package)
    ) -> Result<Package> {
        let pkgbase = match name_suffix {
            None => basename.to_string(),
            Some(suf) => format!("{}{}", basename, suf),
        };

        let mut crate_deps = vec![];

        if feature.is_some() && !f_deps.contains(&"") {
            // Feature subpackages depend directly on the base crate package,
            // even when the feature only reaches it through another feature.
            crate_deps.push(CrateDep::new("%{pkgname}".to_string(), None));
        }

        // Build crate_deps from f_deps (internal feature dependencies, no version)
        for f in &f_deps {
            if f.is_empty() {
                // Empty feature means dependency on base crate
                crate_deps.push(CrateDep::new("%{pkgname}".to_string(), None));
            } else {
                // Feature dependency
                crate_deps.push(CrateDep::new("%{pkgname}".to_string(), Some(f.to_string())));
            }
        }

        let crate_requires = crate_requirements_from_cargo_deps(&ori_deps, basename);

        Ok(Package {
            name: match feature {
                None => rpm_package_name(&pkgbase),
                Some(f) => rpm_feature_package_name(&pkgbase, f),
            },
            crate_deps,
            crate_requires,
            feature_provides: f_provides
                .iter()
                .map(|feature| feature.to_string())
                .collect(),
            summary,
            description,
            extra_lines: vec![],
            feature: feature.map(|s| s.to_string()),
            crate_name: Some(basename.to_string()),
            all_features,
        })
    }

    pub fn new_extra(name: String) -> Self {
        Package {
            name,
            crate_deps: Default::default(),
            crate_requires: Default::default(),
            feature_provides: Default::default(),
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

    pub fn apply_overrides(&mut self, config: &Config, key: PackageKey) {
        self.summary
            .apply_overrides(&config.summary, config.package_summary(key));
        self.description
            .apply_overrides(&config.description, config.package_description(key));

        self.extra_lines.extend(
            config
                .package_extra_lines(key)
                .into_iter()
                .flatten()
                .map(|s| s.to_string()),
        );
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

/// Translates a semver into a takopack-format upstream version.
/// Omits the build metadata, and uses a ~ before the prerelease version so it
/// compares earlier than the subsequent release.
pub fn rpm_upstream_version(v: &Version) -> String {
    let mut s = format!("{}.{}.{}", v.major, v.minor, v.patch);
    if !v.pre.is_empty() {
        // Use '-' instead of '~' for prerelease versions in RPM spec
        write!(s, "-{}", v.pre.as_str()).unwrap();
    }
    s
}

pub fn base_crate_package_name(crate_name: &str) -> String {
    crate_name.replace('_', "-").to_lowercase()
}

pub fn rpm_source_name(name: &str) -> String {
    format!("{}-{}", Source::pkg_prefix(), base_crate_package_name(name))
}

pub fn rpm_package_name(name: &str) -> String {
    format!(
        "{}-{}",
        Package::pkg_prefix(),
        base_crate_package_name(name)
    )
}

pub fn rpm_feature_package_name(name: &str, feature: &str) -> String {
    format!(
        "{}-{}-{}",
        Package::pkg_prefix(),
        base_crate_package_name(name),
        base_crate_package_name(feature)
    )
}

#[cfg(test)]
mod tests {
    use super::{CrateDep, Source, crate_requirements_from_cargo_deps};
    use crate::crates::{all_dependencies_and_features, transitive_deps};
    use crate::rpm::spec;

    use cargo::GlobalContext;
    use cargo::core::{Dependency, EitherManifest, SourceId, dependency::DepKind};
    use cargo::util::toml::read_manifest;
    use std::fs;

    fn test_dep(
        name: &str,
        version: &str,
        uses_default_features: bool,
        features: &[&str],
    ) -> Dependency {
        let source_id = SourceId::for_path(&std::env::current_dir().unwrap()).unwrap();
        let mut dep = Dependency::parse(name, Some(version), source_id).unwrap();
        dep.set_default_features(uses_default_features);
        dep.set_features(features.iter().copied());
        dep
    }

    fn manifest_from_toml(toml: &str) -> cargo::core::Manifest {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("src")).unwrap();
        fs::write(temp.path().join("src/lib.rs"), "pub fn marker() {}\n").unwrap();
        let cargo_toml = temp.path().join("Cargo.toml");
        fs::write(&cargo_toml, toml).unwrap();
        let source_id = SourceId::for_path(temp.path()).unwrap();
        match read_manifest(&cargo_toml, source_id, &GlobalContext::default().unwrap()).unwrap() {
            EitherManifest::Real(manifest) => manifest,
            _ => panic!("expected real manifest"),
        }
    }

    fn rendered_cargo_requirements(deps: &[Dependency]) -> Vec<String> {
        rendered_cargo_requirements_for_crate(deps, "current_crate")
    }

    fn rendered_cargo_requirements_for_crate(
        deps: &[Dependency],
        current_crate_name: &str,
    ) -> Vec<String> {
        crate_requirements_from_cargo_deps(deps, current_crate_name)
            .into_iter()
            .map(|requirement| spec::render_crate_requires(&requirement))
            .collect()
    }

    fn rendered_feature_requirements(toml: &str, feature: &str) -> Vec<String> {
        let manifest = manifest_from_toml(toml);
        let features_with_deps = all_dependencies_and_features(&manifest).unwrap();
        let (_, deps) = transitive_deps(&features_with_deps, feature).unwrap();
        rendered_cargo_requirements_for_crate(&deps, manifest.name().as_str())
    }

    #[test]
    fn source_header_uses_major_branch_for_package_names() {
        let source = Source::new(
            "clap",
            "4.6.1",
            None,
            "clap",
            "https://example.invalid/clap",
            "MIT OR Apache-2.0",
            "4.6.1".to_string(),
            None,
        )
        .unwrap();
        let rendered = source.to_string();

        assert!(rendered.contains("%global pkgname clap-4"));
        assert!(rendered.contains("Name:           rust-clap-4"));
    }

    #[test]
    fn cargo_dependency_default_features_require_default_capability() {
        let dep = test_dep("base64", "0.22.1", true, &[]);
        let rendered = rendered_cargo_requirements(&[dep]);

        assert_eq!(
            vec!["Requires:       crate(base64-0.22/default) >= 0.22.1"],
            rendered
        );
    }

    #[test]
    fn cargo_dependency_default_features_false_requires_base_capability() {
        let dep = test_dep("base64", "0.22.1", false, &[]);
        let rendered = rendered_cargo_requirements(&[dep]);

        assert!(
            rendered
                .iter()
                .all(|line| !line.contains("crate(base64-0.22/default)"))
        );
        assert_eq!(
            vec!["Requires:       crate(base64-0.22) >= 0.22.1"],
            rendered
        );
    }

    #[test]
    fn cargo_dependency_explicit_features_include_default_when_enabled() {
        let dep = test_dep("serde", "1", true, &["derive", "std"]);
        let rendered = rendered_cargo_requirements(&[dep]);

        assert_eq!(
            vec![
                "Requires:       crate(serde-1/default) >= 1.0.0",
                "Requires:       crate(serde-1/derive) >= 1.0.0",
                "Requires:       crate(serde-1/std) >= 1.0.0",
            ],
            rendered
        );
    }

    #[test]
    fn cargo_dependency_explicit_features_skip_default_when_disabled() {
        let dep = test_dep("serde", "1", false, &["derive"]);
        let rendered = rendered_cargo_requirements(&[dep]);

        assert!(
            rendered
                .iter()
                .all(|line| !line.contains("crate(serde-1/default)"))
        );
        assert_eq!(
            vec!["Requires:       crate(serde-1/derive) >= 1.0.0"],
            rendered
        );
    }

    #[test]
    fn cargo_dependency_major_four_uses_major_branch() {
        let dep = test_dep("clap-builder", "4.6.0", true, &[]);
        let rendered = rendered_cargo_requirements(&[dep]);

        assert_eq!(
            vec!["Requires:       crate(clap-builder-4/default) >= 4.6.0"],
            rendered
        );
    }

    #[test]
    fn non_optional_cargo_build_dependency_requires_base_crate_capability() {
        let mut dep = test_dep("cc", "1", true, &[]);
        dep.set_kind(DepKind::Build);

        assert_eq!(
            vec!["Requires:       crate(cc-1) >= 1.0.0"],
            rendered_cargo_requirements(&[dep])
        );
    }

    #[test]
    fn cargo_dev_dependency_does_not_enter_runtime_crate_requires() {
        let mut dep = test_dep("proptest", "1", true, &[]);
        dep.set_kind(DepKind::Development);

        assert!(rendered_cargo_requirements(&[dep]).is_empty());
    }

    #[test]
    fn onig_sys_style_build_dependencies_are_rendered_without_optional_leakage() {
        let toml = r#"
[package]
name = "onig_sys"
version = "69.9.3"
edition = "2021"

[build-dependencies]
bindgen = { version = "0.72", optional = true, features = ["runtime"] }
pkg-config = "^0.3.16"
cc = "1.0"

[dev-dependencies]
proptest = "1"

[features]
default = ["generate"]
generate = ["bindgen"]
"#;

        let base = rendered_feature_requirements(toml, "");
        assert!(base.contains(&"Requires:       crate(cc-1) >= 1.0.0".to_string()));
        assert!(base.contains(&"Requires:       crate(pkg-config-0.3) >= 0.3.16".to_string()));
        assert!(base.iter().all(|line| !line.contains("bindgen")));
        assert!(base.iter().all(|line| !line.contains("proptest")));

        let generate = rendered_feature_requirements(toml, "generate");
        assert!(generate.contains(&"Requires:       crate(cc-1) >= 1.0.0".to_string()));
        assert!(generate.contains(&"Requires:       crate(pkg-config-0.3) >= 0.3.16".to_string()));
        assert!(
            generate
                .iter()
                .any(|line| line == "Requires:       crate(bindgen-0.72/runtime) >= 0.72.0")
        );
        assert!(generate.iter().all(|line| !line.contains("proptest")));
    }

    #[test]
    fn target_specific_normal_dependency_enters_provider_crate_requires() {
        let mut dep = test_dep("windows-win", "3", true, &[]);
        dep.set_platform(Some("cfg(windows)".parse().unwrap()));

        assert_eq!(
            vec!["Requires:       crate(windows-win-3/default) >= 3.0.0"],
            rendered_cargo_requirements(&[dep])
        );
    }

    #[test]
    fn target_specific_dev_dependency_does_not_enter_provider_crate_requires() {
        let mut dep = test_dep("windows-dev", "1", true, &[]);
        dep.set_kind(DepKind::Development);
        dep.set_platform(Some("cfg(windows)".parse().unwrap()));

        assert!(rendered_cargo_requirements(&[dep]).is_empty());
    }

    #[test]
    fn target_specific_optional_dependency_stays_feature_scoped() {
        let toml = r#"
[package]
name = "target_optional"
version = "1.0.0"
edition = "2021"

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.61", optional = true, default-features = false, features = ["Win32_Foundation"] }

[features]
default = []
windows = ["dep:windows-sys"]
"#;

        let base = rendered_feature_requirements(toml, "");
        assert!(base.iter().all(|line| !line.contains("windows-sys")));

        let windows = rendered_feature_requirements(toml, "windows");
        assert_eq!(
            vec!["Requires:       crate(windows-sys-0.61/win32-foundation) >= 0.61.0"],
            windows
        );
    }

    #[test]
    fn rustc_workspace_dependency_does_not_enter_runtime_crate_requires() {
        let dep = test_dep("rustc-std-workspace-core", "1", true, &[]);

        assert!(rendered_cargo_requirements(&[dep]).is_empty());
    }

    #[test]
    fn cargo_wildcard_dependency_does_not_generate_x_capability() {
        let dep = test_dep("base64", "0.22.*", false, &[]);
        let rendered = rendered_cargo_requirements(&[dep]);

        assert!(rendered.iter().all(|line| !line.contains(".x")));
        assert_eq!(
            vec!["Requires:       crate(base64-0.22) >= 0.22.0"],
            rendered
        );
    }

    #[test]
    fn cargo_greater_than_dependency_uses_next_patch_lower_bound() {
        let dep = test_dep("serde", "> 1.2.3", false, &[]);
        let rendered = rendered_cargo_requirements(&[dep]);

        assert_eq!(vec!["Requires:       crate(serde-1) >= 1.2.4"], rendered);
    }

    #[test]
    fn same_crate_feature_dependencies_remain_exact_version() {
        assert_eq!(
            "crate(%{pkgname}) = %{version}",
            CrateDep::new("%{pkgname}".to_string(), None).to_crate_format()
        );
        assert_eq!(
            "crate(%{pkgname}/std) = %{version}",
            CrateDep::new("%{pkgname}".to_string(), Some("std".to_string())).to_crate_format()
        );
    }
}
