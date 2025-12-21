use std::collections::BTreeSet;
use std::path::PathBuf;

use cargo::core::EitherManifest;
use cargo::core::SourceId;
use cargo::util::toml::read_manifest;
use cargo::GlobalContext;

use anyhow::Error;
use clap::Parser;

use crate::crates::all_dependencies_and_features_filtered;
use crate::crates::transitive_deps;
use crate::takopack::deb_deps;
use crate::takopack::toolchain_deps;

#[derive(Debug, Clone, Parser)]
pub struct DebDependenciesArgs {
    /// Cargo.toml for generating dependencies
    cargo_toml: PathBuf,
    /// Features to include in dependencies
    #[clap(long)]
    features: Vec<String>,
    /// Include all features in dependencies
    #[clap(long)]
    all_features: bool,
    /// Do not include default feature in dependencies
    #[clap(long="no-default-features", action=clap::ArgAction::SetFalse)]
    uses_default_features: bool,
    /// Allow prerelease versions of dependencies
    #[clap(long)]
    allow_prerelease_deps: bool,
    /// Include dev-dependencies
    #[clap(long)]
    include_dev_dependencies: bool,
}

pub fn deb_dependencies(
    args: DebDependenciesArgs,
) -> Result<(Vec<String>, BTreeSet<String>), Error> {
    let cargo_toml = args.cargo_toml.canonicalize()?;
    let EitherManifest::Real(manifest) = read_manifest(
        &cargo_toml,
        SourceId::for_path(cargo_toml.parent().unwrap())?,
        &GlobalContext::default()?,
    )?
    else {
        takopack_bail!("Manifest lacks project and package sections")
    };

    let deps_and_features =
        all_dependencies_and_features_filtered(&manifest, args.include_dev_dependencies);

    let features = {
        let mut features: std::collections::HashSet<_> = if args.all_features {
            deps_and_features.keys().copied().collect()
        } else {
            args.features
                .iter()
                .flat_map(|s| s.split_whitespace())
                .flat_map(|s| s.split(','))
                .filter(|s| !s.is_empty())
                .collect()
        };

        if args.uses_default_features {
            features.insert("default");
        }

        features.insert("");

        features
    };
    let dependencies = {
        let mut dependencies = BTreeSet::<String>::new();
        for feature in features.iter() {
            if !deps_and_features.contains_key(feature) {
                takopack_bail!("Unknown feature: {}", feature);
            }
            let (_, feature_deps) = transitive_deps(&deps_and_features, feature)?;
            dependencies.extend(deb_deps(args.allow_prerelease_deps, &feature_deps)?);
        }
        dependencies
    };
    let toolchain_deps = toolchain_deps(&manifest.rust_version().map(|x| x.to_string()));
    Ok((toolchain_deps, dependencies))
}
