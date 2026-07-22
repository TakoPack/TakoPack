use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::io::{self, ErrorKind, Seek, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::process::Command;

use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use takopack_core::{
    config::{Config, PackageKey},
    errors::*,
    util::{self, copy_tree, expect_success, traverse_depth},
};
use takopack_core::{takopack_bail, takopack_info, takopack_warn};
use tar::{Archive, Builder};
use tempfile;

use self::metadata::{Description, Package, Source};
use self::metadata::{base_crate_package_name, rpm_upstream_version};
use self::spec::{
    SpecFiles, render_build_check_install_placeholder, render_changelog_section,
    render_files_section, render_patch_prep_placeholder,
};
use crate::crates::{CrateDepInfo, CrateInfo, all_dependencies_and_features, show_dep};

pub mod metadata;
pub mod spec;

pub struct RpmPackageInfo {
    upstream_name: String,
    /// takopack package name without `rust-` prefix or any semver suffix
    base_package_name: String,
    /// Package name suffix after the base package name.
    /// Some implies semver_suffix, i.e. Some("") is different from None
    name_suffix: Option<String>,
    /// takopack package name without `rust-` prefix
    package_name: String,
    rpm_upstream_version: String,
    takopack_version: String,
    package_source_dir: PathBuf,
    source_archive_path: PathBuf,
}

impl RpmPackageInfo {
    pub fn new(crate_info: &CrateInfo, takopack_version: &str, semver_suffix: bool) -> Self {
        let upstream_name = crate_info.package_id().name().to_string();
        let name_dashed = base_crate_package_name(&upstream_name);
        let base_package_name = name_dashed.to_lowercase();
        let rpm_upstream_version = rpm_upstream_version(crate_info.version());

        let (name_suffix, package_name) = if semver_suffix {
            // semver now includes full version for prerelease (e.g., 0.26.0-beta.1)
            // and compat version for normal releases (e.g., 0.26 or 1)
            let semver = crate_info.semver();
            let name_suffix = format!("-{}", &semver);
            let pkgname = format!("{}{}", base_package_name, &name_suffix);
            (Some(name_suffix), pkgname)
        } else {
            (None, base_package_name.clone())
        };
        let package_source_dir = PathBuf::from(format!(
            "{}-{}-{}",
            Source::pkg_prefix(),
            package_name,
            rpm_upstream_version
        ));
        let source_archive_path = PathBuf::from(format!(
            "{}-{}-{}.tar.gz",
            Source::pkg_prefix(),
            package_name,
            rpm_upstream_version
        ));

        RpmPackageInfo {
            upstream_name,
            base_package_name,
            name_suffix,
            package_name,
            rpm_upstream_version,
            takopack_version: takopack_version.to_string(),
            package_source_dir,
            source_archive_path,
        }
    }

    pub fn upstream_name(&self) -> &str {
        self.upstream_name.as_str()
    }

    pub fn base_package_name(&self) -> &str {
        self.base_package_name.as_str()
    }

    pub fn name_suffix(&self) -> Option<&str> {
        self.name_suffix.as_deref()
    }

    pub fn package_name(&self) -> &str {
        self.package_name.as_str()
    }

    pub fn rpm_upstream_version(&self) -> &str {
        self.rpm_upstream_version.as_str()
    }

    pub fn takopack_version(&self) -> &str {
        self.takopack_version.as_str()
    }

    pub fn package_source_dir(&self) -> &Path {
        self.package_source_dir.as_ref()
    }

    pub fn source_archive_path(&self) -> &Path {
        self.source_archive_path.as_ref()
    }
}

impl std::fmt::Debug for RpmPackageInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        writeln!(f, "Package Name:   {}", self.package_name)?;
        writeln!(f, "Version:        {}", self.rpm_upstream_version)?;
        writeln!(f, "Source Dir:     {}", self.package_source_dir.display())?;
        writeln!(f, "Source Archive: {}", self.source_archive_path.display())?;
        if let Some(ref suffix) = self.name_suffix {
            writeln!(f, "Name Suffix:    {}", suffix)?;
        }
        Ok(())
    }
}

