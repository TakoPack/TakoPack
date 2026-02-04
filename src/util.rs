use core::panic;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::{BufRead, BufReader, Error};
use std::iter::Iterator;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::lockfile_parser::DependencyGraph;
use crate::package::{PackageExecuteArgs, PackageExtractArgs, PackageInitArgs, PackageProcess};
use anyhow::{bail, Context, Result};
use itertools::Itertools;
use semver::Version;
use walkdir::WalkDir;
pub const HINT_SUFFIX: &str = ".takopack.hint";

/// Calculate compatibility version following Rust semver rules
/// - Prerelease versions (e.g., 0.26.0-beta.1) -> full version (0.26.0-beta.1)
/// - BuildMetadata versions (e.g., 0.7.5+spec-1.1.0) -> full version (0.7.0)
/// - 0.x.y -> 0.x (0.x series, minor version compatibility)
/// - 1.x.y+ -> 1.0 (1.0+ series, major version compatibility)
/// - 0.0.x+ -> 0.0.x (0.0.x series, patch version compatibility)
pub fn calculate_compat_version(version: &Version) -> String {
    // For prerelease versions, use the full version including prerelease tag
    if !version.pre.is_empty() {
        format!(
            "{}.{}.{}-{}",
            version.major, version.minor, version.patch, version.pre
        )
    } else if false {
        // } else if !version.build.is_empty() {
        // TODO: In crates.io, build metadata is ignored for version precedence.
        // There can't be 0.9.11+spec-1.1.0 and 0.9.11+spec-1.2.0 at crates.io.
        // So we just use the full version. major.minor.patch without build metadata.
        // format!("{}.{}.{}", version.major, version.minor, version.patch)

        // format!("{}.{}.{}+{}", version.major, version.minor, version.patch, version.build)
        panic!("nerver to be here.")
    } else if version.major != 0 {
        format!("{}.0", version.major)
    } else if version.minor != 0 {
        format!("0.{}", version.minor)
    } else {
        format!("0.0.{}", version.patch)
    }
}

#[cfg(unix)]
pub fn hint_file_for(file: &Path) -> Option<Cow<'_, Path>> {
    let file = file.as_os_str().as_bytes();
    if file.len() >= HINT_SUFFIX.len()
        && &file[file.len() - HINT_SUFFIX.len()..] == HINT_SUFFIX.as_bytes()
    {
        Some(Cow::Borrowed(Path::new(OsStr::from_bytes(
            &file[..file.len() - HINT_SUFFIX.len()],
        ))))
    } else {
        None
    }
}

#[cfg(not(unix))]
pub fn hint_file_for(file: &Path) -> Option<Cow<'_, Path>> {
    if let Some(file_str) = file.to_str() {
        if file_str.ends_with(HINT_SUFFIX) {
            let trimmed_path = &file_str[..file_str.len() - HINT_SUFFIX.len()];
            Some(Cow::Owned(PathBuf::from(trimmed_path)))
        } else {
            None
        }
    } else {
        // Handle the case where the path is not representable as a string
        None
    }
}

pub fn lookup_fixmes(srcdir: &Path) -> Result<BTreeSet<PathBuf>, Error> {
    let mut fixmes = BTreeSet::new();
    let takopackdir = srcdir.join("takopack");
    for entry in WalkDir::new(takopackdir) {
        let entry = entry?;
        if entry.file_type().is_file() {
            let file = fs::File::open(entry.path())?;
            let reader = BufReader::new(file);
            // If we find one FIXME we break the loop and check next file. Idea
            // is only to find files with FIXME strings in it.
            for line in reader.lines() {
                match line {
                    Ok(line_content) => {
                        if line_content.contains("FIXME") {
                            fixmes.insert(entry.path().to_path_buf());
                            break;
                        }
                    }
                    Err(e) => {
                        takopack_warn!(
                            "Warning: Could not check for FIXMEs in file {:?}: {}",
                            rel_p(entry.path(), srcdir),
                            e
                        );
                        break;
                    }
                }
            }
        }
    }

    // ignore hint files whose non-hint partners exists and don't have a FIXME
    let fixmes = fixmes
        .iter()
        .filter(|f| match hint_file_for(f) {
            Some(ff) => fixmes.contains(ff.as_ref()) || !ff.exists(),
            None => true,
        })
        .cloned()
        .collect::<BTreeSet<_>>();
    Ok(fixmes)
}

pub fn rel_p<'a>(path: &'a Path, base: &'a Path) -> Cow<'a, str> {
    path.strip_prefix(base).unwrap_or(path).to_string_lossy()
}

