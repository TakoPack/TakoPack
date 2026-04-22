use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tar::Archive;

const FALLBACK_LICENSE: &str = "LicenseRef-Unknown-Please-Check-Manual";

fn re_license_ops_with_capture() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Treat textual operators only when separated by whitespace, so SPDX IDs
    // like GPL-2.0-or-later are not split into broken tokens.
    RE.get_or_init(|| Regex::new("(?i)(?:\\s*(?:/|\\||,)\\s*|\\s+(?:or|and)\\s+)").unwrap())
}

fn re_spdx_like_core() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9.+-]+(?:\s+WITH\s+[A-Za-z0-9.+-]+)?$").unwrap())
}

fn re_md_link() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[([^\]]+)\]\([^\)]+\)").unwrap())
}

fn re_html_tag() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<[^>]+>").unwrap())
}

pub fn process_python_package(
    package_name: &str,
    version: Option<&str>,
    output_dir: Option<PathBuf>,
) -> Result<()> {
    // Always create the target folder first so both normal and fallback flows
    // can write a deterministic spec path.
    let srcname = normalize_srcname(package_name);
    let output_base = output_dir.unwrap_or_else(|| PathBuf::from("."));
    let package_dir = output_base.join(format!("python-{}", srcname));
    fs::create_dir_all(&package_dir).with_context(|| {
        format!(
            "failed to create output directory for python package: {}",
            package_dir.display()
        )
    })?;

    // If the package does not exist on PyPI, generate an editable skeleton
    // instead of failing the whole batch.
    let pypi_json = match fetch_pypi_json(package_name) {
        Ok(v) => v,
        Err(e) => {
            let skeleton_version = version
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .unwrap_or("0.0.0");
            let spec_path = package_dir.join(format!("python-{}.spec", srcname));
            let spec_content = render_skeleton_spec(&srcname, skeleton_version, package_name, &e);
            fs::write(&spec_path, spec_content).with_context(|| {
                format!("failed to write generated spec: {}", spec_path.display())
            })?;
            println!(
                "[WARN] PyPI metadata not found for {}. Generated skeleton spec: {}",
                package_name,
                spec_path.display()
            );
            return Ok(());
        }
    };
    let info = pypi_json
        .get("info")
        .and_then(Value::as_object)
        .context("invalid PyPI metadata: missing info object")?;

    let resolved_version = match version {
        Some(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => info
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_string)
            .context("failed to resolve latest version from PyPI metadata")?,
    };

    // Some versions publish wheels only; in that case we still emit a skeleton
    // because spec generation depends on source archives.
    let release_file = match pick_release_file(&pypi_json, &resolved_version) {
        Ok(v) => v,
        Err(e) => {
            let spec_path = package_dir.join(format!("python-{}.spec", srcname));
            let spec_content = render_skeleton_spec(&srcname, &resolved_version, package_name, &e);
            fs::write(&spec_path, spec_content).with_context(|| {
                format!("failed to write generated spec: {}", spec_path.display())
            })?;
            println!(
                "[WARN] No source archive available for {}@{}. Generated skeleton spec: {}",
                package_name,
                resolved_version,
                spec_path.display()
            );
            return Ok(());
        }
    };
    let temp_dir = tempfile::Builder::new()
        .prefix("takopack-py-")
        .tempdir_in(".")
        .context("failed to create temporary directory")?;
    let archive_path = temp_dir.path().join(&release_file.filename);
    download_file(&release_file.url, &archive_path)?;
    verify_sha256(&archive_path, &release_file.sha256)?;

    let extract_dir = temp_dir.path().join("extract");
    fs::create_dir_all(&extract_dir)?;
    extract_tar_gz(&archive_path, &extract_dir)?;

    let extracted_root = detect_extract_root(&extract_dir)?;
    let mut meta = SpecMeta {
        summary: info
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        license: resolve_license_from_info(info),
        url: preferred_url_from_info(info)
            .or_else(|| {
                info.get("package_url")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| format!("https://pypi.org/project/{}/", package_name)),
        vcs: String::new(),
        description: info
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    };

    enrich_meta_from_pyproject(&extracted_root, &mut meta);
    enrich_meta_from_pkg_info(&extracted_root, &mut meta);
    enrich_meta_from_license_files(&extracted_root, &mut meta);
    finalize_meta(&mut meta, package_name);

    let pypi_name = filename_dist_name(&release_file.filename, &resolved_version)
        .unwrap_or_else(|| package_name.to_string());
    let spec_path = package_dir.join(format!("python-{}.spec", srcname));
    let spec_content = render_spec(
        &srcname,
        &pypi_name,
        &resolved_version,
        &release_file.filename,
        &release_file.sha256,
        &meta,
    );
    fs::write(&spec_path, spec_content)
        .with_context(|| format!("failed to write generated spec: {}", spec_path.display()))?;

    println!("Generated spec file: {}", spec_path.display());
    Ok(())
}

struct SpecMeta {
    summary: String,
    license: String,
    url: String,
    vcs: String,
    description: String,
}

struct ReleaseFile {
    filename: String,
    url: String,
    sha256: String,
    upload_time: String,
}

fn fetch_pypi_json(package_name: &str) -> Result<Value> {
    let url = format!("https://pypi.org/pypi/{}/json", package_name);
    let response = ureq::get(&url)
        .call()
        .with_context(|| "failed to query PyPI metadata")?;
    let mut reader = response.into_reader();
    let mut body = Vec::new();
    reader
        .read_to_end(&mut body)
        .with_context(|| "failed to read PyPI metadata response")?;
    let json: Value =
        serde_json::from_slice(&body).context("failed to parse PyPI JSON metadata")?;
    Ok(json)
}

fn pick_release_file(pypi_json: &Value, version: &str) -> Result<ReleaseFile> {
    let releases = pypi_json
        .get("releases")
        .and_then(Value::as_object)
        .context("invalid PyPI metadata: missing releases object")?;
    let files = releases
        .get(version)
        .and_then(Value::as_array)
        .with_context(|| format!("version {} not found on PyPI", version))?;

    // Prefer .tar.gz over .tgz and, for equal formats, pick the latest upload.
    let mut selected: Option<ReleaseFile> = None;
    let mut selected_rank: i32 = -1;
    for file in files {
        let obj = match file.as_object() {
            Some(v) => v,
            None => continue,
        };
        if obj
            .get("packagetype")
            .and_then(Value::as_str)
            .unwrap_or_default()
            != "sdist"
        {
            continue;
        }
        if obj.get("yanked").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }

        let filename = match obj.get("filename").and_then(Value::as_str) {
            Some(v) => v.to_string(),
            None => continue,
        };
        let rank = if filename.ends_with(".tar.gz") {
            2
        } else if filename.ends_with(".tgz") {
            1
        } else {
            0
        };
        if rank <= 0 {
            continue;
        }

        let candidate = ReleaseFile {
            filename,
            url: obj
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            sha256: obj
                .get("digests")
                .and_then(Value::as_object)
                .and_then(|dig| dig.get("sha256"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            upload_time: obj
                .get("upload_time_iso_8601")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        };
        if candidate.url.is_empty() || candidate.sha256.is_empty() {
            continue;
        }

        let should_replace = match &selected {
            None => true,
            Some(current) => {
                rank > selected_rank
                    || (rank == selected_rank && candidate.upload_time > current.upload_time)
            }
        };
        if should_replace {
            selected_rank = rank;
            selected = Some(candidate);
        }
    }

    selected.with_context(|| {
        format!(
            "no supported source distribution (.tar.gz/.tgz) found on PyPI for version {}",
            version
        )
    })
}

fn download_file(url: &str, destination: &Path) -> Result<()> {
    let response = ureq::get(url)
        .call()
        .with_context(|| format!("failed to download source archive: {}", url))?;
    let mut reader = response.into_reader();
    let mut file = fs::File::create(destination).with_context(|| {
        format!(
            "failed to create destination file: {}",
            destination.display()
        )
    })?;
    std::io::copy(&mut reader, &mut file).with_context(|| {
        format!(
            "failed to write downloaded archive: {}",
            destination.display()
        )
    })?;
    file.flush().with_context(|| {
        format!(
            "failed to flush downloaded archive: {}",
            destination.display()
        )
    })?;
    Ok(())
}

fn verify_sha256(path: &Path, expected_sha256: &str) -> Result<()> {
    if expected_sha256.trim().is_empty() {
        return Ok(());
    }

    let mut file = fs::File::open(path).with_context(|| {
        format!(
            "failed to open downloaded archive for checksum: {}",
            path.display()
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf).with_context(|| {
            format!(
                "failed to read downloaded archive for checksum: {}",
                path.display()
            )
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    let actual = format!("{:x}", hasher.finalize());
    if actual != expected_sha256.to_ascii_lowercase() {
        anyhow::bail!(
            "sha256 mismatch for {}: expected {}, got {}",
            path.display(),
            expected_sha256,
            actual
        );
    }

    Ok(())
}

fn extract_tar_gz(archive_path: &Path, extract_dir: &Path) -> Result<()> {
    let f = fs::File::open(archive_path)
        .with_context(|| format!("failed to open archive: {}", archive_path.display()))?;
    let decoder = GzDecoder::new(f);
    let mut archive = Archive::new(decoder);
    archive
        .unpack(extract_dir)
        .with_context(|| format!("failed to extract archive: {}", archive_path.display()))?;
    Ok(())
}

fn detect_extract_root(extract_dir: &Path) -> Result<PathBuf> {
    let entries = fs::read_dir(extract_dir).with_context(|| {
        format!(
            "failed to read extract directory: {}",
            extract_dir.display()
        )
    })?;
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

fn preferred_url_from_info(info: &serde_json::Map<String, Value>) -> Option<String> {
    if let Some(project_urls) = info.get("project_urls").and_then(Value::as_object) {
        // Try repository-like keys first so URL is as close as possible to VCS.
        let candidates = [
            "Repository",
            "Source",
            "Source Code",
            "Code",
            "Homepage",
            "Home",
            "Bug Tracker",
        ];
        for key in candidates {
            if let Some(v) = project_urls.get(key).and_then(Value::as_str) {
                if let Some(cleaned) = normalize_url(v) {
                    return Some(cleaned);
                }
            }
        }
        for (_, v) in project_urls {
            if let Some(url) = v.as_str() {
                if let Some(git_url) = normalize_git_url(url) {
                    return Some(git_url);
                }
            }
        }
        if let Some((_, v)) = project_urls.iter().next() {
            if let Some(url) = v.as_str() {
                if let Some(cleaned) = normalize_url(url) {
                    return Some(cleaned);
                }
            }
        }
    }
    let home_page = info
        .get("home_page")
        .and_then(Value::as_str)
        .and_then(non_empty)
        .and_then(normalize_url);

    home_page.or_else(|| {
        info.get("project_url")
            .and_then(Value::as_str)
            .and_then(non_empty)
            .and_then(normalize_url)
    })
}

fn enrich_meta_from_pyproject(root: &Path, meta: &mut SpecMeta) {
    let pyproject_path = root.join("pyproject.toml");
    if !pyproject_path.exists() {
        return;
    }
    let content = match fs::read_to_string(&pyproject_path) {
        Ok(v) => v,
        Err(_) => return,
    };
    let value: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(_) => return,
    };

    if let Some(project) = value.get("project").and_then(toml::Value::as_table) {
        if meta.summary.trim().is_empty() {
            if let Some(desc) = project.get("description").and_then(toml::Value::as_str) {
                meta.summary = desc.to_string();
            }
        }
        if meta.license.trim().is_empty() {
            if let Some(license) = project.get("license") {
                if let Some(text) = license.as_str() {
                    meta.license = text.to_string();
                } else if let Some(tbl) = license.as_table() {
                    if let Some(text) = tbl.get("text").and_then(toml::Value::as_str) {
                        meta.license = text.to_string();
                    } else if let Some(file) = tbl.get("file").and_then(toml::Value::as_str) {
                        meta.license = format!("SEE LICENSE IN {}", file);
                    }
                }
            }
        }
        if let Some(urls) = project.get("urls").and_then(toml::Value::as_table) {
            for key in ["Repository", "Source", "Source Code", "Code", "Homepage"] {
                if let Some(url) = urls.get(key).and_then(toml::Value::as_str) {
                    assign_preferred_url(meta, url);
                }
            }
            for (_, value) in urls {
                if let Some(url) = value.as_str() {
                    assign_preferred_url(meta, url);
                }
            }
        }
    }

    if let Some(poetry) = value
        .get("tool")
        .and_then(toml::Value::as_table)
        .and_then(|tool| tool.get("poetry"))
        .and_then(toml::Value::as_table)
    {
        if meta.summary.trim().is_empty() {
            if let Some(desc) = poetry.get("description").and_then(toml::Value::as_str) {
                meta.summary = desc.to_string();
            }
        }
        if meta.license.trim().is_empty() {
            if let Some(license) = poetry.get("license").and_then(toml::Value::as_str) {
                meta.license = license.to_string();
            }
        }
        if let Some(url) = poetry.get("repository").and_then(toml::Value::as_str) {
            assign_preferred_url(meta, url);
        }
        if let Some(url) = poetry.get("homepage").and_then(toml::Value::as_str) {
            assign_preferred_url(meta, url);
        }
    }
}

fn enrich_meta_from_pkg_info(root: &Path, meta: &mut SpecMeta) {
    if let Some(pkg_info_path) = find_pkg_info(root) {
        let content = match fs::read_to_string(pkg_info_path) {
            Ok(v) => v,
            Err(_) => return,
        };
        let mut in_body = false;
        let mut body = String::new();
        let mut license_classifiers: Vec<String> = Vec::new();
        for line in content.lines() {
            if !in_body && line.is_empty() {
                in_body = true;
                continue;
            }
            if in_body {
                body.push_str(line);
                body.push('\n');
                continue;
            }
            if meta.summary.trim().is_empty() && line.starts_with("Summary: ") {
                meta.summary = line.trim_start_matches("Summary: ").to_string();
            } else if meta.license.trim().is_empty() && line.starts_with("License-Expression: ") {
                meta.license = line.trim_start_matches("License-Expression: ").to_string();
            } else if meta.license.trim().is_empty() && line.starts_with("License: ") {
                meta.license = line.trim_start_matches("License: ").to_string();
            } else if line.starts_with("Classifier: ") {
                let classifier = line.trim_start_matches("Classifier: ").trim();
                if classifier.starts_with("License ::") {
                    license_classifiers.push(classifier.to_string());
                }
            } else if line.starts_with("Home-page: ") {
                assign_preferred_url(meta, line.trim_start_matches("Home-page: ").trim());
            } else if line.starts_with("Project-URL: ") {
                let content = line.trim_start_matches("Project-URL: ").trim();
                if let Some((_, url)) = content.split_once(',') {
                    assign_preferred_url(meta, url.trim());
                } else {
                    assign_preferred_url(meta, content);
                }
            } else if meta.description.trim().is_empty() && line.starts_with("Description: ") {
                meta.description = line.trim_start_matches("Description: ").to_string();
            }
        }

        if normalize_license_to_spdx(&meta.license).is_empty() {
            for classifier in license_classifiers {
                if let Some(license_part) = classifier.rsplit("::").next() {
                    let spdx = normalize_license_to_spdx(license_part.trim());
                    if !spdx.is_empty() {
                        meta.license = spdx;
                        break;
                    }
                }
            }
        }

        // Prefer long description body from sdist metadata when available.
        if !body.trim().is_empty() {
            meta.description = body;
        }
    }
}

fn find_pkg_info(root: &Path) -> Option<PathBuf> {
    let root_pkg = root.join("PKG-INFO");
    if root_pkg.exists() {
        return Some(root_pkg);
    }

    let entries = fs::read_dir(root).ok()?;
    for entry in entries {
        let path = entry.ok()?.path();
        if path.is_dir() {
            let is_egg_info = path
                .extension()
                .and_then(OsStr::to_str)
                .map(|ext| ext == "egg-info" || ext == "dist-info")
                .unwrap_or(false);
            if !is_egg_info {
                continue;
            }
            let pkg_info = path.join("PKG-INFO");
            if pkg_info.exists() {
                return Some(pkg_info);
            }
        }
    }
    None
}

fn finalize_meta(meta: &mut SpecMeta, fallback_name: &str) {
    meta.summary = cleanup_single_line(&meta.summary);
    if meta.summary.is_empty() {
        meta.summary = format!("Python package {}", fallback_name);
    }

    meta.license = normalize_license_to_spdx(&meta.license);
    if meta.license.is_empty() {
        // Keep SPDX-compatible placeholder and explicitly ask for manual verification.
        meta.license = FALLBACK_LICENSE.to_string();
    }

    meta.url = cleanup_single_line(&meta.url);
    if meta.url.is_empty() {
        meta.url = format!("https://pypi.org/project/{}/", fallback_name);
    } else if is_git_repo_url(&meta.url)
        && (meta.url.starts_with("https://") || meta.url.starts_with("http://"))
    {
        meta.url = meta.url.trim_end_matches('/').to_string();
    }

    meta.vcs = cleanup_single_line(&meta.vcs);

    let description = best_description_paragraph(&meta.description, &meta.summary);
    meta.description = if description.is_empty() {
        meta.summary.clone()
    } else {
        description
    };
}

fn render_skeleton_spec(
    srcname: &str,
    version: &str,
    package_name: &str,
    reason: &anyhow::Error,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("%global srcname {}\n\n", srcname));
    out.push_str("Name:           python-%{srcname}\n");
    out.push_str(&format!("Version:        {}\n", version));
    out.push_str("Release:        %autorelease\n");
    out.push_str("Summary:        \n");
    out.push_str(&format!("License:        {}\n", FALLBACK_LICENSE));
    out.push_str("URL:            \n");
    out.push_str("#!RemoteAsset:  sha256:\n");
    out.push_str("Source0:        \n");
    out.push_str("BuildArch:      noarch\n");
    out.push_str("BuildSystem:    pyproject\n\n");
    out.push_str("BuildOption(install):  -l %{srcname}\n\n");
    out.push_str("BuildRequires:  pyproject-rpm-macros\n");
    out.push_str("BuildRequires:  pkgconfig(python3)\n\n");
    out.push_str("Provides:       python3-%{srcname} = %{version}-%{release}\n");
    out.push_str("%python_provide python3-%{srcname}\n\n");
    out.push_str("%description\n");
    out.push_str("TODO: PyPI package metadata/source archive unavailable, please fill in summary, license, URL and Source0 manually.\n\n");
    out.push_str("%generate_buildrequires\n");
    out.push_str("%pyproject_buildrequires\n\n");
    out.push_str("%files -f %{pyproject_files}\n\n");
    out.push_str("%changelog\n");
    out.push_str("%autochangelog\n\n");
    out.push_str("# Manual check note:\n");
    out.push_str(&format!("# package: {}\n", package_name));
    out.push_str(&format!(
        "# reason: {}\n",
        cleanup_single_line(&reason.to_string())
    ));
    out
}

fn assign_preferred_url(meta: &mut SpecMeta, candidate: &str) {
    if let Some(git_url) = normalize_git_url(candidate) {
        if git_url.starts_with("https://") || git_url.starts_with("http://") {
            let cleaned = git_url.trim_end_matches('/').to_string();
            meta.vcs.clear();
            if meta.url.trim().is_empty() {
                meta.url = cleaned;
                return;
            }

            // Keep existing URL unless the new one clearly points to a VCS repository.
            let current_is_git = is_git_repo_url(meta.url.trim());
            if !current_is_git {
                meta.url = cleaned;
            }
            return;
        }

        // Non-HTTP git endpoint should be emitted as VCS, not URL.
        meta.vcs = format!("git:{}", git_url);
        return;
    }

    let Some(cleaned) = normalize_url(candidate) else {
        return;
    };

    if meta.url.trim().is_empty() {
        meta.url = cleaned;
        return;
    }

    // Keep existing URL unless the new one clearly points to a VCS repository.
    let current_is_git = is_git_repo_url(meta.url.trim());
    let candidate_is_git = is_git_repo_url(&cleaned);
    if candidate_is_git && !current_is_git {
        meta.url = cleaned;
    }
}

fn normalize_url(raw: &str) -> Option<String> {
    let trimmed = cleanup_single_line(raw);
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return Some(trimmed);
    }
    None
}

fn normalize_git_url(raw: &str) -> Option<String> {
    let mut url = cleanup_single_line(raw);
    if url.is_empty() {
        return None;
    }
    if let Some(stripped) = url.strip_prefix("git+") {
        url = stripped.to_string();
    }
    if let Some((without_fragment, _)) = url.split_once('#') {
        url = without_fragment.to_string();
    }
    if let Some(canonical) = canonicalize_hosted_git_repo_url(&url) {
        return Some(canonical);
    }
    if is_git_repo_url(&url) {
        return Some(url);
    }
    None
}

fn is_git_repo_url(url: &str) -> bool {
    if canonicalize_hosted_git_repo_url(url).is_some() {
        return true;
    }

    let lower = url.to_ascii_lowercase();
    lower.starts_with("git@") || lower.ends_with(".git")
}

fn canonicalize_hosted_git_repo_url(raw: &str) -> Option<String> {
    let cleaned = cleanup_single_line(raw);
    let (scheme, rest) = cleaned.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }

    let (host_with_port, path_and_more) = rest.split_once('/')?;
    let host = host_with_port
        .split(':')
        .next()
        .unwrap_or(host_with_port)
        .to_ascii_lowercase();

    if host != "github.com"
        && host != "gitlab.com"
        && host != "bitbucket.org"
        && host != "codeberg.org"
    {
        return None;
    }

    let path_no_query = path_and_more.split('?').next().unwrap_or(path_and_more);
    let path = path_no_query.split('#').next().unwrap_or(path_no_query);
    let mut segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 2 {
        return None;
    }

    // Drop common non-repository page suffixes, e.g. /issues, /pull, /tree/main.
    let non_repo_markers = [
        "-",
        "issues",
        "issue",
        "pull",
        "pulls",
        "merge_requests",
        "merge-requests",
        "commits",
        "commit",
        "tree",
        "blob",
        "releases",
        "release",
        "actions",
        "wiki",
        "wikis",
        "src",
    ];
    if let Some(idx) = segments
        .iter()
        .position(|seg| non_repo_markers.contains(seg) && segments.len() > 2)
    {
        segments.truncate(idx);
    }

    if host == "github.com" || host == "bitbucket.org" {
        segments.truncate(2);
    }
    if segments.len() < 2 {
        return None;
    }

    Some(format!("{}://{}/{}", scheme, host, segments.join("/")))
}

fn enrich_meta_from_license_files(root: &Path, meta: &mut SpecMeta) {
    if !normalize_license_to_spdx(&meta.license).is_empty() {
        return;
    }

    if let Some(spdx) = detect_license_from_source_tree(root) {
        meta.license = spdx;
    }
}

fn detect_license_from_source_tree(root: &Path) -> Option<String> {
    // Common top-level hints first.
    let common = [
        "LICENSE",
        "LICENSE.txt",
        "LICENSE.md",
        "COPYING",
        "COPYING.txt",
        "NOTICE",
    ];
    for name in common {
        let p = root.join(name);
        if let Some(spdx) = detect_license_from_file(&p) {
            return Some(spdx);
        }
    }

    // Fallback: shallow tree scan for likely license files.
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for entry in entries {
            let entry = match entry {
                Ok(v) => v,
                Err(_) => continue,
            };
            let path = entry.path();
            if path.is_dir() {
                if depth < 3 {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !looks_like_license_file(&path) {
                continue;
            }
            if let Some(spdx) = detect_license_from_file(&path) {
                return Some(spdx);
            }
        }
    }
    None
}

fn looks_like_license_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    name.starts_with("license")
        || name.starts_with("copying")
        || name.starts_with("notice")
        || name.starts_with("copyright")
}

fn detect_license_from_file(path: &Path) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    let meta = fs::metadata(path).ok()?;
    if meta.len() > 256 * 1024 {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    detect_spdx_from_license_text(&text)
}

fn detect_spdx_from_license_text(text: &str) -> Option<String> {
    // Heuristic matcher for common license full texts in source tarballs.
    let lower = text.to_ascii_lowercase();

    if lower.contains("permission is hereby granted")
        && lower.contains("software is provided \"as is\"")
    {
        return Some("MIT".to_string());
    }
    if lower.contains("apache license") && lower.contains("version 2.0") {
        return Some("Apache-2.0".to_string());
    }
    if (lower.contains("redistribution and use in source and binary forms")
        && lower.contains("neither the name of"))
        || lower.contains("bsd 3-clause")
    {
        return Some("BSD-3-Clause".to_string());
    }
    if lower.contains("redistribution and use in source and binary forms")
        && !lower.contains("neither the name of")
        || lower.contains("bsd 2-clause")
    {
        return Some("BSD-2-Clause".to_string());
    }
    if lower.contains("mozilla public license") && lower.contains("version 2.0") {
        return Some("MPL-2.0".to_string());
    }
    if lower.contains("gnu lesser general public license") && lower.contains("version 3") {
        if lower.contains("or any later version") {
            return Some("LGPL-3.0-or-later".to_string());
        }
        return Some("LGPL-3.0-only".to_string());
    }
    if lower.contains("gnu lesser general public license") && lower.contains("version 2.1") {
        if lower.contains("or any later version") {
            return Some("LGPL-2.1-or-later".to_string());
        }
        return Some("LGPL-2.1-only".to_string());
    }
    if lower.contains("gnu general public license") && lower.contains("version 3") {
        if lower.contains("or any later version") {
            return Some("GPL-3.0-or-later".to_string());
        }
        return Some("GPL-3.0-only".to_string());
    }
    if lower.contains("gnu general public license") && lower.contains("version 2") {
        if lower.contains("or any later version") {
            return Some("GPL-2.0-or-later".to_string());
        }
        return Some("GPL-2.0-only".to_string());
    }
    if lower.contains("isc license") {
        return Some("ISC".to_string());
    }
    None
}

fn resolve_license_from_info(info: &serde_json::Map<String, Value>) -> String {
    let raw = info
        .get("license")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if !raw.is_empty() {
        return raw;
    }

    let classifiers = match info.get("classifiers").and_then(Value::as_array) {
        Some(v) => v,
        None => return String::new(),
    };
    for classifier in classifiers {
        let text = match classifier.as_str() {
            Some(v) => v,
            None => continue,
        };
        if !text.starts_with("License ::") {
            continue;
        }
        let license_part = text.rsplit("::").next().unwrap_or("").trim();
        if license_part.is_empty() {
            continue;
        }
        let spdx = normalize_license_to_spdx(license_part);
        if !spdx.is_empty() {
            return spdx;
        }
    }
    String::new()
}

fn render_spec(
    srcname: &str,
    pypi_name: &str,
    version: &str,
    filename: &str,
    sha256: &str,
    meta: &SpecMeta,
) -> String {
    let mut out = String::new();
    let needs_pypi_name_global = pypi_name.to_lowercase() != srcname;
    let source_ext = if let Some(ext) = filename.strip_prefix(&format!("{}-{}", pypi_name, version))
    {
        ext.to_string()
    } else if let Some(ext) = filename.strip_prefix(&format!("{}-{}", srcname, version)) {
        ext.to_string()
    } else if filename.ends_with(".tgz") {
        ".tgz".to_string()
    } else {
        ".tar.gz".to_string()
    };
    let source_stem = if needs_pypi_name_global {
        "%{pypi_name}"
    } else {
        "%{srcname}"
    };
    let source0 = format!(
        "https://files.pythonhosted.org/packages/source/{}/%{{srcname}}/{}-%{{version}}{}",
        srcname.chars().next().unwrap_or('p'),
        source_stem,
        source_ext
    );

    out.push_str(&format!("%global srcname {}\n", srcname));
    if needs_pypi_name_global {
        out.push_str(&format!("%global pypi_name {}\n", pypi_name));
    }
    out.push('\n');

    out.push_str("Name:           python-%{srcname}\n");
    out.push_str(&format!("Version:        {}\n", version));
    out.push_str("Release:        %autorelease\n");
    out.push_str(&format!("Summary:        {}\n", meta.summary));
    out.push_str(&format!("License:        {}\n", meta.license));
    out.push_str(&format!("URL:            {}\n", meta.url));
    if !meta.vcs.is_empty() {
        out.push_str(&format!("VCS:            {}\n", meta.vcs));
    }
    out.push_str(&format!("#!RemoteAsset:  sha256:{}\n", sha256));
    out.push_str(&format!("Source0:        {}\n", source0));
    out.push_str("BuildArch:      noarch\n");
    out.push_str("BuildSystem:    pyproject\n\n");
    if needs_pypi_name_global {
        out.push_str("BuildOption(install):  -l %{pypi_name}\n\n");
    } else {
        out.push_str("BuildOption(install):  -l %{srcname}\n\n");
    }
    out.push_str("BuildRequires:  pyproject-rpm-macros\n");
    out.push_str("BuildRequires:  pkgconfig(python3)\n\n");
    out.push_str("Provides:       python3-%{srcname} = %{version}-%{release}\n");
    out.push_str("%python_provide python3-%{srcname}\n\n");
    out.push_str("%description\n");
    out.push_str(&meta.description);
    out.push_str("\n\n");
    out.push_str("%generate_buildrequires\n");
    out.push_str("%pyproject_buildrequires\n\n");
    out.push_str("%files -f %{pyproject_files}\n\n");
    out.push_str("%changelog\n");
    out.push_str("%autochangelog\n");
    out
}

fn filename_dist_name(filename: &str, version: &str) -> Option<String> {
    let suffixes = [format!("-{}.tar.gz", version), format!("-{}.tgz", version)];
    for suffix in suffixes {
        if let Some(stripped) = filename.strip_suffix(&suffix) {
            return Some(stripped.to_string());
        }
    }
    None
}

fn normalize_srcname(name: &str) -> String {
    name.trim().to_lowercase().replace('_', "-")
}

fn non_empty(s: &str) -> Option<&str> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s)
    }
}