impl Clone for RpmPackageInfo {
    fn clone(&self) -> Self {
        RpmPackageInfo {
            upstream_name: self.upstream_name.clone(),
            base_package_name: self.base_package_name.clone(),
            name_suffix: self.name_suffix.clone(),
            package_name: self.package_name.clone(),
            rpm_upstream_version: self.rpm_upstream_version.clone(),
            takopack_version: self.takopack_version.clone(),
            package_source_dir: self.package_source_dir.clone(),
            source_archive_path: self.source_archive_path.clone(),
        }
    }
}

pub fn prepare_source_archive(
    crate_info: &CrateInfo,
    archive: &Path,
    src_modified: bool,
    output_dir: &Path,
) -> Result<()> {
    let crate_file = crate_info.crate_file();
    let archive_parent = archive
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let tempdir = tempfile::Builder::new()
        .prefix("takopack")
        .tempdir_in(archive_parent)?;
    let archive_name = archive
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("source archive path has no file name"))?;
    let temp_archive_path = tempdir.path().join(archive_name);

    // Remove existing archive file if it exists to avoid "File exists" error.
    if archive.exists() {
        fs::remove_file(archive)?;
    }

    let mut create = fs::OpenOptions::new();
    create.write(true).create_new(true);

    if src_modified {
        takopack_info!("crate tarball was modified; repacking for takopack");
        let mut f = crate_file.file();
        f.seek(io::SeekFrom::Start(0))?;
        let mut archive = Archive::new(GzDecoder::new(f));
        let mut new_archive = Builder::new(GzEncoder::new(
            create.open(&temp_archive_path)?,
            Compression::best(),
        ));

        for entry in archive.entries()? {
            let entry = entry?;
            let path = entry.path()?.into_owned();
            if path.ends_with("Cargo.toml") && path.iter().count() == 2 {
                // Put the rewritten and original Cargo.toml back into the source archive.
                let mut new_archive_append = |name: &str| {
                    let mut header = entry.header().clone();
                    let srcpath = output_dir.join(name);
                    header.set_path(path.parent().unwrap().join(name))?;
                    header.set_size(fs::metadata(&srcpath)?.len());
                    header.set_cksum();
                    new_archive.append(&header, fs::File::open(&srcpath)?)
                };
                new_archive_append("Cargo.toml")?;
                new_archive_append("Cargo.toml.orig")?;
            } else {
                match crate_info.filter_path(&entry.path()?) {
                    Err(e) => takopack_bail!(e),
                    Ok(r) => {
                        if !r {
                            new_archive.append_data(&mut entry.header().clone(), path, entry)?;
                        } else {
                            writeln!(
                                io::stderr(),
                                "Filtered out files from source archive: {:?}",
                                &entry.path()?
                            )?;
                        }
                    }
                }
            }
        }

        new_archive.finish()?;
    } else {
        fs::copy(crate_file.path(), &temp_archive_path)?;
    }

    fs::rename(temp_archive_path, archive)?;
    Ok(())
}