pub fn copy_tree(oldtree: &Path, newtree: &Path) -> Result<(), Error> {
    for entry in WalkDir::new(oldtree) {
        let entry = entry?;
        if entry.depth() == 0 {
            continue;
        }
        let oldpath = entry.path();
        let newpath = newtree.join(oldpath.strip_prefix(oldtree).unwrap());
        let ftype = entry.file_type();
        match ftype {
            f if f.is_dir() => {
                fs::create_dir(newpath)?;
            }
            f if f.is_file() => {
                fs::copy(oldpath, newpath)?;
            }
            #[cfg(unix)]
            f if f.is_symlink() => {
                symlink(fs::read_link(oldpath)?, newpath)?;
            }
            _ => {}
        }
    }
    Ok(())
}

pub fn show_vec_with<'a, T, F>(it: impl IntoIterator<Item = &'a T>, f: F) -> String
where
    T: 'a,
    F: FnMut(&T) -> String,
{
    Itertools::intersperse(it.into_iter().map(f), ", ".to_string()).collect::<String>()
}

pub fn show_vec<'a, T>(it: impl IntoIterator<Item = &'a T>) -> String
where
    T: fmt::Display + 'a,
{
    show_vec_with(it, std::string::ToString::to_string)
}

pub fn expect_success(cmd: &mut Command, err: &str) -> Result<(), anyhow::Error> {
    match cmd.status() {
        Ok(status) if status.success() => Ok(()),
        Ok(_) => bail!("{}", err),
        Err(e) => bail!("{}\n{}", err, e),
    }
}

pub(crate) fn traverse_depth<'a, V, F>(succ: &'a F, key: V) -> BTreeSet<V>
where
    V: Ord + Copy + 'a,
    F: Fn(&V) -> Option<&'a Vec<V>>,
{
    let mut remain = VecDeque::from_iter([key]);
    let mut seen = BTreeSet::new();
    while let Some(v) = remain.pop_front() {
        for v_ in succ(&v).into_iter().flatten() {
            if !seen.contains(v_) {
                seen.insert(*v_);
                remain.push_back(*v_);
            }
        }
    }
    seen
}

/// Get a value that might be set at a key or any of its ancestor keys,
/// whichever is closest. Error if there are conflicting definitions.
#[allow(clippy::type_complexity)]
pub(crate) fn get_transitive_val<
    'a,
    P: Fn(K) -> Option<&'a Vec<K>>,
    F: Fn(K) -> Option<V>,
    K: 'a + Ord + Copy,
    V: Eq + Ord,
>(
    getparents: &'a P,
    f: &F,
    key: K,
) -> Result<Option<V>, (K, Vec<(K, V)>)> {
    let mut visited = std::collections::BTreeSet::new();
    get_transitive_val_impl(getparents, f, key, &mut visited)
}

fn get_transitive_val_impl<
    'a,
    P: Fn(K) -> Option<&'a Vec<K>>,
    F: Fn(K) -> Option<V>,
    K: 'a + Ord + Copy,
    V: Eq + Ord,
>(
    getparents: &'a P,
    f: &F,
    key: K,
    visited: &mut std::collections::BTreeSet<K>,
) -> Result<Option<V>, (K, Vec<(K, V)>)> {
    // Check for cycles
    if visited.contains(&key) {
        // Cycle detected, return None to break the recursion
        return Ok(None);
    }

    visited.insert(key);

    let here = f(key);
    if here.is_some() {
        // value overrides anything from parents
        Ok(here)
    } else {
        let mut candidates = Vec::new();
        for par in getparents(key).into_iter().flatten() {
            if let Some(v) = get_transitive_val_impl(getparents, f, *par, visited)? {
                candidates.push((*par, v))
            }
        }
        if candidates.is_empty() {
            Ok(None) // here is None
        } else {
            let mut values = candidates.iter().map(|(_, v)| v).collect::<Vec<_>>();
            values.sort();
            values.dedup();
            if values.len() == 1 {
                Ok(candidates.pop().map(|(_, v)| v))
            } else {
                Err((key, candidates)) // handle conflict
            }
        }
    }
}