fn cleanup_single_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_license_to_spdx(raw: &str) -> String {
    let cleaned = cleanup_single_line(raw);
    if cleaned.is_empty() {
        return String::new();
    }

    let lower = cleaned.to_ascii_lowercase();
    let direct = map_license_alias(&lower);
    if !direct.is_empty() {
        return direct;
    }

    // Frequently seen full MIT text from PyPI metadata.
    if lower.contains("permission is hereby granted")
        && lower.contains("software is provided \"as is\"")
    {
        return "MIT".to_string();
    }

    let operators = ["/", "|", " or ", " and ", ","];
    if operators.iter().any(|op| lower.contains(op)) {
        if let Some(expr) = normalize_composite_license_expression(&cleaned) {
            return expr;
        }
    }

    String::new()
}

fn normalize_composite_license_expression(input: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut ops: Vec<String> = Vec::new();
    let mut last = 0usize;

    for m in re_license_ops_with_capture().find_iter(input) {
        let token = input[last..m.start()].trim();
        if token.is_empty() {
            return None;
        }
        let normalized = normalize_license_token(token)?;
        parts.push(normalized);

        let op_raw = m.as_str().to_ascii_lowercase();
        let op = if op_raw.contains("and") { "AND" } else { "OR" };
        ops.push(op.to_string());
        last = m.end();
    }

    let tail = input[last..].trim();
    if tail.is_empty() {
        return None;
    }
    parts.push(normalize_license_token(tail)?);

    if parts.len() != ops.len() + 1 {
        return None;
    }

    let mut out = String::new();
    for (idx, part) in parts.iter().enumerate() {
        if idx > 0 {
            out.push(' ');
            out.push_str(&ops[idx - 1]);
            out.push(' ');
        }
        out.push_str(part);
    }
    Some(out)
}