pub fn apply_overlay_and_patches(
    crate_info: &mut CrateInfo,
    config_path: Option<&Path>,
    config: &Config,
    output_dir: &Path,
) -> Result<tempfile::TempDir> {
    let tempdir = tempfile::Builder::new()
        .prefix("takopack")
        .tempdir_in(".")?;
    let overlay = config.overlay_dir(config_path);
    if let Some(p) = overlay.as_ref() {
        for anc in tempdir.path().ancestors() {
            if p.as_path() == anc {
                takopack_bail!(
                    "Aborting: refusing to copy an ancestor {} into a descendant {}",
                    p.as_path().display(),
                    tempdir.path().display(),
                );
            }
        }
        copy_tree(p.as_path(), tempdir.path())?;
    }
    if fs::read_dir(tempdir.path())?.any(|entry| {
        entry
            .ok()
            .and_then(|entry| {
                entry
                    .path()
                    .extension()
                    .map(|ext| ext == OsStr::new("spec"))
            })
            .unwrap_or(false)
    }) {
        takopack_warn!(
            "Most of the time you shouldn't overlay generated spec files; \
it's a maintenance burden. Use takopack.toml instead."
        );
    }
    // apply patches to Cargo.toml in case they exist, and re-read it
    if tempdir.path().join("patches").join("series").exists() {
        takopack_info!("applying patches..");
        let output_dir = &fs::canonicalize(output_dir)?;
        let stderr = || {
            // create a new owned handle to stderr
            fs::OpenOptions::new()
                .append(true)
                .open("/dev/stderr")
                .unwrap()
        };
        // common case, patches might need rebasing!
        if let Err(err) = expect_success(
            Command::new("quilt")
                .stdout(stderr())
                .current_dir(output_dir)
                .env("QUILT_PATCHES", tempdir.path().join("patches"))
                .args(["push", "--quiltrc=-", "-a"]),
            "failed to apply patches using quilt",
        ) {
            takopack_warn!(format!("{err}, attempting cleanup"));
            let _ = expect_success(
                Command::new("quilt")
                    .stdout(stderr())
                    .current_dir(output_dir)
                    .env("QUILT_PATCHES", tempdir.path().join("patches"))
                    .args(["pop", "--quiltrc=-", "-a", "-f"]),
                "failed to unapply partially applied patches",
            );
            fs::remove_dir_all(output_dir.join(".pc"))?;
            takopack_bail!("applying patches failed! see above for details..");
        }
        takopack_info!("reloading Cargo.toml..");
        crate_info.replace_manifest(&output_dir.join("Cargo.toml"))?;

        // this should never fail!
        takopack_info!("unapplying patches again..");
        expect_success(
            Command::new("quilt")
                .stdout(stderr())
                .current_dir(output_dir)
                .env("QUILT_PATCHES", tempdir.path().join("patches"))
                .args(["pop", "--quiltrc=-", "-a"]),
            "failed to unapply patches",
        )?;
        fs::remove_dir_all(output_dir.join(".pc"))?;
    }
    Ok(tempdir)
}

#[allow(clippy::too_many_arguments)]
pub fn prepare_takopack_folder(
    crate_info: &mut CrateInfo,
    rpm_info: &RpmPackageInfo,
    config_path: Option<&Path>,
    config: &Config,
    output_dir: &Path,
    tempdir: &tempfile::TempDir,
    _changelog_ready: bool,
    _copyright_guess_harder: bool,
    overlay_write_back: bool,
    sha256: Option<String>, // SHA256 hash of downloaded crate
    lockfile_deps: Option<std::collections::HashMap<String, semver::Version>>, // Optional: dependencies from Cargo.lock
    with_spdx: bool,
) -> Result<()> {
    let mut create = fs::OpenOptions::new();
    create.write(true).create_new(true);

    let mut new_hints = vec![];
    let mut file = |name: &str| {
        let path = tempdir.path();
        let f = path.join(name);
        fs::create_dir_all(f.parent().unwrap())?;
        create.open(&f).or_else(|e| match e.kind() {
            ErrorKind::AlreadyExists => {
                let hintname = name.to_owned() + util::HINT_SUFFIX;
                let hint = path.join(&hintname);
                if hint.exists() {
                    fs::remove_file(&hint)?;
                }
                new_hints.push(hintname);
                create.open(&hint)
            }
            _ => Err(e),
        })
    };

    // takopack/cargo-checksum.json
    {
        let checksum = crate_info
            .checksum()
            .unwrap_or("Could not get crate checksum");
        let mut cargo_checksum_json = file("cargo-checksum.json")?;
        writeln!(
            cargo_checksum_json,
            r#"{{"package":"{}","files":{{}}}}"#,
            checksum
        )?;
    }

    // takopack/<spec>
    let _source = prepare_takopack_spec(
        rpm_info,
        crate_info,
        config,
        sha256,
        lockfile_deps.as_ref(),
        &mut file,
        with_spdx,
    )?;

    if overlay_write_back {
        let overlay = config.overlay_dir(config_path);
        if let Some(p) = overlay.as_ref() {
            for hint in &new_hints {
                let newpath = tempdir.path().join(hint);
                let oldpath = p.join(hint);
                fs::copy(newpath, oldpath).expect("could not write back");
                takopack_info!("Wrote back file to overlay: {}", hint);
            }
        }
    }

    fs::rename(tempdir.path(), output_dir.join("takopack"))?;
    Ok(())
}

