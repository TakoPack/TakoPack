use std::path::PathBuf;

use clap::{Parser, crate_version};
use takopack_core::config::Config;
use takopack_core::errors::Result;
use takopack_core::{takopack_info, takopack_warn, util};

use crate::crates::CrateInfo;
use crate::rpm::{self, RpmPackageInfo};

pub struct PackageProcess {
    // below state is filled in during init
    pub crate_info: CrateInfo,
    pub rpm_info: RpmPackageInfo,
    pub config_path: Option<PathBuf>,
    pub config: Config,
    pub sha256: Option<String>, // SHA256 hash of downloaded crate file
    // below state is filled in during the process
    /// Output directory as specified by the user.
    pub output_dir: Option<PathBuf>,
    pub source_modified: Option<bool>,
    /// Tempdir that contains a working copy of the eventual output.
    pub temp_output_dir: Option<tempfile::TempDir>,
    pub source_archive: Option<PathBuf>,
}

#[derive(Debug, Clone, Parser)]
pub struct PackageInitArgs {
    /// Name of the crate to package.
    pub crate_name: String,
    /// Version of the crate to package; may contain dependency operators.
    /// If empty string or omitted, resolves to the latest version.
    pub version: Option<String>,
}

#[derive(Debug, Clone, Parser)]
pub struct PackageExtractArgs {
    /// Output root directory. The package directory is created under this root.
    #[arg(long, value_name = "OUT_ROOT")]
    pub directory: Option<PathBuf>,
}

#[derive(Debug, Clone, Parser)]
pub struct PackageExecuteArgs {
    /// Assume the changelog is already bumped, and leave it alone.
    #[arg(long)]
    pub changelog_ready: bool,
    /// Reserved for compatibility with the original packaging pipeline.
    #[arg(long)]
    pub copyright_guess_harder: bool,
    /// Don't write back generated hint files to the source overlay directory.
    #[arg(long)]
    pub no_overlay_write_back: bool,
    /// Include TakoPack's built-in SPDX header in generated spec files.
    #[arg(long)]
    pub with_spdx: bool,
    /// Optional dependencies from Cargo.lock for accurate spec generation.
    #[arg(skip)]
    pub lockfile_deps: Option<std::collections::HashMap<String, semver::Version>>,
}

impl PackageProcess {
    /// More fine-grained access. For normal usage see `Self::init` instead.
    pub fn new(
        mut crate_info: CrateInfo,
        config_path: Option<PathBuf>,
        config: Config,
    ) -> Result<Self> {
        crate_info.set_includes_excludes(
            config.source_archive_excludes(),
            config.source_archive_whitelist(),
        );
        let rpm_info = RpmPackageInfo::new(&crate_info, crate_version!(), config.semver_suffix);

        // Calculate SHA256 hash for downloaded crates
        let sha256 = match crate_info.calculate_sha256() {
            Ok(hash) => {
                log::info!("Calculated SHA256: {}", hash);
                Some(hash)
            }
            Err(e) => {
                log::warn!("Failed to calculate SHA256: {:?}", e);
                None
            }
        };

        Ok(Self {
            crate_info,
            rpm_info,
            config_path,
            config,
            sha256,
            output_dir: None,
            source_modified: None,
            temp_output_dir: None,
            source_archive: None,
        })
    }

    pub fn init(init_args: PackageInitArgs) -> Result<Self> {
        let crate_name = &init_args.crate_name;
        let version = init_args.version.as_deref();
        let (config_path, config) = Config::load()?;

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
            rpm_info,
            ..
        } = self;
        // vars read; begin stage

        let output_dir = extract
            .directory
            .unwrap_or_else(|| rpm_info.package_source_dir().to_path_buf());

        let source_modified = crate_info.extract_crate(&output_dir)?;

        // Get crate info before clean (for backup)
        let crate_name = crate_info.crate_name().to_string();
        let version = crate_info.version().to_string();

        // Backup original Cargo.toml under the takopack cargo_back origin path (no cleaning)
        let cargo_toml = output_dir.join("Cargo.toml");
        if let Err(e) = takopack_core::util::backup_cargo_toml(
            &cargo_toml,
            &crate_name,
            &version,
            Some("origin"),
        ) {
            log::warn!("Failed to backup original Cargo.toml: {:?}", e);
        }

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

        let temp_output_dir =
            rpm::apply_overlay_and_patches(crate_info, config_path.as_deref(), config, output_dir)?;

        // stage finished; set vars
        self.temp_output_dir = Some(temp_output_dir);
        Ok(())
    }

    pub fn prepare_source_archive(&mut self) -> Result<()> {
        assert!(self.source_archive.is_none());
        let Self {
            crate_info,
            rpm_info,
            output_dir,
            source_modified,
            ..
        } = self;
        let output_dir = output_dir.as_ref().unwrap();
        let source_modified = source_modified.as_ref().unwrap();
        // vars read; begin stage

        let source_archive = output_dir
            .parent()
            .unwrap()
            .join(rpm_info.source_archive_path());
        rpm::prepare_source_archive(crate_info, &source_archive, *source_modified, output_dir)?;

        // stage finished; set vars
        self.source_archive = Some(source_archive);
        Ok(())
    }

    pub fn prepare_takopack_folder(&mut self, args: PackageExecuteArgs) -> Result<()> {
        let Self {
            crate_info,
            rpm_info,
            config_path,
            config,
            sha256,
            output_dir,
            temp_output_dir,
            ..
        } = self;
        let output_dir = output_dir.as_ref().unwrap();
        let temp_output_dir = temp_output_dir.as_ref().unwrap();
        rpm::prepare_takopack_folder(
            crate_info,
            rpm_info,
            config_path.as_deref(),
            config,
            output_dir,
            temp_output_dir,
            args.changelog_ready,
            args.copyright_guess_harder,
            !args.no_overlay_write_back,
            sha256.clone(),
            args.lockfile_deps, // Pass lockfile dependencies
            args.with_spdx,
        )?;

        // stage finished; set vars
        Ok(())
    }

    pub fn post_package_checks(&self) -> Result<()> {
        let Self {
            config_path,
            config,
            output_dir,
            source_archive,
            ..
        } = self;
        let output_dir = output_dir.as_ref().unwrap();
        let source_archive = source_archive.as_ref().unwrap();

        let curdir = std::env::current_dir()?;
        takopack_info!(
            concat!("Package Source: {}\n", "Source Archive for package: {}\n"),
            util::rel_p(output_dir, &curdir),
            util::rel_p(source_archive, &curdir)
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
                None => takopack_warn!("\t •  Write overrides in takopack.toml"),
                Some(c) => {
                    takopack_warn!("\t •  Add or edit overrides in your config file:");
                    takopack_warn!("\t    {}", util::rel_p(c, &curdir));
                }
            };
            match config.overlay_dir(config_path.as_deref()) {
                None => takopack_warn!(
                    "\t •  Create an overlay directory and add it to your config file with overlay = \"/path/to/overlay\""
                ),
                Some(p) => {
                    takopack_warn!("\t •  Add or edit files in your overlay directory:");
                    takopack_warn!("\t    {}", util::rel_p(&p, &curdir));
                }
            }
        }
        Ok(())
    }
}
