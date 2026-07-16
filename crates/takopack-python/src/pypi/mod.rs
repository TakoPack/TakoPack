pub mod package;
mod process;

use std::{
    fs,
    path::{Path, PathBuf},
};

use package::PypiPackage;
use tempfile::TempDir;

use crate::pypi::{
    package::{PypiInfo, PypiReleaseFile, PypiReleasesError},
    process::{FetchPypiError, ReleaseFileDownloadError, VerifyError},
};

pub struct PypiFetcher<'a> {
    name: &'a str,
    version: Option<&'a str>,
    // options:
    temp_root: Option<&'a str>,
}

impl<'a> PypiFetcher<'a> {
    pub fn new(name: &'a str, version: Option<&'a str>) -> Self {
        Self {
            name,
            version,
            temp_root: None,
        }
    }

    pub fn temp_dir(&mut self, path_str: &'a str) {
        self.temp_root = Some(path_str);
    }

    // Fetch from PyPI, validate and download the source tarball picked by version.
    pub fn fetch(&self) -> Result<Pypi<'a>, FetchError> {
        let pypi_package: PypiPackage =
            process::fetch_from_pypi(self.name).map_err(|e| FetchError::FetchPypi(e))?;

        // Validate the version.
        let version = match self.version {
            Some(version) => version,
            None => &pypi_package.info.version,
        };

        let files = pypi_package
            .releases
            .get_files(version)
            .map_err(|e| FetchError::GetVersion(e))?;
        let release_file = process::pick_release_file(files)
            .ok_or_else(|| FetchError::PickFile(PypiReleasesError::NoFilePicked))?;

        let temp_dir = tempfile::Builder::new()
            .prefix("takopack-py-")
            .tempdir_in(self.temp_root.unwrap_or("."))
            .map_err(|e| FetchError::CreateTempFile(e))?;

        let archive_path = temp_dir.path().join(&release_file.filename);

        // Validate release_file checksum.
        process::download_pypi_release_file(&archive_path, &release_file)
            .map_err(|e| FetchError::DownloadReleaseFile(e))?;
        process::verify_release_file_sha256(&archive_path, &release_file)
            .map_err(|e| FetchError::Sha256Check(e))?;

        Ok(Pypi {
            name: self.name,
            version: version.to_string(),
            info: pypi_package.info,
            release_file,
            temp_resource: TempResource {
                root_dir: temp_dir,
                tarball_path: archive_path,
            },
        })
    }
}

#[derive(Debug)]
pub enum FetchError {
    FetchPypi(FetchPypiError),
    GetVersion(PypiReleasesError),
    PickFile(PypiReleasesError),
    CreateTempFile(std::io::Error),
    DownloadReleaseFile(ReleaseFileDownloadError),
    Sha256Check(VerifyError),
}

#[derive(Debug)]
pub struct Pypi<'a> {
    name: &'a str,
    version: String,
    info: PypiInfo,
    release_file: PypiReleaseFile,
    temp_resource: TempResource,
}

#[derive(Debug)]
pub struct TempResource {
    pub root_dir: TempDir,
    pub tarball_path: PathBuf,
}

impl<'a> Pypi<'a> {
    pub fn name(&self) -> &'a str {
        self.name
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn info(&self) -> &PypiInfo {
        &self.info
    }

    pub fn release_file(&self) -> &PypiReleaseFile {
        &self.release_file
    }

    pub fn temp_dir(&self) -> &std::path::Path {
        self.temp_resource.root_dir.path()
    }

    pub fn src_archive(&self) -> &std::path::Path {
        &self.temp_resource.tarball_path
    }

    pub fn extract_tar_gz(&self) -> Result<PathBuf, std::io::Error> {
        use flate2::read::GzDecoder;
        use tar::Archive;

        fn extract_tar_gz(archive_path: &Path, extract_dir: &Path) -> Result<(), std::io::Error> {
            let f = std::fs::File::open(archive_path)?;
            let gz = GzDecoder::new(f);
            let mut tar = Archive::new(gz);
            tar.unpack(extract_dir)?;
            Ok(())
        }

        fn detect_extract_root(extract_dir: &Path) -> Result<PathBuf, std::io::Error> {
            let entries = fs::read_dir(extract_dir)?;
            let mut dirs = Vec::new();
            for entry in entries {
                let path = entry?.path();
                if path.is_dir() {
                    dirs.push(path);
                }
            }
            if dirs.len() == 1 {
                Ok(dirs.remove(0))
            } else {
                Ok(extract_dir.to_path_buf())
            }
        }

        let extract_dir = self.temp_dir().join("extract");
        fs::create_dir_all(&extract_dir)?;
        extract_tar_gz(self.src_archive(), &extract_dir)?;

        let ret = detect_extract_root(&extract_dir)?;
        Ok(ret)
    }
}