fn prepare_takopack_spec<F: FnMut(&str) -> std::result::Result<fs::File, io::Error>>(
    rpm_info: &RpmPackageInfo,
    crate_info: &CrateInfo,
    config: &Config,
    sha256: Option<String>, // SHA256 hash of downloaded crate
    lockfile_deps: Option<&HashMap<String, semver::Version>>, // Optional lockfile dependencies
    mut file: F,
    with_spdx: bool,
) -> Result<Source> {
    let crate_name = crate_info.crate_name();
    let base_pkgname = rpm_info.base_package_name();
    let name_suffix = rpm_info.name_suffix();

    let lib = crate_info.is_lib();
    let bins = crate_info.get_binary_targets();
    let prepared = prepare_spec_source(rpm_info, crate_info, config, sha256, with_spdx)?;

    let output_names = util::rust_crate_output_names(crate_name, crate_info.version());
    let mut spec_file = io::BufWriter::new(file(&output_names.spec_file)?);
    write!(spec_file, "{}", prepared.source)?;

    if lib {
        write_library_packages(
            &mut spec_file,
            config,
            &prepared.features_with_deps,
            base_pkgname,
            name_suffix,
            crate_name,
            &prepared.summary_prefix,
            &prepared.description_prefix,
            lockfile_deps,
        )?;
    } else if !bins.is_empty() {
        write_binary_only_package(
            &mut spec_file,
            config,
            &prepared.features_with_deps,
            base_pkgname,
            name_suffix,
            crate_name,
            &bins,
            &prepared.summary_prefix,
            &prepared.description_prefix,
            lockfile_deps,
        )?;
    }

    write_extra_packages(&mut spec_file, config)?;
    write_trailing_spec_sections(&mut spec_file)?;

    Ok(prepared.source)
}

struct PreparedSpec {
    source: Source,
    features_with_deps: CrateDepInfo,
    summary_prefix: String,
    description_prefix: String,
}

fn prepare_spec_source(
    rpm_info: &RpmPackageInfo,
    crate_info: &CrateInfo,
    config: &Config,
    sha256: Option<String>,
    with_spdx: bool,
) -> Result<PreparedSpec> {
    let crate_name = crate_info.crate_name();
    let features_with_deps = all_dependencies_and_features(crate_info.manifest())?;
    log_feature_deps("features_with_deps", &features_with_deps);

    let meta = crate_info.metadata();
    let homepage = meta
        .homepage
        .as_deref()
        .or(meta.repository.as_deref())
        .unwrap_or("");
    let license = meta.license.as_deref().unwrap_or("").replace('/', " OR ");
    let full_version = crate_info.version().to_string();
    let mut source = Source::new(
        rpm_info.base_package_name(),
        rpm_info.rpm_upstream_version(),
        rpm_info.name_suffix(),
        crate_name,
        homepage,
        &license,
        full_version,
        sha256,
    )?;
    source.apply_overrides(config, with_spdx);

    let (crate_summary, crate_description) = crate_info.get_summary_description();
    let summary_prefix = crate_summary.unwrap_or(format!("Rust crate \"{}\"", crate_name));
    let description_prefix = {
        let tmp = crate_description.unwrap_or_default();
        if tmp.is_empty() {
            tmp
        } else {
            format!("{}\n", tmp)
        }
    };

    Ok(PreparedSpec {
        source,
        features_with_deps,
        summary_prefix,
        description_prefix,
    })
}

