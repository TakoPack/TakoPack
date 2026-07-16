use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct PypiPackage {
    pub info: PypiInfo,
    pub releases: PypiReleases,
}

#[derive(Debug, Deserialize)]
pub struct PypiInfo {
    pub author: Option<String>,
    pub license: Option<String>,
    pub summary: String,
    pub version: String,
    pub home_page: Option<String>,
    pub package_url: Option<String>,
    pub project_url: Option<String>,
    pub project_urls: Value,
    pub description: String,
    pub classifiers: Value,
}

#[derive(Debug, Deserialize)]
pub struct PypiReleases(Value);

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub struct PypiReleaseFile {
    pub filename: String,
    pub packagetype: String,
    pub yanked: bool,
    pub upload_time_iso_8601: String,
    pub url: String,
    pub digests: FileDigest,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub struct FileDigest {
    pub sha256: String,
}

impl PypiReleases {
    pub fn get_files(&self, version: &str) -> Result<Vec<PypiReleaseFile>, PypiReleasesError> {
        let version_picked = self
            .0
            .get(version)
            .ok_or_else(|| PypiReleasesError::VersionNotFound)?;

        Ok(serde_json::from_value::<Vec<PypiReleaseFile>>(
            version_picked.clone(),
        )?)
    }
}

impl Ord for PypiReleaseFile {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let self_rank = match &self.filename {
            f if f.ends_with(".tar.gz") => 2,
            f if f.ends_with(".tgz") => 1,
            _ => 0,
        };
        let other_rank = match &other.filename {
            f if f.ends_with(".tar.gz") => 2,
            f if f.ends_with(".tgz") => 1,
            _ => 0,
        };

        self_rank
            .cmp(&other_rank)
            .then(self.upload_time_iso_8601.cmp(&other.upload_time_iso_8601))
    }
}

impl PartialOrd for PypiReleaseFile {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug)]
pub enum PypiReleasesError {
    VersionNotFound,
    NoFilePicked,
    FailedParseFiles(serde_json::Error),
}

impl From<serde_json::Error> for PypiReleasesError {
    fn from(value: serde_json::Error) -> Self {
        Self::FailedParseFiles(value)
    }
}