pub fn graph_from_succ<V, FV, FL, E>(
    seed: impl IntoIterator<Item = V>,
    succ: &mut FV,
    log: &mut FL,
) -> Result<BTreeMap<V, BTreeSet<V>>, E>
where
    V: Ord + Clone,
    FV: FnMut(&V) -> Result<(Vec<V>, Vec<V>), E>,
    FL: FnMut(&VecDeque<V>, &BTreeMap<V, BTreeSet<V>>) -> Result<(), E>,
{
    let mut seen = BTreeSet::from_iter(seed);
    let mut graph = BTreeMap::new();
    let mut remain = VecDeque::from_iter(seen.iter().cloned());
    while let Some(v) = remain.pop_front() {
        log(&remain, &graph)?;
        let (hard, soft) = succ(&v)?;
        for v_ in hard.iter().chain(soft.iter()) {
            if !seen.contains(v_) {
                seen.insert(v_.clone());
                remain.push_back(v_.clone());
            }
        }
        graph.insert(v, BTreeSet::from_iter(hard));
    }
    Ok(graph)
}

pub fn succ_proj<S, T, F>(succ: &BTreeMap<S, BTreeSet<S>>, proj: F) -> BTreeMap<T, BTreeSet<T>>
where
    F: Fn(&S) -> T,
    S: Ord,
    T: Ord + Clone,
{
    let mut succ_proj: BTreeMap<T, BTreeSet<T>> = BTreeMap::new();
    for (s, ss) in succ {
        let e = succ_proj.entry(proj(s)).or_default();
        for s_ in ss {
            e.insert(proj(s_));
        }
    }
    succ_proj
}

pub fn succ_to_pred<V>(succ: &BTreeMap<V, BTreeSet<V>>) -> BTreeMap<V, BTreeSet<V>>
where
    V: Ord + Clone,
{
    let mut pred: BTreeMap<V, BTreeSet<V>> = BTreeMap::new();
    for (v, vv) in succ {
        for v_ in vv {
            pred.entry(v_.clone()).or_default().insert(v.clone());
        }
    }
    pred
}

pub fn topo_sort<V>(
    seed: impl IntoIterator<Item = V>,
    succ: BTreeMap<V, BTreeSet<V>>,
    mut pred: BTreeMap<V, BTreeSet<V>>,
) -> Result<Vec<V>, BTreeMap<V, BTreeSet<V>>>
where
    V: Ord + Clone,
{
    let empty = BTreeSet::new();
    let mut remain = VecDeque::from_iter(seed);
    let mut sort = Vec::new();
    while let Some(v) = remain.pop_front() {
        sort.push(v.clone());
        for v_ in succ.get(&v).unwrap_or(&empty) {
            let par = pred.entry(v_.clone()).or_default();
            par.remove(&v);
            if par.is_empty() {
                remain.push_back(v_.clone());
            }
        }
    }
    pred.retain(|_, v| !v.is_empty());
    if !pred.is_empty() {
        Err(pred)
    } else {
        Ok(sort)
    }
}

/// Backup Cargo.toml to ~/cargo_back directory
/// File will be named as: crate_name-version.toml
/// If subdir is provided, file will be saved in ~/cargo_back/{subdir}/
pub fn backup_cargo_toml(
    cargo_toml_path: &Path,
    crate_name: &str,
    version: &str,
    subdir: Option<&str>,
) -> Result<(), anyhow::Error> {
    use anyhow::Context;

    let home_dir = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("Failed to get home directory")?;

    let mut backup_dir = PathBuf::from(home_dir).join("cargo_back");

    // Add subdirectory if specified (e.g., "patch" for local packages)
    if let Some(sub) = subdir {
        backup_dir = backup_dir.join(sub);
    }

    fs::create_dir_all(&backup_dir)
        .with_context(|| format!("Failed to create backup directory: {:?}", backup_dir))?;

    let backup_filename = format!("{}-{}.toml", crate_name.replace('_', "-"), version);
    let backup_path = backup_dir.join(&backup_filename);

    if cargo_toml_path.exists() {
        fs::copy(cargo_toml_path, &backup_path)
            .with_context(|| format!("Failed to backup Cargo.toml to {:?}", backup_path))?;
        log::info!("Backed up Cargo.toml to: {:?}", backup_path);
    } else {
        log::warn!("Cargo.toml not found at: {:?}", cargo_toml_path);
    }

    Ok(())
}