#[allow(clippy::too_many_arguments)]
fn write_library_packages(
    spec_file: &mut io::BufWriter<fs::File>,
    config: &Config,
    features_with_deps: &CrateDepInfo,
    base_pkgname: &str,
    name_suffix: Option<&str>,
    crate_name: &str,
    summary_prefix: &str,
    description_prefix: &str,
    lockfile_deps: Option<&HashMap<String, semver::Version>>,
) -> Result<()> {
    let transformed = transform_feature_packages(features_with_deps.clone(), config)?;
    let mut provides = transformed.provides;
    let reduced_features_with_deps = transformed.reduced_features_with_deps;
    let original_features = transformed.original_features;
    let all_subpackage_features =
        collect_subpackage_features(&reduced_features_with_deps, &provides);

    for (feature, (f_deps, o_deps)) in reduced_features_with_deps.into_iter() {
        let pk = PackageKey::feature(feature);
        let f_provides = provides.remove(feature).unwrap();
        let summary_suffix = package_summary_suffix(feature, &f_provides);
        let description_suffix = package_description_suffix(crate_name, feature, &f_provides);
        let package_all_features = if feature.is_empty() {
            original_features
                .iter()
                .filter(|f| !all_subpackage_features.contains(*f))
                .cloned()
                .collect()
        } else {
            vec![]
        };

        let mut package = Package::new(
            base_pkgname,
            name_suffix,
            Description {
                prefix: summary_prefix.to_string(),
                suffix: summary_suffix,
            },
            Description {
                prefix: description_prefix.to_string(),
                suffix: description_suffix,
            },
            if feature.is_empty() {
                None
            } else {
                Some(feature)
            },
            f_deps,
            o_deps.clone(),
            f_provides.clone(),
            package_all_features,
        )?;

        if let Some(lockfile) = lockfile_deps {
            package.apply_lockfile_deps(lockfile);
        }
        package.apply_overrides(config, pk);
        write!(spec_file, "{}", package)?;
    }
    assert!(provides.is_empty());
    Ok(())
}

struct TransformedFeatures {
    provides: BTreeMap<&'static str, Vec<&'static str>>,
    reduced_features_with_deps: CrateDepInfo,
    original_features: Vec<String>,
}

fn transform_feature_packages(
    mut working_features_with_deps: CrateDepInfo,
    config: &Config,
) -> Result<TransformedFeatures> {
    let potential_corner_case = working_features_with_deps
        .keys()
        .filter(|x| base_crate_package_name(x).as_str() != **x)
        .cloned()
        .collect::<Vec<_>>();
    for f in potential_corner_case {
        let f_ = base_crate_package_name(f);
        if let Some((df1, dd1)) = working_features_with_deps.remove(f_.as_str()) {
            working_features_with_deps
                .entry(f)
                .and_modify(|(df0, dd0)| {
                    let mut df = BTreeSet::from_iter(df0.drain(..));
                    df.extend(df1);
                    df.remove(f_.as_str());
                    df.remove(f);
                    let mut dd: HashSet<cargo::core::Dependency> =
                        HashSet::from_iter(dd0.drain(..));
                    dd.extend(dd1);
                    df0.extend(df);
                    dd0.extend(dd);
                });
            for (_, (df, _)) in working_features_with_deps.iter_mut() {
                for feat in df.iter_mut() {
                    if *feat == f_.as_str() {
                        *feat = f;
                    }
                }
            }
            let dep_feats = traverse_depth(
                &|k: &&'static str| working_features_with_deps.get(k).map(|x| &x.0),
                f,
            );
            if dep_feats.contains(f) {
                log::debug!("transitive deps of feature {}: {:?}", f, dep_feats);
                takopack_bail!(
                    "Tried to merge features {} and {} as they are not representable separately\n\
                     in takopack, but this resulted in a feature cycle. You need to manually patch the package.",
                    f,
                    f_
                );
            } else {
                takopack_warn!(
                    "Merged features {} and {} as they are not representable separately in takopack.\n\
                     We checked that this does not break the package in an obvious way (feature cycle), however\n\
                     if there is a more sophisticated breakage, you'll have to manually patch those \
                     features instead.",
                    f,
                    f_
                );
            }
        }
    }
    log_feature_deps("working_features_with_deps", &working_features_with_deps);

    let original_features = working_features_with_deps
        .keys()
        .filter(|&k| !k.is_empty())
        .map(|k| k.to_string())
        .collect();
    let (provides, reduced_features_with_deps) = if config.collapse_features {
        collapse_features(working_features_with_deps)
    } else {
        reduce_provides(working_features_with_deps)
    };
    log_feature_deps("reduced_features_with_deps", &reduced_features_with_deps);
    log::trace!("provides: {:?}", provides);

    Ok(TransformedFeatures {
        provides,
        reduced_features_with_deps,
        original_features,
    })
}

