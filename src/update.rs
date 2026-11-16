//! Update checks against the GitHub releases of Patina, and in-place
//! self-update of the Linux build.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

const REPO: &str = "Kurisu003/patina";
const ASSET: &str = "patina-linux-x86_64.tar.gz";
pub const RELEASES_URL: &str = "https://github.com/Kurisu003/patina/releases";

#[derive(Clone, Debug)]
pub struct Release {
    /// Version without the leading "v", e.g. "0.1.8".
    pub version: String,
    pub page: String,
    /// Download URL of the Linux build, if this release has one.
    pub asset: Option<String>,
}

pub enum CheckResult {
    Newer(Release),
    UpToDate,
    Failed(String),
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(60))).build().into()
}

fn user_agent() -> String {
    format!("patina/{}", env!("CARGO_PKG_VERSION"))
}

/// Parse "v1.2.3" / "1.2.3" into comparable numbers.
fn parse_version(v: &str) -> Vec<u64> {
    v.trim().trim_start_matches('v').split(['.', '-', '+']).map_while(|p| p.parse().ok()).collect()
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    parse_version(candidate) > parse_version(current)
}

/// Latest release via the github.com "latest" redirect (no API rate limit).
fn fetch_latest() -> Result<Release, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(30)))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();
    let resp = agent.get(&format!("https://github.com/{REPO}/releases/latest")).header("User-Agent", &user_agent()).call().map_err(|e| e.to_string())?;
    let loc = resp.headers().get("location").and_then(|v| v.to_str().ok()).ok_or_else(|| format!("unexpected answer from GitHub ({})", resp.status()))?;
    let tag = loc.rsplit('/').next().filter(|t| !t.is_empty() && *t != "releases").ok_or("no release published yet")?;
    let version = tag.trim_start_matches('v').to_string();
    if parse_version(&version).is_empty() {
        return Err(format!("unexpected release tag {tag}"));
    }
    let asset_url = format!("https://github.com/{REPO}/releases/download/{tag}/{ASSET}");
    // is the Linux build attached (yet)?
    let has_asset = agent.head(&asset_url).header("User-Agent", &user_agent()).call().map(|r| r.status().is_redirection() || r.status().is_success()).unwrap_or(false);
    Ok(Release { version, page: format!("https://github.com/{REPO}/releases/tag/{tag}"), asset: has_asset.then_some(asset_url) })
}

/// The running version (PATINA_UPDATE_TEST_VERSION overrides it for testing).
pub fn current_version() -> String {
    std::env::var("PATINA_UPDATE_TEST_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string())
}

/// Check for a newer release on a background thread.
pub fn check_async() -> Receiver<CheckResult> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let r = match fetch_latest() {
            Ok(rel) if is_newer(&rel.version, &current_version()) => CheckResult::Newer(rel),
            Ok(_) => CheckResult::UpToDate,
            Err(e) => CheckResult::Failed(e),
        };
        let _ = tx.send(r);
    });
    rx
}

/// The real path of the running program (symlinks resolved).
pub fn exe_path() -> Result<PathBuf, String> {
    let p = std::env::current_exe().map_err(|e| e.to_string())?;
    std::fs::canonicalize(&p).map_err(|e| e.to_string())
}

/// Whether this build can replace itself (Linux x86_64 only).
pub fn can_self_update() -> bool {
    cfg!(all(target_os = "linux", target_arch = "x86_64"))
}

/// Download the release archive and replace the running executable.
/// Returns the path that was replaced.
fn install(url: &str) -> Result<PathBuf, String> {
    let exe = exe_path()?;
    let dir = exe.parent().ok_or("no install directory")?;
    let mut resp = agent().get(url).header("User-Agent", &user_agent()).call().map_err(|e| format!("download failed: {e}"))?;
    let mut archive = Vec::new();
    resp.body_mut().as_reader().read_to_end(&mut archive).map_err(|e| format!("download failed: {e}"))?;
    let binary = extract_binary(&archive)?;
    if binary.len() < 1_000_000 || &binary[..4] != b"\x7fELF" {
        return Err("the downloaded file is not a Linux program".into());
    }
    // write next to the old binary, then atomically rename over it
    let tmp = dir.join(format!(".patina-update-{}", std::process::id()));
    std::fs::write(&tmp, &binary).map_err(|e| permission_hint(dir, e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
    }
    if let Err(e) = std::fs::rename(&tmp, &exe) {
        let _ = std::fs::remove_file(&tmp);
        return Err(permission_hint(dir, e));
    }
    Ok(exe)
}

fn permission_hint(dir: &Path, e: std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        format!("no permission to write to {} (download the new version from the releases page instead)", dir.display())
    } else {
        e.to_string()
    }
}

fn extract_binary(archive: &[u8]) -> Result<Vec<u8>, String> {
    let gz = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(gz);
    for entry in tar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.to_path_buf();
        if path.file_name().is_some_and(|n| n == "patina") && entry.header().entry_type().is_file() {
            let mut data = Vec::new();
            entry.read_to_end(&mut data).map_err(|e| e.to_string())?;
            return Ok(data);
        }
    }
    Err("the archive does not contain the patina program".into())
}

/// Run `install` on a background thread.
pub fn install_async(url: String) -> Receiver<Result<PathBuf, String>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(install(&url));
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(is_newer("v0.1.8", "0.1.7"));
        assert!(is_newer("0.2.0", "0.1.17"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("v0.1.7", "0.1.7"));
        assert!(!is_newer("0.1.6", "0.1.7"));
        assert!(is_newer("0.1.10", "0.1.9"));
    }
}