/// Backup Cargo.lock to ~/cargo_back directory
/// File will be named as: crate_name-version.lock
/// If subdir is provided, file will be saved in ~/cargo_back/{subdir}/
pub fn backup_cargo_lock(
    cargo_lock_path: &Path,
    crate_name: &str,
    version: &str,
    subdir: Option<&str>,
) -> Result<PathBuf, anyhow::Error> {
    use anyhow::Context;

    let home_dir = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("Failed to get home directory")?;

    let mut backup_dir = PathBuf::from(home_dir).join("cargo_back");

    // Add subdirectory if specified (e.g., "origin" for downloaded crates)
    if let Some(sub) = subdir {
        backup_dir = backup_dir.join(sub);
    }

    fs::create_dir_all(&backup_dir)
        .with_context(|| format!("Failed to create backup directory: {:?}", backup_dir))?;

    let backup_filename = format!("{}-{}.lock", crate_name.replace('_', "-"), version);
    let backup_path = backup_dir.join(&backup_filename);

    if cargo_lock_path.exists() {
        fs::copy(cargo_lock_path, &backup_path)
            .with_context(|| format!("Failed to backup Cargo.lock to {:?}", backup_path))?;
        log::info!("Backed up Cargo.lock to: {:?}", backup_path);
    } else {
        log::warn!("Cargo.lock not found at: {:?}", cargo_lock_path);
        panic!("Cargo.lock backup failed!");
    }

    Ok(backup_path)
}

/// Process a single crate
/// If dep_graph is provided, use Cargo.lock dependencies for spec generation
pub fn process_single_crate(
    crate_name: &str,
    version: &str,
    base_dir: &PathBuf,
    dep_graph: Option<&DependencyGraph>,
) -> Result<()> {
    // Convert base_dir to absolute path before changing directory
    let base_dir_abs = fs::canonicalize(base_dir)
        .with_context(|| format!("Failed to get absolute path for: {:?}", base_dir))?;

    // Create a unique working directory for this crate to avoid conflicts
    let work_dir = base_dir_abs.join(format!(".work_{}", crate_name.replace('/', "_")));
    fs::create_dir_all(&work_dir)?;

    // Save current directory
    let original_dir = std::env::current_dir()?;

    // Change to working directory
    std::env::set_current_dir(&work_dir)
        .with_context(|| format!("Failed to change to work directory: {:?}", work_dir))?;
    let result = (|| -> Result<()> {
        // Initialize package process
        let init_args = PackageInitArgs {
            crate_name: crate_name.to_string(),
            version: Some(version.to_string()),
            config: None,
        };

        let extract_args = PackageExtractArgs {
            directory: None, // Let it extract to current (work) directory
        };

        // Extract lockfile dependencies if dep_graph is provided
        let lockfile_deps = dep_graph.and_then(|graph| {
            // Parse version for lookup
            if let Ok(ver) = semver::Version::parse(version) {
                graph.get_dependencies_map(crate_name, &ver)
            } else {
                None
            }
        });
        let finish_args = PackageExecuteArgs {
            changelog_ready: false,
            copyright_guess_harder: false,
            no_overlay_write_back: false,
            lockfile_deps, // Pass lockfile dependencies
        };

        let mut process = PackageProcess::init(init_args)?;

        // Extract crate (will create directory in work dir)
        process.extract(extract_args)?;

        // Apply overrides
        process.apply_overrides()?;

        // Prepare orig tarball
        process.prepare_orig_tarball()?;

        // Prepare takopack folder
        process.prepare_takopack_folder(finish_args)?;

        // Copy spec file to base_dir (use absolute path)
        let output_path = process.output_dir.as_ref().unwrap();
        let takopack_dir = output_path.join("takopack");
        let spec_filename = format!("rust-{}.spec", crate_name.replace('_', "-"));
        let source_spec = takopack_dir.join(&spec_filename);

        // Calculate compatibility version for target directory name
        let version_obj = process.crate_info().version();
        let compat_version = crate::util::calculate_compat_version(version_obj);
        let target_dirname = format!("rust-{}-{}", crate_name.replace('_', "-"), compat_version);

        // Create target directory in base_dir_abs (not work_dir)
        let target_dir = base_dir_abs.join(&target_dirname);
        fs::create_dir_all(&target_dir)?;
        let final_spec = target_dir.join(&spec_filename);

        // Copy spec file to target directory
        if source_spec.exists() {
            fs::copy(&source_spec, &final_spec)?;
            log::debug!("Copied spec file to: {:?}", final_spec);
        } else {
            return Err(anyhow::anyhow!(
                "Spec file not found at: {}",
                source_spec.display()
            ));
        }

        Ok(())
    })();

    // Always restore original directory
    std::env::set_current_dir(&original_dir)
        .with_context(|| format!("Failed to restore original directory: {:?}", original_dir))?;

    // Cleanup work directory
    if work_dir.exists() {
        fs::remove_dir_all(&work_dir)
            .with_context(|| format!("Failed to cleanup work directory: {:?}", work_dir))?;
    }

    result
}