fn normalize_license_token(token: &str) -> Option<String> {
    let mut t = cleanup_single_line(token);
    if t.is_empty() {
        return None;
    }

    let mut prefix = String::new();
    while t.starts_with('(') {
        prefix.push('(');
        t = t[1..].trim_start().to_string();
    }

    let mut suffix = String::new();
    while t.ends_with(')') {
        suffix.insert(0, ')');
        t.pop();
        t = t.trim_end().to_string();
    }

    if t.is_empty() {
        return None;
    }

    let mapped = map_license_alias(&t.to_ascii_lowercase());
    if !mapped.is_empty() {
        return Some(format!("{}{}{}", prefix, mapped, suffix));
    }

    if re_spdx_like_core().is_match(&t) {
        return Some(format!("{}{}{}", prefix, t, suffix));
    }

    None
}

fn map_license_alias(lower: &str) -> String {
    match lower.trim() {
        "mit" | "mit license" => "MIT".to_string(),
        "apache-2.0"
        | "apache 2.0"
        | "apache software license"
        | "apache license 2.0"
        | "apache license, version 2.0" => "Apache-2.0".to_string(),
        "bsd-2-clause" | "bsd 2-clause" | "bsd 2 clause" => "BSD-2-Clause".to_string(),
        "bsd-3-clause" | "bsd 3-clause" | "bsd 3 clause" | "bsd license" => {
            "BSD-3-Clause".to_string()
        }
        "mpl-2.0" | "mozilla public license 2.0" => "MPL-2.0".to_string(),
        "lgpl-2.1" | "lgplv2.1" | "gnu lesser general public license v2 (lgplv2)" => {
            "LGPL-2.1-only".to_string()
        }
        "lgpl-2.1+" | "lgplv2+" | "lgplv2.1+" => "LGPL-2.1-or-later".to_string(),
        "lgpl-3.0" | "lgplv3" => "LGPL-3.0-only".to_string(),
        "lgpl-3.0+" | "lgplv3+" => "LGPL-3.0-or-later".to_string(),
        "gpl-2.0" | "gplv2" => "GPL-2.0-only".to_string(),
        "gpl-2.0+" | "gplv2+" => "GPL-2.0-or-later".to_string(),
        "gpl-3.0" | "gplv3" => "GPL-3.0-only".to_string(),
        "gpl-3.0+" | "gplv3+" => "GPL-3.0-or-later".to_string(),
        "isc" => "ISC".to_string(),
        "python software foundation license" | "psf" => "PSF-2.0".to_string(),
        "artistic license" | "artistic-2.0" => "Artistic-2.0".to_string(),
        _ => String::new(),
    }
}

