use std::path::PathBuf;

use anyhow::Context;

use clap::{crate_version, Parser};

use crate::config::{Config, PackageKey};
use crate::crates::CrateInfo;
use crate::errors::Result;
use crate::takopack::{self, DebInfo};
use crate::util;

pub struct PackageProcess {
    // below state is filled in during init
    pub crate_info: CrateInfo,
    pub deb_info: DebInfo,
    pub config_path: Option<PathBuf>,
    pub config: Config,
    // below state is filled in during the process
    /// Output directory as specified by the user.
    pub output_dir: Option<PathBuf>,
    pub source_modified: Option<bool>,
    /// Tempdir that contains a working copy of the eventual output.
    pub temp_output_dir: Option<tempfile::TempDir>,
    pub orig_tarball: Option<PathBuf>,
}

#[derive(Debug, Clone, Parser)]
pub struct PackageInitArgs {
    /// Name of the crate to package.
    pub crate_name: String,
    /// Version of the crate to package; may contain dependency operators.
    /// If empty string or omitted, resolves to the latest version.
    pub version: Option<String>,
    /// TOML file providing package-specific options.
    #[arg(long)]
    pub config: Option<PathBuf>,
}

#[derive(Debug, Clone, Parser)]
pub struct PackageExtractArgs {
    /// Output directory for the package. The orig tarball is named according
    /// to takopack conventions in the parent directory of this directory.
    #[arg(long)]
    pub directory: Option<PathBuf>,
}

#[derive(Debug, Clone, Parser)]
pub struct PackageExecuteArgs {
    /// Assume the changelog is already bumped, and leave it alone.
    #[arg(long)]
    pub changelog_ready: bool,
    /// Guess extra values for d/copyright. Might be slow.
    #[arg(long)]
    pub copyright_guess_harder: bool,
    /// Don't write back hint files or d/changelog to the source overlay directory.
    #[arg(long)]
    pub no_overlay_write_back: bool,
}

impl PackageProcess {
    /// More fine-grained access. For normal usage see `Self::init` instead.
    pub fn new(
        mut crate_info: CrateInfo,
        config_path: Option<PathBuf>,
        config: Config,
    ) -> Result<Self> {
        crate_info.set_includes_excludes(config.orig_tar_excludes(), config.orig_tar_whitelist());
        let deb_info = DebInfo::new(&crate_info, crate_version!(), config.semver_suffix);

        Ok(Self {
            crate_info,
            deb_info,
            config_path,
            config,
            output_dir: None,
            source_modified: None,
            temp_output_dir: None,
            orig_tarball: None,
        })
    }

    pub fn init(init_args: PackageInitArgs) -> Result<Self> {
        let crate_name = &init_args.crate_name;
        let version = init_args.version.as_deref();
        let config = init_args.config;

        let (config_path, config) = match config {
            Some(path) => {
                let config = Config::parse(&path).context("failed to parse takopack.toml")?;
                (Some(path), config)
            }
            None => (None, Config::default()),
        };

        let crate_path = config.crate_src_path(config_path.as_deref());
        let crate_info = match crate_path {
            Some(p) => CrateInfo::new_with_local_crate(crate_name, version, &p)?,
            None => CrateInfo::new(crate_name, version)?,
        };

        Self::new(crate_info, config_path, config)
    }

    pub fn crate_info(&self) -> &CrateInfo {
        &self.crate_info
    }

    pub fn temp_output_dir(&self) -> &Option<tempfile::TempDir> {
        &self.temp_output_dir
    }

    pub fn extract(&mut self, extract: PackageExtractArgs) -> Result<()> {
        assert!(self.output_dir.is_none());
        assert!(self.source_modified.is_none());
        let Self {
            crate_info,
            deb_info,
            ..
        } = self;
        // vars read; begin stage

        let output_dir = extract
            .directory
            .unwrap_or_else(|| deb_info.package_source_dir().to_path_buf());

        let source_modified = crate_info.extract_crate(&output_dir)?;

        // stage finished; set vars
        self.output_dir = Some(output_dir);
        self.source_modified = Some(source_modified);
        Ok(())
    }

