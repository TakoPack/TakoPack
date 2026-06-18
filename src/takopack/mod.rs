use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{self, ErrorKind, Seek, Write as IoWrite};
use std::ops::Deref;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use control::BuildDeps;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use itertools::Itertools;
use tar::{Archive, Builder};
use tempfile;

use crate::config::{package_field_for_feature, testing_ignore_debpolv, Config, PackageKey};
use crate::crates::{
    all_dependencies_and_features, show_dep, transitive_deps, CrateDepInfo, CrateInfo,
};
use crate::errors::*;
use crate::util::{self, copy_tree, expect_success, get_transitive_val, traverse_depth};

use self::control::{base_deb_name, deb_upstream_version};
use self::control::{Description, Package, PkgTest, Source};
pub use self::dependency::{deb_dep_add_nocheck, deb_deps};
use self::spec::{
    render_build_check_install_placeholder, render_changelog_section, render_files_section,
    render_patch_prep_placeholder, SpecFiles,
};

pub mod control;
mod dependency;
pub mod spec;

pub struct DebInfo {
    upstream_name: String,
    /// takopack package name without `rust-` prefix or any semver suffix
    base_package_name: String,
    /// Package name suffix after the base package name.
    /// Some implies semver_suffix, i.e. Some("") is different from None
    name_suffix: Option<String>,
    uscan_version_pattern: Option<String>,
    /// takopack package name without `rust-` prefix
    package_name: String,
    deb_upstream_version: String,
    takopack_version: String,
    package_source_dir: PathBuf,
    orig_tarball_path: PathBuf,
}

impl DebInfo {
    pub fn new(crate_info: &CrateInfo, takopack_version: &str, semver_suffix: bool) -> Self {
        let upstream_name = crate_info.package_id().name().to_string();
        let name_dashed = base_deb_name(&upstream_name);
        let base_package_name = name_dashed.to_lowercase();
        let deb_upstream_version = deb_upstream_version(crate_info.version());

        let (name_suffix, uscan_version_pattern, package_name) = if semver_suffix {
            // semver now includes full version for prerelease (e.g., 0.26.0-beta.1)
            // and compat version for normal releases (e.g., 0.26 or 1)
            let semver = crate_info.semver();
            let name_suffix = format!("-{}", &semver);
            // See `man uscan` description of @ANY_VERSION@ on how these
            // regex patterns were built.
            let uscan = format!("[-_]?({}\\.\\d[\\-+\\.:\\~\\da-zA-Z]*)", &semver);
            let pkgname = format!("{}{}", base_package_name, &name_suffix);
            (Some(name_suffix), Some(uscan), pkgname)
        } else {
            (None, None, base_package_name.clone())
        };
        let package_source_dir = PathBuf::from(format!(
            "{}-{}-{}",
            Source::pkg_prefix(),
            package_name,
            deb_upstream_version
        ));
        let orig_tarball_path = PathBuf::from(format!(
            "{}-{}_{}.orig.tar.gz",
            Source::pkg_prefix(),
            package_name,
            deb_upstream_version
        ));

        DebInfo {
            upstream_name,
            base_package_name,
            name_suffix,
            uscan_version_pattern,
            package_name,
            deb_upstream_version,
            takopack_version: takopack_version.to_string(),
            package_source_dir,
            orig_tarball_path,
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

    pub fn deb_upstream_version(&self) -> &str {
        self.deb_upstream_version.as_str()
    }

    pub fn takopack_version(&self) -> &str {
        self.takopack_version.as_str()
    }

    pub fn package_source_dir(&self) -> &Path {
        self.package_source_dir.as_ref()
    }

    pub fn orig_tarball_path(&self) -> &Path {
        self.orig_tarball_path.as_ref()
    }
}

impl std::fmt::Debug for DebInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        writeln!(f, "Package Name:   {}", self.package_name)?;
        writeln!(f, "Version:        {}", self.deb_upstream_version)?;
        writeln!(f, "Source Dir:     {}", self.package_source_dir.display())?;
        writeln!(f, "Tarball:        {}", self.orig_tarball_path.display())?;
        if let Some(ref suffix) = self.name_suffix {
            writeln!(f, "Name Suffix:    {}", suffix)?;
        }
        Ok(())
    }
}