fn cleanup_multiline(s: &str) -> String {
    let mut lines = Vec::new();
    for line in s.lines() {
        let cleaned = normalize_description_line(line);
        if !cleaned.is_empty() {
            lines.push(cleaned);
        }
    }
    lines.join("\n")
}

fn normalize_description_line(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with("<!--") || trimmed.ends_with("-->") {
        return String::new();
    }
    if trimmed
        .chars()
        .all(|c| matches!(c, '=' | '-' | '_' | '~' | '`' | '*'))
    {
        return String::new();
    }

    let mut cleaned = trimmed
        .trim_start_matches('#')
        .trim_start_matches('-')
        .trim_start_matches('*')
        .trim()
        .to_string();

    if cleaned.starts_with("http://") || cleaned.starts_with("https://") {
        return String::new();
    }
    if cleaned.starts_with("![") {
        return String::new();
    }

    cleaned = re_md_link().replace_all(&cleaned, "$1").to_string();

    cleaned = re_html_tag().replace_all(&cleaned, " ").to_string();
    cleanup_single_line(&cleaned)
}

fn best_description_paragraph(s: &str, summary: &str) -> String {
    let mut chosen = String::new();
    for paragraph in s.split("\n\n") {
        let cleaned = cleanup_multiline(paragraph);
        if cleaned.is_empty() {
            continue;
        }
        let lower = cleaned.to_ascii_lowercase();
        if lower.starts_with("metadata-version:")
            || lower.starts_with("name:")
            || lower.starts_with("version:")
            || lower.starts_with("summary:")
        {
            continue;
        }
        chosen = shorten_description(&cleaned);
        if chosen.len() < 24 || !chosen.contains(' ') {
            chosen.clear();
            continue;
        }
        break;
    }
    if chosen.is_empty() {
        return shorten_description(summary);
    }
    chosen
}