fn log_feature_deps(label: &str, features_with_deps: &CrateDepInfo) {
    log::trace!(
        "{}: {:?}",
        label,
        features_with_deps
            .iter()
            .map(|(&f, (ff, dd))| { (f, (ff, dd.iter().map(show_dep).collect::<Vec<_>>())) })
            .collect::<Vec<_>>()
    );
}

fn collect_subpackage_features(
    reduced_features_with_deps: &CrateDepInfo,
    provides: &BTreeMap<&'static str, Vec<&'static str>>,
) -> HashSet<String> {
    let mut all_subpackage_features = HashSet::new();
    for (&feature, _) in reduced_features_with_deps.iter() {
        if !feature.is_empty() {
            all_subpackage_features.insert(feature.to_string());
            if let Some(merged_features) = provides.get(feature) {
                for &merged_feat in merged_features.iter() {
                    all_subpackage_features.insert(merged_feat.to_string());
                }
            }
        }
    }
    all_subpackage_features
}

fn package_summary_suffix(feature: &str, f_provides: &[&str]) -> String {
    if feature.is_empty() {
        " - Rust source code".to_string()
    } else {
        match f_provides.len() {
            0 => format!(" - feature \"{}\"", feature),
            _ => format!(" - feature \"{}\" and {} more", feature, f_provides.len()),
        }
    }
}

