use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::Context;
use cargo::core::{Dependency, PackageId};
use clap::{Parser, ValueEnum};

use crate::config::Config;
use crate::crates::{
    all_dependencies_and_features, crate_name_ver_to_dep, show_dep, transitive_deps, CrateDepInfo,
    CrateInfo,
};
use crate::errors::Result;
use crate::package::{PackageExtractArgs, PackageProcess};
use crate::takopack::control::base_deb_name;
use crate::util;

#[derive(Debug, Clone, Copy, ValueEnum)]
#[value(rename_all = "verbatim")]
pub enum ResolveType {
    SourceFortakopackUnstable,
    BinaryAllFortakopackTesting,
}

#[derive(Debug, Clone, Parser)]
pub struct BuildOrderArgs {
    /// Name of the crate to package.
    crate_name: String,
    /// Version of the crate to package; may contain dependency operators.
    version: Option<String>,
    /// Directory for configs. The config subdirectory for a given crate is
    /// looked up by their crate name and version, from more specific to less
    /// specific, e.g. <crate>-1.2.3, then <crate>-1.2, then <crate>-1 and
    /// finally <crate>. The config file is read from the takopack/takopack.toml
    /// subpath of the looked-up subdirectory.
    #[arg(long)]
    config_dir: Option<PathBuf>,
    /// Resolution type
    #[arg(value_enum, long, default_value = "SourceFortakopackUnstable")]
    resolve_type: ResolveType,
    /// Emulate resolution as if every package were built with --collapse-features.
    #[arg(long)]
    emulate_collapse_features: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PackageIdFeat(PackageId, &'static str);

impl fmt::Display for PackageIdFeat {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}@{}/{}", self.0.name(), self.0.version(), self.1)
    }
}

// First result: if somebody build-depends on us, what do they first need to build?
// Second result: what other packages need to go into takopack Testing before us?
fn get_build_deps(
    crate_details: &(CrateInfo, CrateDepInfo, Config),
    package: &PackageIdFeat,
    resolve_type: ResolveType,
    emulate_collapse_features: bool,
) -> Result<(Vec<Dependency>, Vec<Dependency>)> {
    let (_, crate_dep_info, config) = crate_details;
    let all_deps = crate_dep_info
        .iter()
        .flat_map(|(_, v)| v.1.iter())
        .cloned()
        .collect::<HashSet<_>>();
    let feature_deps: HashSet<Dependency> =
        HashSet::from_iter(transitive_deps(crate_dep_info, package.1)?.1);
    let additional_deps = if emulate_collapse_features || config.collapse_features {
        all_deps.clone()
    } else {
        // TODO: if build_depends_features is an override, use that instead of "default"
        // TODO: also deprecate build_depends_excludes
        HashSet::from_iter(transitive_deps(crate_dep_info, "default")?.1)
    };
    let hard_deps = feature_deps
        .union(&additional_deps)
        .cloned()
        .collect::<Vec<_>>();
    use ResolveType::*;
    match resolve_type {
        SourceFortakopackUnstable => Ok((hard_deps, vec![])),
        BinaryAllFortakopackTesting => {
            let mut soft_deps = all_deps;
            for h in hard_deps.iter() {
                soft_deps.remove(h);
            }
            Ok((hard_deps, soft_deps.into_iter().collect::<Vec<_>>()))
        }
    }
}

fn dep_features(dep: &Dependency) -> Vec<&'static str> {
    let mut feats = dep
        .features()
        .iter()
        .map(|x| x.as_str())
        .collect::<Vec<_>>();
    if dep.uses_default_features() {
        feats.push("default")
    }
    feats.push(""); // bare-bones library with no features
    feats
}

fn find_config(config_dir: &Path, id: PackageId) -> Result<(Option<PathBuf>, Config)> {
    let name = base_deb_name(&id.name());
    let version = id.version();
    let candidates = [
        format!(
            "{}-{}.{}.{}",
            name, version.major, version.minor, version.patch
        ),
        format!("{}-{}.{}", name, version.major, version.minor),
        format!("{}-{}", name, version.major),
        name,
    ];
    for c in candidates {
        let path = config_dir.join(c).join("takopack").join("takopack.toml");
        if path.is_file() {
            let config = Config::parse(&path).context("failed to parse takopack.toml")?;
            log::debug!("using config for {}: {:?}", id, path);
            return Ok((Some(path), config));
        }
    }
    Ok((None, Config::default()))
}

