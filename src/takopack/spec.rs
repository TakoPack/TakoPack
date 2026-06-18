use std::fmt::{self, Write};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityVersion {
    None,
    Exact(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RequirementVersion {
    None,
    Exact(String),
    Range(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrateCapability {
    pub crate_name: String,
    pub feature: Option<String>,
    pub version: CapabilityVersion,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrateRequirement {
    pub crate_name: String,
    pub feature: Option<String>,
    pub requirement: RequirementVersion,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpecSource {
    pub crate_name: String,
    pub full_version: String,
    pub pkgname: String,
    pub rpm_name: String,
    pub rpm_version: String,
    pub summary: String,
    pub license: String,
    pub url: String,
    pub source_url: String,
    pub sha256: Option<String>,
    pub build_requires: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SpecPackage {
    pub feature: Option<String>,
    pub summary: String,
    pub description: String,
    pub requires: Vec<CrateRequirement>,
    pub provides: Vec<CrateCapability>,
    pub obsoletes: Vec<String>,
    pub conflicts: Vec<String>,
    pub extra_lines: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpecFiles {
    pub package: Option<String>,
    pub entries: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RpmSpec {
    pub source: SpecSource,
    pub main_package: SpecPackage,
    pub feature_packages: Vec<SpecPackage>,
    pub files: Vec<SpecFiles>,
    pub changelog: bool,
}

impl CrateCapability {
    pub fn package_feature(feature: Option<String>) -> Self {
        Self {
            crate_name: "%{pkgname}".to_string(),
            feature,
            version: CapabilityVersion::Exact("%{version}".to_string()),
        }
    }
}

impl CrateRequirement {
    pub fn same_crate(feature: Option<String>) -> Self {
        Self {
            crate_name: "%{pkgname}".to_string(),
            feature,
            requirement: RequirementVersion::Exact("%{version}".to_string()),
        }
    }
}

pub fn normalize_crate_name(crate_name: &str) -> String {
    if crate_name.starts_with("%{") {
        crate_name.to_string()
    } else {
        crate_name.replace('_', "-").to_lowercase()
    }
}

pub fn normalize_feature_name(feature: &str) -> String {
    feature
        .replace('_', "-")
        .to_lowercase()
        .trim_start_matches('-')
        .to_string()
}

pub fn render_crate_capability(cap: &CrateCapability) -> String {
    let capability = render_crate_name_feature(&cap.crate_name, cap.feature.as_deref());
    match &cap.version {
        CapabilityVersion::None => capability,
        CapabilityVersion::Exact(version) => format!("{} = {}", capability, version),
    }
}

pub fn render_crate_requirement(req: &CrateRequirement) -> String {
    let requirement = render_crate_name_feature(&req.crate_name, req.feature.as_deref());
    match &req.requirement {
        RequirementVersion::None => requirement,
        RequirementVersion::Exact(version) => format!("{} = {}", requirement, version),
        RequirementVersion::Range(version) => format!("{} {}", requirement, version),
    }
}

pub fn render_crate_provides(cap: &CrateCapability) -> String {
    format!("Provides:       {}", render_crate_capability(cap))
}

pub fn render_crate_requires(req: &CrateRequirement) -> String {
    format!("Requires:       {}", render_crate_requirement(req))
}

pub fn render_header_section<W: Write>(out: &mut W, source: &SpecSource) -> fmt::Result {
    writeln!(out, "%global crate_name {}", source.crate_name)?;
    writeln!(out, "%global full_version {}", source.full_version)?;
    writeln!(out, "%global pkgname {}", source.pkgname)?;
    writeln!(out)?;
    writeln!(out, "Name:           {}", source.rpm_name)?;
    writeln!(out, "Version:        {}", source.rpm_version)?;
    writeln!(out, "Release:        %autorelease")?;
    writeln!(out, "Summary:        {}", source.summary)?;
    writeln!(out, "License:        {}", source.license)?;
    writeln!(out, "URL:            {}", source.url)?;
    if let Some(ref hash) = source.sha256 {
        writeln!(out, "#!RemoteAsset:  sha256:{}", hash)?;
    } else {
        writeln!(out, "#!RemoteAsset:  sha256:")?;
    }
    writeln!(out, "Source:         {}", source.source_url)?;
    writeln!(out, "BuildArch:      noarch")?;
    writeln!(out, "BuildSystem:    rustcrates")?;
    writeln!(out)?;
    Ok(())
}

pub fn render_source_requirements_section<W: Write>(
    out: &mut W,
    source: &SpecSource,
) -> fmt::Result {
    for requirement in &source.build_requires {
        writeln!(out, "BuildRequires:  {}", requirement)?;
    }
    writeln!(out)?;
    Ok(())
}

pub fn render_main_package_section<W: Write>(out: &mut W, package: &SpecPackage) -> fmt::Result {
    render_package_metadata(out, package)?;
    render_description(out, None, &package.description)
}

pub fn render_feature_package_section<W: Write>(out: &mut W, package: &SpecPackage) -> fmt::Result {
    let feature = package
        .feature
        .as_deref()
        .map(normalize_feature_name)
        .unwrap_or_default();
    writeln!(out)?;
    writeln!(out, "%package     -n %{{name}}+{}", feature)?;
    writeln!(out, "Summary:        {}", package.summary)?;
    render_package_metadata(out, package)?;
    render_description(out, Some(&feature), &package.description)
}

pub fn render_patch_prep_placeholder<W: Write>(_out: &mut W) -> fmt::Result {
    Ok(())
}

pub fn render_build_check_install_placeholder<W: Write>(_out: &mut W) -> fmt::Result {
    Ok(())
}

pub fn render_files_section<W: Write>(out: &mut W, files: &[SpecFiles]) -> fmt::Result {
    for file_section in files {
        match &file_section.package {
            Some(package) => writeln!(out, "%files -n {}", package)?,
            None => writeln!(out, "%files")?,
        }
        for entry in &file_section.entries {
            writeln!(out, "{}", entry)?;
        }
        writeln!(out)?;
    }
    Ok(())
}

pub fn render_changelog_section<W: Write>(out: &mut W) -> fmt::Result {
    writeln!(out, "%changelog")?;
    writeln!(out, "%autochangelog")
}

impl RpmSpec {
    pub fn write_to<W: Write>(&self, out: &mut W) -> fmt::Result {
        render_header_section(out, &self.source)?;
        render_source_requirements_section(out, &self.source)?;
        render_main_package_section(out, &self.main_package)?;
        for feature_package in &self.feature_packages {
            render_feature_package_section(out, feature_package)?;
        }
        writeln!(out)?;
        render_patch_prep_placeholder(out)?;
        render_build_check_install_placeholder(out)?;
        render_files_section(out, &self.files)?;
        if self.changelog {
            render_changelog_section(out)?;
        }
        Ok(())
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        self.write_to(&mut out)
            .expect("rendering RPM spec to String should not fail");
        out
    }
}

fn render_crate_name_feature(crate_name: &str, feature: Option<&str>) -> String {
    let crate_name = normalize_crate_name(crate_name);
    if let Some(feature) = feature {
        let feature = normalize_feature_name(feature);
        format!("crate({}/{})", crate_name, feature)
    } else {
        format!("crate({})", crate_name)
    }
}

fn render_package_metadata<W: Write>(out: &mut W, package: &SpecPackage) -> fmt::Result {
    for requirement in &package.requires {
        writeln!(out, "{}", render_crate_requires(requirement))?;
    }
    for capability in &package.provides {
        writeln!(out, "{}", render_crate_provides(capability))?;
    }
    for obsolete in &package.obsoletes {
        writeln!(out, "Obsoletes:      {}", obsolete)?;
    }
    for conflict in &package.conflicts {
        writeln!(out, "Conflicts:      {}", conflict)?;
    }
    for line in &package.extra_lines {
        writeln!(out, "{}", line)?;
    }
    Ok(())
}

fn render_description<W: Write>(
    out: &mut W,
    feature: Option<&str>,
    description: &str,
) -> fmt::Result {
    writeln!(out)?;
    if let Some(feature) = feature {
        writeln!(out, "%description -n %{{name}}+{}", feature)?;
    } else {
        writeln!(out, "%description")?;
    }
    for line in description.lines() {
        writeln!(out, "{}", line.trim())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        CapabilityVersion, CrateCapability, CrateRequirement, RequirementVersion, RpmSpec,
        SpecFiles, SpecPackage, SpecSource,
    };

    #[test]
    fn renders_versioned_crate_capabilities_and_requirements() {
        let spec = RpmSpec {
            source: SpecSource {
                crate_name: "serde_with".to_string(),
                full_version: "3.18.0".to_string(),
                pkgname: "serde-with-3".to_string(),
                rpm_name: "rust-serde-with-3".to_string(),
                rpm_version: "3.18.0".to_string(),
                summary: "Rust crate \"serde_with\"".to_string(),
                license: "MIT OR Apache-2.0".to_string(),
                url: "https://example.invalid/serde_with".to_string(),
                source_url: "https://static.crates.io/crates/%{crate_name}/%{full_version}/download#/%{name}-%{version}.tar.gz".to_string(),
                sha256: None,
                build_requires: vec!["rust-rpm-macros".to_string()],
            },
            main_package: SpecPackage {
                description: "Main package".to_string(),
                requires: vec![CrateRequirement {
                    crate_name: "base64-0.22".to_string(),
                    feature: None,
                    requirement: RequirementVersion::Range(">= 0.22.1".to_string()),
                }],
                provides: vec![CrateCapability::package_feature(None)],
                ..SpecPackage::default()
            },
            feature_packages: vec![
                SpecPackage {
                    feature: Some("default".to_string()),
                    summary: "Default feature".to_string(),
                    description: "Default feature package".to_string(),
                    requires: vec![CrateRequirement::same_crate(None)],
                    provides: vec![CrateCapability::package_feature(Some("default".to_string()))],
                    ..SpecPackage::default()
                },
                SpecPackage {
                    feature: Some("rc".to_string()),
                    summary: "Rc feature".to_string(),
                    description: "Rc feature package".to_string(),
                    requires: vec![CrateRequirement::same_crate(None)],
                    provides: vec![CrateCapability {
                        crate_name: "%{pkgname}".to_string(),
                        feature: Some("rc".to_string()),
                        version: CapabilityVersion::Exact("%{version}".to_string()),
                    }],
                    ..SpecPackage::default()
                },
            ],
            files: vec![SpecFiles {
                package: None,
                entries: vec!["%{_datadir}/cargo/registry/%{crate_name}-%{version}/".to_string()],
            }],
            changelog: true,
        };

        let rendered = spec.render();
        assert!(rendered.contains("Provides:       crate(%{pkgname}) = %{version}"));
        assert!(rendered.contains("%package     -n %{name}+default"));
        assert!(rendered.contains("Provides:       crate(%{pkgname}/default) = %{version}"));
        assert!(rendered.contains("Requires:       crate(%{pkgname}) = %{version}"));
        assert!(rendered.contains("%package     -n %{name}+rc"));
        assert!(rendered.contains("Provides:       crate(%{pkgname}/rc) = %{version}"));
        assert!(rendered.contains("Requires:       crate(base64-0.22) >= 0.22.1"));
    }
}