impl Clone for DebInfo {
    fn clone(&self) -> Self {
        DebInfo {
            upstream_name: self.upstream_name.clone(),
            base_package_name: self.base_package_name.clone(),
            name_suffix: self.name_suffix.clone(),
            uscan_version_pattern: self.uscan_version_pattern.clone(),
            package_name: self.package_name.clone(),
            deb_upstream_version: self.deb_upstream_version.clone(),
            takopack_version: self.takopack_version.clone(),
            package_source_dir: self.package_source_dir.clone(),
            orig_tarball_path: self.orig_tarball_path.clone(),
        }
    }
}

pub fn prepare_orig_tarball(
    crate_info: &CrateInfo,
    tarball: &Path,
    src_modified: bool,
    output_dir: &Path,
) -> Result<()> {
    let crate_file = crate_info.crate_file();
    let tempdir = tempfile::Builder::new()
        .prefix("takopack")
        .tempdir_in(".")?;
    let temp_archive_path = tempdir.path().join(tarball);

    // Remove existing tarball file if it exists to avoid "File exists" error
    if tarball.exists() {
        fs::remove_file(tarball)?;
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
                // Put the rewritten and original Cargo.toml back into the orig tarball
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
                                "Filtered out files from .orig.tar.gz: {:?}",
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

    fs::rename(temp_archive_path, tarball)?;
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
    if tempdir.path().join("control").exists() {
        takopack_warn!(
            "Most of the time you shouldn't overlay takopack/control, \
it's a maintenance burden. Use takopack.toml instead."
        )
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
    deb_info: &DebInfo,
    config_path: Option<&Path>,
    config: &Config,
    output_dir: &Path,
    tempdir: &tempfile::TempDir,
    changelog_ready: bool,
    _copyright_guess_harder: bool,
    overlay_write_back: bool,
    sha256: Option<String>, // SHA256 hash of downloaded crate
    lockfile_deps: Option<std::collections::HashMap<String, semver::Version>>, // Optional: dependencies from Cargo.lock
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

    // takopack/control & takopack/tests/control
    let (_source, has_dev_depends, default_test_broken) = prepare_takopack_control(
        deb_info,
        crate_info,
        config,
        sha256,
        lockfile_deps.as_ref(),
        &mut file,
    )?;

    // for testing only, takopack/takopack_testing_bin/env
    if testing_ignore_debpolv() {
        fs::create_dir_all(tempdir.path().join("takopack_testing_bin"))?;
        let mut env_hack = file("takopack_testing_bin/env")?;
        #[cfg(unix)]
        env_hack.set_permissions(fs::Permissions::from_mode(0o777))?;
        // intercept calls to dh-cargo-built-using
        writeln!(
            env_hack,
            r#"#!/bin/sh
case "$*" in */usr/share/cargo/bin/dh-cargo-built-using*)
echo "takopack testing: suppressing dh-cargo-built-using";;
*) /usr/bin/env "$@";; esac
"#
        )?;
    }

    // takopack/rules
    {
        let mut rules = file("rules")?;
        #[cfg(unix)]
        rules.set_permissions(fs::Permissions::from_mode(0o777))?;
        if has_dev_depends || testing_ignore_debpolv() {
            // don't run any tests, we don't want extra B-D on dev-depends
            // this could potentially cause B-D cycles so we avoid it
            //
            // also don't run crate tests during integration testing since some
            // of them are brittle and fail; the purpose is to test takopack
            // not the actual crates
            write!(
                rules,
                "{}",
                concat!(
                    "#!/usr/bin/make -f\n",
                    "%:\n",
                    "\tdh $@ --buildsystem cargo\n"
                )
            )?;
            // some crates need nightly to compile, annoyingly. only do this in
            // testing; outside of testing the user should explicitly override
            // takopack/rules to do this
            if testing_ignore_debpolv() {
                writeln!(rules, "export RUSTC_BOOTSTRAP := 1")?;
                writeln!(
                    rules,
                    "export PATH := $(CURDIR)/takopack/takopack_testing_bin:$(PATH)"
                )?;
            }
        } else {
            write!(
                rules,
                "{}{}",
                concat!(
                    "#!/usr/bin/make -f\n",
                    "%:\n",
                    "\tdh $@ --buildsystem cargo\n",
                    "\n",
                    "override_dh_auto_test:\n",
                ),
                // TODO: this logic is slightly brittle if another feature
                // "provides" the default feature. In this case, you need to
                // set test_is_broken explicitly on package."lib+default" and
                // not package."lib+theotherfeature".
                if default_test_broken {
                    "\tdh_auto_test -- test --all || true\n"
                } else {
                    "\tdh_auto_test -- test --all\n"
                },
            )?;
        }
    }

    if overlay_write_back {
        let overlay = config.overlay_dir(config_path);
        if let Some(p) = overlay.as_ref() {
            if !changelog_ready {
                // Special-case d/changelog:
                // Always write it back, this is safe because of our prepending logic
                new_hints.push("changelog".to_string());
            }
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

fn prepare_takopack_control<F: FnMut(&str) -> std::result::Result<fs::File, io::Error>>(
    deb_info: &DebInfo,
    crate_info: &CrateInfo,
    config: &Config,
    sha256: Option<String>, // SHA256 hash of downloaded crate
    lockfile_deps: Option<&HashMap<String, semver::Version>>, // Optional lockfile dependencies
    mut file: F,
) -> Result<(Source, bool, bool)> {
    let crate_name = crate_info.crate_name();
    let deb_upstream_version = deb_info.deb_upstream_version();
    let base_pkgname = deb_info.base_package_name();
    let name_suffix = deb_info.name_suffix();

    let lib = crate_info.is_lib();
    let (bins, bin_name) = selected_binary_targets(crate_info, deb_info, config, lib);
    let prepared = prepare_control_source(deb_info, crate_info, config, sha256, lib, &bins)?;

    let output_names = util::rust_crate_output_names(crate_name, crate_info.version());
    let mut control = io::BufWriter::new(file(&output_names.spec_file)?);
    write!(control, "{}", prepared.source)?;

    if lib {
        write_library_packages(
            &mut control,
            &mut file,
            &prepared.source,
            crate_info,
            config,
            &prepared.features_with_deps,
            base_pkgname,
            name_suffix,
            crate_name,
            deb_upstream_version,
            &prepared.summary_prefix,
            &prepared.description_prefix,
            &prepared.test_deps,
            lockfile_deps,
        )?;
    } else if !bins.is_empty() {
        write_binary_only_package(
            &mut control,
            crate_info,
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

    if !bins.is_empty() {
        prepare_binary_package_for_overrides(
            config,
            lib,
            bin_name,
            name_suffix,
            crate_name,
            &bins,
            &prepared.summary_prefix,
            &prepared.description_prefix,
            lockfile_deps,
        );
    }

    write_extra_packages(&mut control, config)?;
    write_trailing_spec_sections(&mut control)?;

    let default_test_broken =
        feature_test_is_broken(config, &prepared.features_with_deps, "default")?;
    Ok((prepared.source, prepared.has_dev_deps, default_test_broken))
}

struct PreparedControl {
    source: Source,
    features_with_deps: CrateDepInfo,
    has_dev_deps: bool,
    test_deps: Vec<String>,
    summary_prefix: String,
    description_prefix: String,
}

fn selected_binary_targets<'a>(
    crate_info: &'a CrateInfo,
    deb_info: &'a DebInfo,
    config: &'a Config,
    lib: bool,
) -> (Vec<&'a str>, &'a str) {
    let mut bins = crate_info.get_binary_targets();
    if lib && !bins.is_empty() && !config.build_bin_package() {
        bins.clear();
    }

    let bin_name = if config.bin_name.eq(&Config::default().bin_name) {
        let default_bin_name = deb_info.base_package_name();
        if !bins.is_empty() {
            takopack_info!(
                "Generate binary crate with default name '{}', set bin_name to override or bin = false to disable.",
                &default_bin_name
            );
        }
        default_bin_name
    } else {
        config.bin_name.as_str()
    };

    (bins, bin_name)
}

fn prepare_control_source(
    deb_info: &DebInfo,
    crate_info: &CrateInfo,
    config: &Config,
    sha256: Option<String>,
    lib: bool,
    bins: &[&str],
) -> Result<PreparedControl> {
    let crate_name = crate_info.crate_name();
    let features_with_deps = all_dependencies_and_features(crate_info.manifest())?;
    log_feature_deps("features_with_deps", &features_with_deps);

    let dev_depends = deb_deps(config.allow_prerelease_deps, &crate_info.dev_dependencies())?;
    let has_dev_deps = !dev_depends.is_empty();
    let build_deps = build_deps_for_source(config, crate_info, &features_with_deps, lib, bins)?;
    let test_deps: Vec<String> = Some(rustc_dep(&crate_info.rust_version(), false))
        .into_iter()
        .chain(dev_depends)
        .collect();

    let meta = crate_info.metadata();
    let homepage = meta
        .homepage
        .as_deref()
        .or(meta.repository.as_deref())
        .unwrap_or("");
    let repository = meta.repository.as_deref().unwrap_or("");
    let license = meta.license.as_deref().unwrap_or("").replace('/', " OR ");
    let full_version = crate_info.version().to_string();
    let mut source = Source::new(
        deb_info.base_package_name(),
        deb_info.deb_upstream_version(),
        deb_info.name_suffix(),
        crate_name,
        homepage,
        repository,
        &license,
        lib,
        build_deps,
        full_version,
        sha256,
    )?;
    source.apply_overrides(config);

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

    Ok(PreparedControl {
        source,
        features_with_deps,
        has_dev_deps,
        test_deps,
        summary_prefix,
        description_prefix,
    })
}

fn build_deps_for_source(
    config: &Config,
    crate_info: &CrateInfo,
    features_with_deps: &CrateDepInfo,
    lib: bool,
    bins: &[&str],
) -> Result<BuildDeps> {
    let mut build_deps = BuildDeps::default();
    build_deps.build_depends.extend(
        ["debhelper-compat (= 13)", "dh-sequence-cargo"]
            .iter()
            .map(|x| x.to_string()),
    );

    // note: please keep this in sync with build_order::dep_features
    let (default_features, default_deps) = transitive_deps(features_with_deps, "default")?;
    let extra_override_deps = package_field_for_feature(
        |x| config.package_depends(x),
        PackageKey::feature("default"),
        &default_features,
    );
    let build_deps_arch = toolchain_deps(&crate_info.rust_version())
        .into_iter()
        .chain(deb_deps(config.allow_prerelease_deps, &default_deps)?)
        .chain(extra_override_deps);

    if !bins.is_empty() {
        build_deps.build_depends_arch.extend(build_deps_arch);
    } else {
        if !lib {
            log::warn!(
                "Crate has no library or binary targets; generating source-only build dependencies"
            );
        }
        build_deps
            .build_depends_arch
            .extend(build_deps_arch.map(|d| {
                if config.skip_nocheck().unwrap_or(false) {
                    d
                } else {
                    deb_dep_add_nocheck(&d)
                }
            }));
    }

    Ok(build_deps)
}

fn feature_test_is_broken(
    config: &Config,
    features_with_deps: &CrateDepInfo,
    feature: &str,
) -> Result<bool> {
    let test_is_marked_broken = |f: &str| config.package_test_is_broken(PackageKey::feature(f));
    let getparents = |f: &str| features_with_deps.get(f).map(|(d, _)| d);
    match get_transitive_val(&getparents, &test_is_marked_broken, feature) {
        Err((k, vv)) => takopack_bail!(
            "{} {}: {}: {:?}",
            "error trying to recursively determine test_is_broken for",
            k,
            "dependencies have inconsistent config values",
            vv
        ),
        Ok(v) => Ok(v.unwrap_or(false)),
    }
}

fn feature_test_architecture(
    config: &Config,
    features_with_deps: &CrateDepInfo,
    feature: &str,
) -> Result<Option<Vec<String>>> {
    let getparents = |f: &str| features_with_deps.get(f).map(|(d, _)| d);
    let feature_get_test_architecture =
        |f: &str| config.package_test_architecture(PackageKey::feature(f));
    match get_transitive_val(&getparents, &feature_get_test_architecture, feature) {
        Err((k, vv)) => takopack_bail!(
            "{} {}: {}: {:?}",
            "error trying to recursively determine test_architecture for",
            k,
            "dependencies have inconsistent config values",
            vv
        ),
        Ok(Some(v)) if v.is_empty() => Ok(None), // allow resetting via explicit empty list
        Ok(other) => Ok(other.cloned()),
    }
}

#[allow(clippy::too_many_arguments)]
fn write_library_packages<F>(
    control: &mut io::BufWriter<fs::File>,
    file: &mut F,
    source: &Source,
    crate_info: &CrateInfo,
    config: &Config,
    features_with_deps: &CrateDepInfo,
    base_pkgname: &str,
    name_suffix: Option<&str>,
    crate_name: &str,
    deb_upstream_version: &str,
    summary_prefix: &str,
    description_prefix: &str,
    test_deps: &[String],
    lockfile_deps: Option<&HashMap<String, semver::Version>>,
) -> Result<()>
where
    F: FnMut(&str) -> std::result::Result<fs::File, io::Error>,
{
    let all_features: Vec<&str> = features_with_deps.keys().copied().collect();
    let all_features_test_broken = match config.package_test_is_broken(PackageKey::feature("@")) {
        Some(v) => v,
        None => all_features.iter().any(|f| {
            config
                .package_test_is_broken(PackageKey::feature(f))
                .unwrap_or(false)
        }),
    };
    let all_features_test_arch = match feature_test_architecture(config, features_with_deps, "@")? {
        Some(v) => v,
        None => all_features
            .iter()
            .fold(HashSet::new(), |mut set, f| {
                if let Ok(Some(arch)) = feature_test_architecture(config, features_with_deps, f) {
                    set.extend(arch);
                }
                set
            })
            .into_iter()
            .collect_vec(),
    };
    let all_features_test_arch: Vec<&str> =
        all_features_test_arch.iter().map(AsRef::as_ref).collect();
    let all_features_test_depends =
        generate_test_dependencies("@", &all_features, config, test_deps);
    let mut testctl = io::BufWriter::new(file("tests/control")?);
    write!(
        testctl,
        "{}",
        PkgTest::new(
            source.name(),
            crate_name,
            "@",
            deb_upstream_version,
            vec!["--all-features"],
            &all_features_test_depends,
            if all_features_test_broken {
                vec!["flaky"]
            } else {
                vec![]
            },
            all_features_test_arch.deref(),
        )?
    )?;

    let transformed = transform_feature_packages(features_with_deps.clone(), config)?;
    let mut provides = transformed.provides;
    let reduced_features_with_deps = transformed.reduced_features_with_deps;
    let original_features = transformed.original_features;
    let (recommends, suggests) = feature_recommendations(&provides);
    let no_features_edge_case = is_no_features_edge_case(features_with_deps);
    let all_subpackage_features =
        collect_subpackage_features(&reduced_features_with_deps, &provides);

    for (feature, (f_deps, o_deps)) in reduced_features_with_deps.into_iter() {
        let pk = PackageKey::feature(feature);
        let f_provides = provides.remove(feature).unwrap();
        let mut crate_features = f_provides.clone();
        crate_features.push(feature);

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
            crate_info.version(),
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
            deb_deps(config.allow_prerelease_deps, &o_deps)?,
            o_deps.clone(),
            f_provides.clone(),
            if feature.is_empty() {
                recommends.clone()
            } else {
                vec![]
            },
            if feature.is_empty() {
                suggests.clone()
            } else {
                vec![]
            },
            package_all_features,
        )?;

        if let Some(lockfile) = lockfile_deps {
            package.apply_lockfile_deps(lockfile);
        }
        package.apply_overrides(config, pk, f_provides);
        write!(control, "{}", package)?;

        if !feature.is_empty() {
            let mut overrides =
                io::BufWriter::new(file(&format!("{}.lintian-overrides", package.name()))?);
            write!(
                overrides,
                "{} binary: empty-rust-library-declares-provides *",
                package.name()
            )?;
        }

        if !no_features_edge_case {
            write_feature_tests(
                &mut testctl,
                &package,
                crate_name,
                deb_upstream_version,
                &crate_features,
                features_with_deps,
                config,
                test_deps,
            )?;
        }
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
        .filter(|x| base_deb_name(x).as_str() != **x)
        .cloned()
        .collect::<Vec<_>>();
    for f in potential_corner_case {
        let f_ = base_deb_name(f);
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
                     in takopack, but this resulted in a feature cycle. You need to manually patch the package.", f, f_);
            } else {
                takopack_warn!(
                    "Merged features {} and {} as they are not representable separately in takopack.\n\
                     We checked that this does not break the package in an obvious way (feature cycle), however\n\
                     if there is a more sophisticated breakage, you'll have to manually patch those \
                     features instead.", f, f_);
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

fn feature_recommendations(
    provides: &BTreeMap<&'static str, Vec<&'static str>>,
) -> (Vec<&'static str>, Vec<&'static str>) {
    let mut recommends = vec![];
    let mut suggests = vec![];
    for (&feature, features) in provides.iter() {
        if feature.is_empty() {
            continue;
        } else if feature == "default" || features.contains(&"default") {
            recommends.push(feature);
        } else {
            suggests.push(feature);
        }
    }
    (recommends, suggests)
}

fn is_no_features_edge_case(features_with_deps: &CrateDepInfo) -> bool {
    let mut no_features_edge_case = BTreeMap::new();
    no_features_edge_case.insert("", (vec![], vec![]));
    no_features_edge_case.insert("default", (vec![""], vec![]));
    features_with_deps == &no_features_edge_case
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
fn write_feature_tests(
    testctl: &mut io::BufWriter<fs::File>,
    package: &Package,
    crate_name: &str,
    deb_upstream_version: &str,
    crate_features: &[&str],
    features_with_deps: &CrateDepInfo,
    config: &Config,
    test_deps: &[String],
) -> Result<()> {
    for f in crate_features {
        let (feature_deps, _) = transitive_deps(features_with_deps, f)?;
        let mut args = if *f == "default" || feature_deps.contains(&"default") {
            vec![]
        } else {
            vec!["--no-default-features"]
        };
        if !f.is_empty() && *f != "default" {
            args.push("--features");
            args.push(f);
        }

        let test_depends = generate_test_dependencies(f, &feature_deps, config, test_deps);
        let test_arch: Vec<String> =
            feature_test_architecture(config, features_with_deps, f)?.unwrap_or_default();
        let test_arch: Vec<&str> = test_arch.iter().map(AsRef::as_ref).collect();
        let pkgtest = PkgTest::new(
            package.name(),
            crate_name,
            f,
            deb_upstream_version,
            args,
            &test_depends,
            if feature_test_is_broken(config, features_with_deps, f)? {
                vec!["flaky"]
            } else {
                vec![]
            },
            test_arch.deref(),
        )?;
        write!(testctl, "\n{}", pkgtest)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_binary_only_package(
    control: &mut io::BufWriter<fs::File>,
    crate_info: &CrateInfo,
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
        crate_info.version(),
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
        deb_deps(config.allow_prerelease_deps, base_deps)?,
        base_deps.clone(),
        vec![],
        vec![],
        vec![],
        vec![],
    )?;

    if let Some(lockfile) = lockfile_deps {
        package.apply_lockfile_deps(lockfile);
    }
    package.apply_overrides(config, PackageKey::feature(""), vec![]);
    write!(control, "{}", package)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn prepare_binary_package_for_overrides(
    config: &Config,
    lib: bool,
    bin_name: &str,
    name_suffix: Option<&str>,
    crate_name: &str,
    bins: &[&str],
    summary_prefix: &str,
    description_prefix: &str,
    lockfile_deps: Option<&HashMap<String, semver::Version>>,
) {
    let mut bin_pkg = Package::new_bin(
        bin_name,
        name_suffix,
        if !lib {
            None
        } else {
            Some("FIXME-(packages.\"(name)\".section)")
        },
        Description {
            prefix: summary_prefix.to_string(),
            suffix: "".to_string(),
        },
        Description {
            prefix: description_prefix.to_string(),
            suffix: binary_description_suffix(crate_name, bins),
        },
    );

    if let Some(lockfile) = lockfile_deps {
        bin_pkg.apply_lockfile_deps(lockfile);
    }
    bin_pkg.apply_overrides(config, PackageKey::Bin, vec![]);
    // Skip bin package output for RPM spec - we only need library packages.
}

fn binary_description_suffix(crate_name: &str, bins: &[&str]) -> String {
    format!(
        "This package contains the following binaries built from the Rust crate\n\"{}\":\n - {}",
        crate_name,
        bins.join("\n - ")
    )
}

fn write_extra_packages(control: &mut io::BufWriter<fs::File>, config: &Config) -> Result<()> {
    for configured in config.configured_packages() {
        if let PackageKey::Extra(package) = configured {
            let mut extra_pkg = Package::new_extra(package.to_string());
            extra_pkg.apply_overrides(config, configured, vec![]);
            write!(control, "\n{}", extra_pkg)?;
        }
    }
    Ok(())
}

fn write_trailing_spec_sections(control: &mut io::BufWriter<fs::File>) -> Result<()> {
    writeln!(control)?;
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
    write!(control, "{}", trailing_sections)?;
    Ok(())
}

fn generate_test_dependencies(
    f: &str,
    feature_deps: &[&str],
    config: &Config,
    test_deps: &[String],
) -> Vec<String> {
    Some(f)
        .iter()
        .chain(feature_deps)
        .flat_map(|f| {
            config
                .package_test_depends(PackageKey::feature(f))
                .into_iter()
                .flatten()
        })
        .map(|s| s.to_string())
        .chain(test_deps.to_owned())
        .collect::<Vec<_>>()
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
    for (&f, (ref ff, ref dd)) in features_with_deps.iter() {
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

pub(crate) fn toolchain_deps(min_rust_version: &Option<String>) -> Vec<String> {
    let rustc = rustc_dep(min_rust_version, true);
    // libstd-rust-dev here is needed to pick up the right arch variant for cross-builds!
    ["cargo:native".into(), rustc, "libstd-rust-dev".into()].into()
}

fn rustc_dep(min_ver: &Option<String>, native: bool) -> String {
    let native = if native { ":native" } else { "" };
    if let Some(min_ver) = min_ver {
        format!("rustc{native} (>= {min_ver})")
    } else {
        format!("rustc{native}")
    }
}

#[cfg(test)]
mod test {
    use super::rustc_dep;

    #[test]
    fn rustc_dep_includes_minver() {
        assert_eq!(
            "rustc:native (>= 1.65)",
            rustc_dep(&Some("1.65".to_string()), true)
        );
    }

    #[test]
    fn rustc_dep_excludes_minver() {
        assert_eq!("rustc:native", rustc_dep(&None, true));
    }

    #[test]
    fn rustc_dep_includes_minver_autopkgtest() {
        assert_eq!(
            "rustc (>= 1.65)",
            rustc_dep(&Some("1.65".to_string()), false)
        );
    }

    #[test]
    fn rustc_dep_excludes_minver_autopkgtest() {
        assert_eq!("rustc", rustc_dep(&None, false));
    }
}
