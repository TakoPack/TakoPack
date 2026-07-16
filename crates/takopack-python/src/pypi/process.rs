//! All functions

use std::{
    fs::File,
    io::{Read as _, Write as _},
    path::Path,
};

use serde_path_to_error::Error;
use sha2::{Digest as _, Sha256};

use crate::pypi::{
    package::PypiReleaseFile,
    process::{
        ReleaseFileDownloadError::{
            FailedCreateFile, FailedFlush, FailedReadResponse, FailedRequest,
        },
        VerifyError::{EmptySha256, FsError, Sha256MisMatch},
    },
};

use super::package::PypiPackage;

/// Fetch a JSON from "https://pypi.org/pypi/{package_name}/json", and deserialize it.
pub fn fetch_from_pypi(package_name: &str) -> Result<PypiPackage, FetchPypiError> {
    let url = format!("https://pypi.org/pypi/{}/json", package_name);
    let response = ureq::get(&url)
        .call()
        .map_err(|e| FetchPypiError::FailedRequest(e))?
        .into_string()
        .map_err(|e| FetchPypiError::FailedReadResponse(e))?;

    let de = &mut serde_json::Deserializer::from_slice(response.as_bytes());
    let result: Result<PypiPackage, _> = serde_path_to_error::deserialize(de);

    result.map_err(|e| FetchPypiError::FailedParseJson(e))
}

#[derive(Debug)]
pub enum FetchPypiError {
    FailedParseJson(Error<serde_json::Error>),
    FailedRequest(ureq::Error),
    FailedReadResponse(std::io::Error),
}

pub fn pick_release_file(files: Vec<PypiReleaseFile>) -> Option<PypiReleaseFile> {
    // Prefer .tar.gz over .tgz and, for equal formats, pick the latest upload.
    let mut files_filtered = files
        .into_iter()
        .filter(|file| file.packagetype == "sdist")
        .filter(|file| !file.yanked)
        .collect::<Vec<PypiReleaseFile>>();

    files_filtered.sort_unstable();
    files_filtered.reverse();
    files_filtered.pop()
}

#[inline]
pub(super) fn download_pypi_release_file(
    destination: &Path,
    file: &PypiReleaseFile,
) -> Result<(), ReleaseFileDownloadError> {
    let response = ureq::get(&file.url).call().map_err(|e| FailedRequest(e))?;
    let mut reader = response.into_reader();

    let mut file = File::create(destination).map_err(|e| FailedCreateFile(e))?;

    std::io::copy(&mut reader, &mut file).map_err(|e| FailedReadResponse(e))?;
    file.flush().map_err(|e| FailedFlush(e))?;

    Ok(())
}

#[derive(Debug)]
pub enum ReleaseFileDownloadError {
    FailedRequest(ureq::Error),
    FailedCreateFile(std::io::Error),
    FailedReadResponse(std::io::Error),
    FailedFlush(std::io::Error),
}

#[inline]
pub(super) fn verify_release_file_sha256(
    path: &Path,
    file: &PypiReleaseFile,
) -> Result<(), VerifyError> {
    let trimed = file.digests.sha256.trim();
    let expected_sha256 = if trimed.is_empty() {
        return Err(EmptySha256);
    } else {
        trimed
    };

    let mut file = File::open(path).map_err(|e| FsError(e))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf).map_err(|e| FsError(e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    let actual = format!("{:x}", hasher.finalize());
    if actual != expected_sha256.to_ascii_lowercase() {
        return Err(Sha256MisMatch(format!(
            "sha256 mismatch: expected {}, got {}",
            expected_sha256, actual,
        )));
    }

    Ok(())
}

#[derive(Debug)]
pub enum VerifyError {
    EmptySha256,
    Sha256MisMatch(String),
    FsError(std::io::Error),
}