fn resolve_info(
    infos: &mut BTreeMap<PackageId, (CrateInfo, CrateDepInfo, Config)>,
    cache: &mut HashMap<Dependency, PackageId>,
    config_dir: Option<&Path>,
    dependency: &Dependency,
    update: bool,
) -> Result<PackageId> {
    if let Some(id) = cache.get(dependency) {
        return Ok(*id);
    }

    // resolve dependency
    let info = CrateInfo::new_from_dependency(dependency, update)?;
    let id = info.package_id();
    cache.insert(dependency.clone(), id);

    // insert info if it's not already there
    if let std::collections::btree_map::Entry::Vacant(e) = infos.entry(id) {
        let id = *e.key();
        let default_config = Config::default();
        let (config_path, config) = match config_dir {
            None => (None, default_config),
            Some(config_dir) => {
                let (config_path, config) = find_config(config_dir, id)?;
                match config_path {
                    None => (None, default_config),
                    Some(_) => (config_path, config),
                }
            }
        };
        let (info, config) = match config_path {
            None => (info, config),
            Some(_) => {
                let mut process = PackageProcess::new(info, config_path, config)?;
                let tempdir = tempfile::Builder::new()
                    .prefix("takopack")
                    .tempdir_in(".")?;
                process.extract(PackageExtractArgs {
                    directory: Some(tempdir.path().to_path_buf()),
                })?;
                process.apply_overrides()?;
                (process.crate_info, process.config)
            }
        };
        let dep_info = all_dependencies_and_features(info.manifest());
        e.insert((info, dep_info, config));
    };
    Ok(id)
}

pub fn build_order(args: BuildOrderArgs) -> Result<Vec<PackageId>> {
    let crate_name = &args.crate_name;
    let version = args.version.as_deref();
    let config_dir = args.config_dir.as_deref();

    let mut infos = BTreeMap::new();
    let mut cache = HashMap::new();
    let seed_dep = crate_name_ver_to_dep(crate_name, version)?;
    let seed_id = resolve_info(&mut infos, &mut cache, config_dir, &seed_dep, true)?;

    let mut next = |idf: &PackageIdFeat| -> Result<(Vec<PackageIdFeat>, Vec<PackageIdFeat>)> {
        log::trace!("{} getting build deps..", idf);
        let (hard, soft) = get_build_deps(
            infos
                .get(&idf.0)
                .expect("build_order next called without crate info"),
            idf,
            args.resolve_type,
            args.emulate_collapse_features,
        )?;
        log::trace!("{} hard-dep: {}", idf, util::show_vec_with(&hard, show_dep));
        if !soft.is_empty() {
            log::trace!("{} soft-dep: {}", idf, util::show_vec_with(&soft, show_dep));
        }
        // note: we might resolve the same crate-version several times;
        // this is expected, since different dependencies (with different
        // version ranges) might resolve into the same crate-version
        let mut hard_p = Vec::new();
        for dep in hard {
            let id = resolve_info(&mut infos, &mut cache, config_dir, &dep, false)?;
            for f in dep_features(&dep) {
                hard_p.push(PackageIdFeat(id, f));
            }
        }
        let mut soft_p = Vec::new();
        for dep in soft {
            let id = resolve_info(&mut infos, &mut cache, config_dir, &dep, false)?;
            for f in dep_features(&dep) {
                soft_p.push(PackageIdFeat(id, f));
            }
        }
        log::trace!("{} hard-dep resolve: {}", idf, util::show_vec(&hard_p));
        if !soft_p.is_empty() {
            log::trace!("{} soft-dep resolve: {}", idf, util::show_vec(&soft_p));
        }
        Ok((hard_p, soft_p))
    };
    let mut i = 0;
    let mut log = |remaining: &VecDeque<_>, graph: &BTreeMap<_, _>| {
        i += 1;
        if i % 16 == 0 {
            takopack_info!(
                "takopack build-order: resolving dependencies: done: {}, todo: {}",
                graph.len(),
                remaining.len()
            );
        }
        Ok(())
    };

    let succ_with_features =
        util::graph_from_succ([PackageIdFeat(seed_id, "")], &mut next, &mut log)?;
    log::trace!("succ_with_features: {:#?}", succ_with_features);

    let succ = util::succ_proj(&succ_with_features, |x| x.0);
    let pred = util::succ_to_pred(&succ);
    let roots = succ
        .iter()
        .filter_map(|(k, v)| if v.is_empty() { Some(*k) } else { None })
        .collect::<BTreeSet<_>>();
    // swap pred/succ for call to topo_sort since we want reverse topo order
    let build_order = match util::topo_sort(roots, pred.clone(), succ.clone()) {
        Ok(r) => r,
        Err(remain) => {
            log::error!(
                "topo_sort got cyclic graph: {:#?}",
                remain
                    .into_iter()
                    .map(|(k, vv)| (
                        k.to_string(),
                        vv.into_iter()
                            .map(|v| v.to_string())
                            .collect::<BTreeSet<_>>()
                    ))
                    .collect::<BTreeMap<_, _>>()
            );
            takopack_bail!(
                "topo_sort got cyclic graph; you'll need to patch the crate(s) to break the cycle."
            )
        }
    };

    // sanity check
    for p in build_order.iter() {
        if infos.remove(p).is_none() {
            log::error!("extra package in build-order not in infos: {}", p);
        }
    }
    for (p, _) in infos {
        log::error!(
            "leftover infos not used in build-order: {}, succ: {}, pred: {}",
            p,
            util::show_vec(succ.get(&p).into_iter().flatten()),
            util::show_vec(pred.get(&p).into_iter().flatten()),
        );
    }

    Ok(build_order)
}
