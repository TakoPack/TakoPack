//! Dynamic BuildRequires generation from Cargo's resolved lockfile.

use std::collections::BTreeSet;
use std::path::Path;

use semver::Version;

use crate::errors::Result;
use crate::resolve_check::{self, LockPackage};
use crate::util::calculate_compat_version;

pub fn run_buildreqs(path: &Path, registry: Option<&Path>) -> Result<i32> {
    let report = resolve_check::resolve_single_crate(path, registry)?;
    for line in buildrequires_from_lock_packages(&report.lock_packages) {
        println!("{line}");
    }
    Ok(0)
}

pub fn buildrequires_from_lock_packages(packages: &[LockPackage]) -> Vec<String> {
    let mut lines = BTreeSet::new();

    for package in packages {
        if !package
            .source
            .as_deref()
            .is_some_and(|source| source.starts_with("registry+"))
        {
            continue;
        }

        let capability_name = package.name.replace('_', "-");
        let compat = calculate_compat_version(&package.version);
        let version = clean_semver_without_build(&package.version);
        lines.insert(format!(
            "BuildRequires:  crate({capability_name}-{compat}) >= {version}"
        ));
    }

    lines.into_iter().collect()
}

fn clean_semver_without_build(version: &Version) -> String {
    let mut clean = format!("{}.{}.{}", version.major, version.minor, version.patch);
    if !version.pre.is_empty() {
        clean.push('-');
        clean.push_str(version.pre.as_str());
    }
    clean
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buildreqs_include_only_registry_packages() {
        let packages = vec![
            LockPackage {
                name: "app".to_string(),
                version: Version::parse("0.1.0").unwrap(),
                source: None,
            },
            LockPackage {
                name: "foo_bar".to_string(),
                version: Version::parse("1.2.3+takopack.1").unwrap(),
                source: Some("registry+https://github.com/rust-lang/crates.io-index".to_string()),
            },
            LockPackage {
                name: "local".to_string(),
                version: Version::parse("0.1.0").unwrap(),
                source: Some("path+file:///tmp/local".to_string()),
            },
        ];

        assert_eq!(
            buildrequires_from_lock_packages(&packages),
            vec!["BuildRequires:  crate(foo-bar-1) >= 1.2.3"]
        );
    }
}