    pub fn apply_overrides(&mut self) -> Result<()> {
        assert!(self.temp_output_dir.is_none());
        let Self {
            crate_info,
            config_path,
            config,
            output_dir,
            ..
        } = self;
        let output_dir = output_dir.as_ref().unwrap();
        // vars read; begin stage

        let temp_output_dir = takopack::apply_overlay_and_patches(
            crate_info,
            config_path.as_deref(),
            config,
            output_dir,
        )?;

        // stage finished; set vars
        self.temp_output_dir = Some(temp_output_dir);
        Ok(())
    }

    pub fn prepare_orig_tarball(&mut self) -> Result<()> {
        assert!(self.orig_tarball.is_none());
        let Self {
            crate_info,
            deb_info,
            output_dir,
            source_modified,
            ..
        } = self;
        let output_dir = output_dir.as_ref().unwrap();
        let source_modified = source_modified.as_ref().unwrap();
        // vars read; begin stage

        let orig_tarball = output_dir
            .parent()
            .unwrap()
            .join(deb_info.orig_tarball_path());
        takopack::prepare_orig_tarball(crate_info, &orig_tarball, *source_modified, output_dir)?;

        // stage finished; set vars
        self.orig_tarball = Some(orig_tarball);
        Ok(())
    }

    pub fn prepare_takopack_folder(&mut self, args: PackageExecuteArgs) -> Result<()> {
        let Self {
            crate_info,
            deb_info,
            config_path,
            config,
            output_dir,
            temp_output_dir,
            ..
        } = self;
        let output_dir = output_dir.as_ref().unwrap();
        let temp_output_dir = temp_output_dir.as_ref().unwrap();
        takopack::prepare_takopack_folder(
            crate_info,
            deb_info,
            config_path.as_deref(),
            config,
            output_dir,
            temp_output_dir,
            args.changelog_ready,
            args.copyright_guess_harder,
            !args.no_overlay_write_back,
        )?;

        // stage finished; set vars
        Ok(())
    }

    pub fn post_package_checks(&self) -> Result<()> {
        let Self {
            config_path,
            config,
            output_dir,
            orig_tarball,
            ..
        } = self;
        let output_dir = output_dir.as_ref().unwrap();
        let orig_tarball = orig_tarball.as_ref().unwrap();

        let curdir = std::env::current_dir()?;
        takopack_info!(
            concat!("Package Source: {}\n", "Original Tarball for package: {}\n"),
            util::rel_p(output_dir, &curdir),
            util::rel_p(orig_tarball, &curdir)
        );
        let fixmes = util::lookup_fixmes(output_dir)?;
        if !fixmes.is_empty() {
            takopack_warn!("FIXME found in the following files.");
            for f in fixmes {
                if util::hint_file_for(&f).is_some() {
                    takopack_warn!("\t(•) {}", util::rel_p(&f, &curdir));
                } else {
                    takopack_warn!("\t •  {}", util::rel_p(&f, &curdir));
                }
            }
            takopack_warn!("");
            takopack_warn!("To fix, try combinations of the following: ");
            match config_path.as_deref() {
                None => takopack_warn!("\t •  Write a config file and use it with --config"),
                Some(c) => {
                    takopack_warn!("\t •  Add or edit overrides in your config file:");
                    takopack_warn!("\t    {}", util::rel_p(c, &curdir));
                }
            };
            match config.overlay_dir(config_path.as_deref()) {
                None => takopack_warn!("\t •  Create an overlay directory and add it to your config file with overlay = \"/path/to/overlay\""),
                Some(p) => {
                    takopack_warn!("\t •  Add or edit files in your overlay directory:");
                    takopack_warn!("\t    {}", util::rel_p(&p, &curdir));
                }
            }
        }
        if let Some(architecture) = self.config.package_architecture(PackageKey::BareLib) {
            if architecture == &vec!["all"] {
                takopack_warn!("");
                takopack_warn!(
                    "'Architecture: all' override found for library package. This potentially breaks Multi-Arch,"
                );
                takopack_warn!(
                    "see https://rust-team.pages.takopack.net/book/policy.html#why-architecture-any."
                );
            }
        }
        Ok(())
    }
}