fn package_description_suffix(crate_name: &str, feature: &str, f_provides: &[&str]) -> String {
    if feature.is_empty() {
        format!("Source code for takopackized Rust crate \"{}\"", crate_name)
    } else {
        format!(
            "This metapackage enables feature \"{}\" for the \
             Rust {} crate, by pulling in any additional \
             dependencies needed by that feature.{}",
            feature,
            crate_name,
            match f_provides.len() {
                0 => "".to_string(),
                1 => format!(
                    "\n\nAdditionally, this package also provides the \
                     \"{}\" feature.",
                    f_provides[0],
                ),
                _ => format!(
                    "\n\nAdditionally, this package also provides the \
                     \"{}\", and \"{}\" features.",
                    f_provides[..f_provides.len() - 1].join("\", \""),
                    f_provides[f_provides.len() - 1],
                ),
            },
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn write_binary_only_package(
    spec_file: &mut io::BufWriter<fs::File>,
    config: &Config,
    features_with_deps: &CrateDepInfo,
    base_pkgname: &str,
    name_suffix: Option<&str>,
    crate_name: &str,
    bins: &[&str],
    summary_prefix: &str,
    description_prefix: &str,
    lockfile_deps: Option<&HashMap<String, semver::Version>>,
) -> Result<()> {
    let empty_deps = (vec![], vec![]);
    let (_, base_deps) = features_with_deps.get("").unwrap_or(&empty_deps);
    let description_suffix = binary_description_suffix(crate_name, bins);

    let mut package = Package::new(
        base_pkgname,
        name_suffix,
        Description {
            prefix: summary_prefix.to_string(),
            suffix: " - Rust source code".to_string(),
        },
        Description {
            prefix: description_prefix.to_string(),
            suffix: description_suffix,
        },
        None,
        vec![],
        base_deps.clone(),
        vec![],
        vec![],
    )?;

    if let Some(lockfile) = lockfile_deps {
        package.apply_lockfile_deps(lockfile);
    }
    package.apply_overrides(config, PackageKey::feature(""));
    write!(spec_file, "{}", package)?;
    Ok(())
}

fn binary_description_suffix(crate_name: &str, bins: &[&str]) -> String {
    format!(
        "This package contains the following binaries built from the Rust crate\n\"{}\":\n - {}",
        crate_name,
        bins.join("\n - ")
    )
}

fn write_extra_packages(spec_file: &mut io::BufWriter<fs::File>, config: &Config) -> Result<()> {
    for configured in config.configured_packages() {
        if let PackageKey::Extra(package) = configured {
            let mut extra_pkg = Package::new_extra(package.to_string());
            extra_pkg.apply_overrides(config, configured);
            write!(spec_file, "\n{}", extra_pkg)?;
        }
    }
    Ok(())
}

fn write_trailing_spec_sections(spec_file: &mut io::BufWriter<fs::File>) -> Result<()> {
    writeln!(spec_file)?;
    let mut trailing_sections = String::new();
    render_patch_prep_placeholder(&mut trailing_sections)?;
    render_build_check_install_placeholder(&mut trailing_sections)?;
    render_files_section(
        &mut trailing_sections,
        &[SpecFiles {
            package: None,
            entries: vec!["%{_datadir}/cargo/registry/%{crate_name}-%{version}/".to_string()],
        }],
    )?;
    render_changelog_section(&mut trailing_sections)?;
    write!(spec_file, "{}", trailing_sections)?;
    Ok(())
}

fn collapse_features(
    orig_features_with_deps: CrateDepInfo,
) -> (BTreeMap<&'static str, Vec<&'static str>>, CrateDepInfo) {
    let (provides, deps) = orig_features_with_deps.iter().fold(
        (Vec::new(), Vec::new()),
        |(mut provides, mut deps), (f, (_, f_deps))| {
            if f != &"" {
                provides.push(*f);
            }
            deps.append(&mut f_deps.clone());
            (provides, deps)
        },
    );

    let mut collapsed_provides = BTreeMap::new();
    collapsed_provides.insert("", provides);

    let mut collapsed_features_with_deps = BTreeMap::new();
    collapsed_features_with_deps.insert("", (Vec::new(), deps));

    (collapsed_provides, collapsed_features_with_deps)
}

/// Calculate Provides: in an attempt to reduce the number of binaries.
///
/// The algorithm is very simple and incomplete. e.g. it does not, yet
/// simplify things like:
///   f1 depends on f2, f3
///   f2 depends on f4
///   f3 depends on f4
/// into
///   f4 provides f1, f2, f3
fn reduce_provides(
    mut features_with_deps: CrateDepInfo,
) -> (BTreeMap<&'static str, Vec<&'static str>>, CrateDepInfo) {
    // If any features have duplicate dependencies, deduplicate them by
    // making all the subsequent ones depend on the first one.
    let mut features_rev_deps = HashMap::new();
    for (&f, dep) in features_with_deps.iter() {
        if !features_rev_deps.contains_key(dep) {
            features_rev_deps.insert(dep.clone(), vec![]);
        }
        features_rev_deps.get_mut(dep).unwrap().push(f);
    }
    for (_, ff) in features_rev_deps.into_iter() {
        let f0 = ff[0];
        for f in &ff[1..] {
            features_with_deps.insert(f, (vec![f0], vec![]));
        }
    }

    // Calculate provides by following 0- or 1-length dependency lists.
    let mut provides = BTreeMap::new();
    let mut provided = Vec::new();
    for (&f, (ff, dd)) in features_with_deps.iter() {
        //takopack_info!("provides considering: {:?}", &f);
        if !dd.is_empty() {
            continue;
        }
        assert!(!ff.is_empty() || f.is_empty());
        let k = if ff.len() == 1 {
            // if A depends on a single feature B, then B provides A.
            ff[0]
        } else {
            continue;
        };
        //takopack_info!("provides still considering: {:?}", &f);
        if !provides.contains_key(k) {
            provides.insert(k, vec![]);
        }
        provides.get_mut(k).unwrap().push(f);
        provided.push(f);
    }

    //takopack_info!("provides-internal: {:?}", &provides);
    //takopack_info!("provided-internal: {:?}", &provided);
    for p in provided {
        features_with_deps.remove(p);
    }

    let provides = features_with_deps
        .keys()
        .map(|k| {
            (
                *k,
                traverse_depth(&|k: &&'static str| provides.get(k), k)
                    .into_iter()
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();

    (provides, features_with_deps)
}