fn shorten_description(text: &str) -> String {
    let cleaned = cleanup_multiline(text);
    if cleaned.len() <= 280 {
        return cleaned;
    }

    let sentences = split_sentences(&cleaned);

    let mut out = String::new();
    for sentence in sentences {
        let next_len = if out.is_empty() {
            sentence.len()
        } else {
            out.len() + 1 + sentence.len()
        };
        if next_len > 280 {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&sentence);
    }

    if out.is_empty() {
        cleaned.chars().take(280).collect()
    } else {
        out
    }
}

fn split_sentences(text: &str) -> Vec<String> {
    // Manual sentence splitting keeps us compatible with Rust regex crate,
    // which does not support look-behind.
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut chars = text.char_indices().peekable();

    while let Some((idx, ch)) = chars.next() {
        if matches!(ch, '.' | '!' | '?') {
            let should_split = match chars.peek() {
                None => true,
                Some((_, next)) => next.is_whitespace(),
            };
            if should_split {
                let part = text[start..=idx].trim();
                if !part.is_empty() {
                    out.push(part.to_string());
                }
                // Skip subsequent whitespace before next sentence.
                while let Some((next_idx, next)) = chars.peek().copied() {
                    if !next.is_whitespace() {
                        start = next_idx;
                        break;
                    }
                    chars.next();
                    if chars.peek().is_none() {
                        start = text.len();
                    }
                }
            }
        }
    }

    if start < text.len() {
        let tail = text[start..].trim();
        if !tail.is_empty() {
            out.push(tail.to_string());
        }
    }

    if out.is_empty() {
        out.push(text.trim().to_string());
    }
    out
}
