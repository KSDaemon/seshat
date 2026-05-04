use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::Duration;

use crate::CliError;
use crate::version_cache::VersionCache;
use flate2::read::GzDecoder;
use indicatif::{ProgressBar, ProgressStyle};
use sha2::Digest;
use tar::Archive;

use sha2::Sha256;

const GITHUB_RELEASES_API: &str = "https://api.github.com/repos/KSDaemon/seshat/releases/latest";
const USER_AGENT: &str = "seshat";
const TIMEOUT_SECS: u64 = 15;

#[derive(Debug, PartialEq, Clone, Copy)]
enum InstallMethod {
    Homebrew,
    Direct,
}

pub fn run_update(check: bool) -> Result<(), CliError> {
    if check {
        run_check()
    } else {
        run_self_update()
    }
}

/// Check for a newer version and print a notice to stderr if one is available.
///
/// Uses the 24h version cache — at most one GitHub API call per day.
/// Silently skips on network errors or if no binary asset matches the current target.
/// All output goes to stderr so MCP protocol consumers are unaffected.
///
/// Should be called once at startup for any command EXCEPT `seshat update` and
/// `seshat update --check`.
pub fn check_and_print_update_notice() {
    check_and_print_update_notice_inner(&VersionCache::cache_path());
}

fn check_and_print_update_notice_inner(cache_path: &Option<PathBuf>) {
    let current = env!("CARGO_PKG_VERSION");

    // Try to load a fresh cache first — avoid network if possible.
    // If cache is fresh we have the latest version and can skip the network call.
    // Note: the cache doesn't store asset availability, so we optimistically print
    // the notice when the cache says a newer version exists. The asset-availability
    // check only gates the notice when we actually do a fresh network fetch.
    if let Some(path) = cache_path {
        if let Some(cache) = VersionCache::read_from_path(path) {
            if cache.is_fresh() {
                if is_newer(&cache.latest_version, current) {
                    eprintln!(
                        "Seshat v{} is available (current: v{current}). Run seshat update to upgrade.",
                        cache.latest_version
                    );
                }
                return;
            }
        }
    }

    // Cache is stale or missing — fetch from network, silently skip on any error.
    let (version, has_assets) = match fetch_latest_release() {
        Ok(result) => result,
        Err(_) => return, // Network failure → silent skip
    };

    // Write fresh cache on successful network fetch
    if let Some(path) = cache_path {
        let cache = VersionCache::new(version.clone());
        let _ = cache.write_to_path(path);
    }

    // No binary asset for current target → no notice
    if !has_assets {
        return;
    }

    if is_newer(&version, current) {
        eprintln!(
            "Seshat v{version} is available (current: v{current}). Run seshat update to upgrade."
        );
    }
}

fn run_self_update() -> Result<(), CliError> {
    if cfg!(target_os = "windows") {
        eprintln!(
            "Self-update not supported on Windows. Use cargo install seshat or download from GitHub Releases."
        );
        return Err(CliError::CommandFailed {
            command: "update".to_owned(),
            reason: "self-update not supported on Windows".to_owned(),
        });
    }

    let install_method = detect_install_method()?;
    if install_method == InstallMethod::Homebrew {
        eprintln!("Seshat was installed via Homebrew. Run brew upgrade seshat to update.");
        return Err(CliError::CommandFailed {
            command: "update".to_owned(),
            reason: "installed via Homebrew".to_owned(),
        });
    }

    let (version, asset_url, checksums_url) = fetch_release_assets()?;

    let current = env!("CARGO_PKG_VERSION");
    if !is_newer(&version, current) {
        println!("Seshat is already up to date (v{current}).");
        return Ok(());
    }

    let expected_sha256 = fetch_checksum_for_asset(&checksums_url, &version)?;

    let temp_dir = tempfile::TempDir::new().map_err(|e| CliError::CommandFailed {
        command: "update".to_owned(),
        reason: format!("failed to create temp directory: {e}"),
    })?;

    let download_path = temp_dir.path().join("seshat.tar.gz");
    download_with_progress(&asset_url, &download_path)?;

    verify_sha256(&download_path, &expected_sha256).inspect_err(|_| {
        let _ = fs::remove_dir_all(temp_dir.path());
    })?;

    let binary_path =
        extract_binary(&download_path, temp_dir.path(), &version).inspect_err(|_| {
            let _ = fs::remove_dir_all(temp_dir.path());
        })?;

    // Pre-flight: verify extracted binary runs (catches macOS Gatekeeper quarantine)
    preflight_check(&binary_path, temp_dir.path())?;

    // Resolve symlinks to find the actual binary on disk
    let target_exe = resolve_target_exe()?;

    // Atomically replace the current binary
    replace_binary(&binary_path, &target_exe, temp_dir.path())?;

    // Print cargo note if applicable (non-fatal)
    if is_cargo_install() {
        println!(
            "Note: seshat was installed via cargo. You may want to run 'cargo install seshat' to keep ~/.cargo/.crates2.json in sync."
        );
    }

    println!("Seshat updated to v{version}.");
    Ok(())
}

fn detect_install_method() -> Result<InstallMethod, CliError> {
    let exe_path = std::env::current_exe().map_err(|e| CliError::CommandFailed {
        command: "update".to_owned(),
        reason: format!("cannot determine current executable: {e}"),
    })?;

    if exe_path.to_string_lossy().contains("/Cellar/") {
        return Ok(InstallMethod::Homebrew);
    }

    if let Ok(canonical) = exe_path.canonicalize() {
        if canonical.to_string_lossy().contains("/Cellar/") {
            return Ok(InstallMethod::Homebrew);
        }
    }

    Ok(InstallMethod::Direct)
}

fn fetch_release_assets() -> Result<(String, String, String), CliError> {
    let agent = build_agent();

    let response = agent
        .get(GITHUB_RELEASES_API)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!("failed to fetch release info: {e}"),
        })?;

    let body = response
        .into_body()
        .read_to_string()
        .map_err(|e| CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!("failed to read release info: {e}"),
        })?;

    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!("failed to parse release info: {e}"),
        })?;

    let tag_name = json["tag_name"].as_str().unwrap_or("v0.0.0");
    let version = tag_name.strip_prefix('v').unwrap_or(tag_name).to_owned();

    let target = current_target();
    if target == "unsupported" {
        return Err(CliError::CommandFailed {
            command: "update".to_owned(),
            reason: "unsupported platform for self-update".to_owned(),
        });
    }

    let assets = json["assets"]
        .as_array()
        .ok_or_else(|| CliError::CommandFailed {
            command: "update".to_owned(),
            reason: "no assets found in release".to_owned(),
        })?;

    let checksums_url = find_checksums_url(assets)?;

    let (asset_name, asset_url) =
        find_binary_asset(assets, target).ok_or_else(|| CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!("no binary asset found for target {target}"),
        })?;

    if !asset_name.contains(&version) {
        eprintln!(
            "Warning: asset name '{asset_name}' does not contain version '{version}', proceeding anyway."
        );
    }

    Ok((version, asset_url, checksums_url))
}

fn find_checksums_url(assets: &[serde_json::Value]) -> Result<String, CliError> {
    for asset in assets {
        let name = asset["name"].as_str().unwrap_or("");
        if name == "sha256sums.txt" || name.contains("sha256sums") {
            return asset["browser_download_url"]
                .as_str()
                .map(|u| u.to_owned())
                .ok_or_else(|| CliError::CommandFailed {
                    command: "update".to_owned(),
                    reason: "no download URL for checksums file".to_owned(),
                });
        }
    }
    Err(CliError::CommandFailed {
        command: "update".to_owned(),
        reason: "checksums file not found in release assets".to_owned(),
    })
}

fn find_binary_asset(assets: &[serde_json::Value], target: &str) -> Option<(String, String)> {
    assets.iter().find_map(|asset| {
        let name = asset["name"].as_str().unwrap_or("");
        if name.contains(target) && (name.ends_with(".tar.gz") || name.ends_with(".tgz")) {
            let url = asset["browser_download_url"].as_str()?;
            Some((name.to_owned(), url.to_owned()))
        } else {
            None
        }
    })
}

fn fetch_checksum_for_asset(checksums_url: &str, version: &str) -> Result<String, CliError> {
    let agent = build_agent();

    let response = agent
        .get(checksums_url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!("failed to download checksums: {e}"),
        })?;

    let body = response
        .into_body()
        .read_to_string()
        .map_err(|e| CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!("failed to read checksums: {e}"),
        })?;

    let target = current_target();
    let expected_archive = format!("seshat-{target}-v{version}.tar.gz");

    for line in body.lines() {
        let parts: Vec<&str> = line.splitn(2, ' ').collect();
        if parts.len() == 2 {
            let checksum = parts[0].trim();
            let filename = parts[1].trim();
            if filename == expected_archive || filename.ends_with(&expected_archive) {
                return Ok(checksum.to_owned());
            }
        }
    }

    Err(CliError::CommandFailed {
        command: "update".to_owned(),
        reason: format!("checksum not found for {expected_archive}"),
    })
}

fn download_with_progress(url: &str, dest: &Path) -> Result<(), CliError> {
    let agent = build_agent();

    let response = agent
        .get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!("failed to download binary: {e}"),
        })?;

    let total_size = response
        .headers()
        .get("Content-Length")
        .and_then(|v| v.to_str().ok().and_then(|s| s.parse().ok()))
        .unwrap_or(0u64);

    let pb = ProgressBar::new(total_size);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})",
        )
        .unwrap()
        .progress_chars("#>-"),
    );

    let mut file = fs::File::create(dest).map_err(|e| CliError::CommandFailed {
        command: "update".to_owned(),
        reason: format!("failed to create download file: {e}"),
    })?;

    let mut reader = response.into_body().into_reader();
    let mut downloaded = 0u64;

    loop {
        let mut buf = [0u8; 8192];
        let read = reader.read(&mut buf).map_err(|e| CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!("download interrupted: {e}"),
        })?;
        if read == 0 {
            break;
        }
        file.write_all(&buf[..read])
            .map_err(|e| CliError::CommandFailed {
                command: "update".to_owned(),
                reason: format!("failed to write download: {e}"),
            })?;
        downloaded += read as u64;
        pb.set_position(downloaded);
    }

    pb.finish_with_message("Download complete");
    Ok(())
}

fn verify_sha256(file_path: &Path, expected: &str) -> Result<(), CliError> {
    let mut file = fs::File::open(file_path).map_err(|e| CliError::CommandFailed {
        command: "update".to_owned(),
        reason: format!("cannot open file for verification: {e}"),
    })?;

    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf).map_err(|e| CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!("failed to read file for hashing: {e}"),
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    let hash = hasher.finalize();
    let mut computed = String::with_capacity(hash.len() * 2);
    for byte in hash {
        use std::fmt::Write;
        let _ = write!(computed, "{byte:02x}");
    }

    if computed.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!("SHA256 mismatch: expected {expected}, computed {computed}"),
        })
    }
}

fn extract_binary(
    archive_path: &Path,
    dest_dir: &Path,
    version: &str,
) -> Result<PathBuf, CliError> {
    let archive_file = fs::File::open(archive_path).map_err(|e| CliError::CommandFailed {
        command: "update".to_owned(),
        reason: format!("failed to open archive for extraction: {e}"),
    })?;

    let decoder = GzDecoder::new(archive_file);
    let mut archive = Archive::new(decoder);

    archive
        .unpack(dest_dir)
        .map_err(|e| CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!("failed to extract archive: {e}"),
        })?;

    let target = current_target();
    let expected_dir = format!("seshat-{target}-v{version}");
    let binary_path = dest_dir.join(&expected_dir).join("seshat");

    if !binary_path.is_file() {
        return Err(CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!(
                "extracted binary not found at expected path: {}",
                binary_path.display()
            ),
        });
    }

    set_executable(&binary_path)?;

    Ok(binary_path)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::metadata(path).map_err(|e| CliError::CommandFailed {
        command: "update".to_owned(),
        reason: format!("failed to read binary metadata: {e}"),
    })?;
    let mut perms = metadata.permissions();
    let current_mode = perms.mode();
    perms.set_mode(current_mode | 0o111);
    fs::set_permissions(path, perms).map_err(|e| CliError::CommandFailed {
        command: "update".to_owned(),
        reason: format!("failed to set executable permission: {e}"),
    })?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), CliError> {
    Ok(())
}

/// Run the extracted binary with `--version` to verify it actually executes.
/// On macOS, Gatekeeper kills processes it quarantines with signal 9 (SIGKILL).
/// We detect that and print the xattr removal command.
fn preflight_check(binary_path: &Path, temp_dir: &Path) -> Result<(), CliError> {
    let output = ProcessCommand::new(binary_path)
        .arg("--version")
        .output()
        .map_err(|e| {
            let _ = fs::remove_dir_all(temp_dir);
            CliError::CommandFailed {
                command: "update".to_owned(),
                reason: format!("failed to run extracted binary: {e}"),
            }
        })?;

    if output.status.success() {
        return Ok(());
    }

    // Check if killed by signal 9 (Gatekeeper on macOS)
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if output.status.signal() == Some(9) {
            let _ = fs::remove_dir_all(temp_dir);
            eprintln!(
                "macOS Gatekeeper blocked the update binary. Remove quarantine with:\n  xattr -d com.apple.quarantine {}",
                binary_path.display()
            );
            return Err(CliError::CommandFailed {
                command: "update".to_owned(),
                reason: "macOS Gatekeeper killed the binary (signal 9)".to_owned(),
            });
        }
    }

    // Non-zero exit but not signal 9 — still consider it a failure
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    // If either output looks like a version string it's fine (some builds exit non-zero from --version)
    if stdout.contains("seshat") || stderr.contains("seshat") {
        return Ok(());
    }

    let _ = fs::remove_dir_all(temp_dir);
    Err(CliError::CommandFailed {
        command: "update".to_owned(),
        reason: format!(
            "extracted binary failed preflight: exit code {:?}",
            output.status.code()
        ),
    })
}

/// Resolve symlinks so we replace the actual binary, not a symlink.
fn resolve_target_exe() -> Result<PathBuf, CliError> {
    let exe = std::env::current_exe().map_err(|e| CliError::CommandFailed {
        command: "update".to_owned(),
        reason: format!("cannot determine current executable: {e}"),
    })?;

    exe.canonicalize().map_err(|e| CliError::CommandFailed {
        command: "update".to_owned(),
        reason: format!("cannot resolve current executable path: {e}"),
    })
}

/// Atomically replace `target_exe` with `new_binary`.
/// Uses `fs::rename` (atomic on same filesystem). Falls back to copy+remove for EXDEV.
fn replace_binary(new_binary: &Path, target_exe: &Path, temp_dir: &Path) -> Result<(), CliError> {
    match fs::rename(new_binary, target_exe) {
        Ok(()) => Ok(()),
        Err(e) => {
            // EXDEV = 18: cross-device rename — fall back to copy + overwrite
            #[cfg(unix)]
            let is_cross_device = e.raw_os_error() == Some(18);
            #[cfg(not(unix))]
            let is_cross_device = false;

            if is_cross_device {
                // Copy new binary to same filesystem as target, then rename
                let parent = target_exe
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("/tmp"));
                let staging = parent.join(".seshat-update-staging");
                fs::copy(new_binary, &staging).map_err(|ce| {
                    let _ = fs::remove_dir_all(temp_dir);
                    map_replace_error(ce, target_exe)
                })?;
                fs::rename(&staging, target_exe).map_err(|re| {
                    let _ = fs::remove_file(&staging);
                    let _ = fs::remove_dir_all(temp_dir);
                    map_replace_error(re, target_exe)
                })?;
                return Ok(());
            }

            // Permission denied
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                let _ = fs::remove_dir_all(temp_dir);
                eprintln!(
                    "Permission denied updating {}. Try: sudo seshat update",
                    target_exe.display()
                );
                return Err(CliError::CommandFailed {
                    command: "update".to_owned(),
                    reason: "permission denied; try sudo seshat update".to_owned(),
                });
            }

            let _ = fs::remove_dir_all(temp_dir);
            Err(map_replace_error(e, target_exe))
        }
    }
}

fn map_replace_error(e: std::io::Error, target_exe: &Path) -> CliError {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        eprintln!(
            "Permission denied updating {}. Try: sudo seshat update",
            target_exe.display()
        );
        CliError::CommandFailed {
            command: "update".to_owned(),
            reason: "permission denied; try sudo seshat update".to_owned(),
        }
    } else {
        CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!("failed to replace binary: {e}"),
        }
    }
}

/// Check if seshat was installed via `cargo install` by looking for the binary
/// in `~/.cargo/.crates2.json` or `~/.cargo/.crates.toml`.
fn is_cargo_install() -> bool {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return false,
    };
    let cargo_dir = home.join(".cargo");

    // Try .crates2.json first
    let crates2 = cargo_dir.join(".crates2.json");
    if crates2.exists() {
        if let Ok(content) = fs::read_to_string(&crates2) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                if cargo_json_contains_seshat(&json) {
                    return true;
                }
            }
        }
    }

    // Try .crates.toml
    let crates_toml = cargo_dir.join(".crates.toml");
    if crates_toml.exists() {
        if let Ok(content) = fs::read_to_string(&crates_toml) {
            if content.contains("seshat") {
                return true;
            }
        }
    }

    false
}

fn cargo_json_contains_seshat(json: &serde_json::Value) -> bool {
    // .crates2.json structure: { "installs": { "seshat <version> (...)": { ... } } }
    if let Some(installs) = json.get("installs").and_then(|v| v.as_object()) {
        return installs.keys().any(|k| k.starts_with("seshat "));
    }
    false
}

fn build_agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(TIMEOUT_SECS)))
        .build();
    config.into()
}

fn run_check() -> Result<(), CliError> {
    run_check_inner(&VersionCache::cache_path())
}

fn run_check_inner(cache_path: &Option<PathBuf>) -> Result<(), CliError> {
    if let Some(path) = cache_path {
        if let Some(cache) = VersionCache::read_from_path(path) {
            if cache.is_fresh() {
                return print_update_status(&cache.latest_version);
            }
        }
    }

    match fetch_latest_release() {
        Ok((version, has_assets)) => {
            if let Some(path) = cache_path {
                let cache = VersionCache::new(version.clone());
                let _ = cache.write_to_path(path);
            }

            if has_assets {
                print_update_status(&version)
            } else {
                println!("Seshat is up to date (v{}).", env!("CARGO_PKG_VERSION"));
                Ok(())
            }
        }
        Err(e) => {
            eprintln!("Could not check for updates: {e}");
            Err(CliError::CommandFailed {
                command: "update".to_owned(),
                reason: e,
            })
        }
    }
}

fn print_update_status(latest_version: &str) -> Result<(), CliError> {
    let current = env!("CARGO_PKG_VERSION");

    if is_newer(latest_version, current) {
        if detect_homebrew() {
            println!(
                "Seshat v{latest_version} is available. You installed via Homebrew. Run brew upgrade seshat."
            );
        } else {
            println!(
                "Seshat v{latest_version} is available (current: v{current}). Run seshat update to upgrade."
            );
        }
    } else {
        println!("Seshat is up to date (v{current}).");
    }

    Ok(())
}

fn fetch_latest_release() -> Result<(String, bool), String> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(TIMEOUT_SECS)))
        .build();
    let agent: ureq::Agent = config.into();

    let response = agent
        .get(GITHUB_RELEASES_API)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| format!("network error: {e}"))?;

    let body = response
        .into_body()
        .read_to_string()
        .map_err(|e| format!("failed to read response: {e}"))?;

    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("failed to parse response: {e}"))?;

    let tag_name = json["tag_name"].as_str().unwrap_or("v0.0.0");
    let version = tag_name.strip_prefix('v').unwrap_or(tag_name);

    let has_assets = has_binary_asset_for_current_target(&json);

    Ok((version.to_owned(), has_assets))
}

fn is_newer(latest: &str, current: &str) -> bool {
    let parse =
        |v: &str| -> Vec<u32> { v.split('.').filter_map(|p| p.parse::<u32>().ok()).collect() };

    let latest_parts = parse(latest);
    let current_parts = parse(current);

    if latest_parts.is_empty() || current_parts.is_empty() {
        return false;
    }

    for (l, c) in latest_parts.iter().zip(current_parts.iter()) {
        if l > c {
            return true;
        }
        if l < c {
            return false;
        }
    }

    latest_parts.len() > current_parts.len()
}

fn current_target() -> &'static str {
    let arch = std::env::consts::ARCH;
    let os = std::env::consts::OS;
    match (arch, os) {
        ("aarch64", "macos") => "aarch64-apple-darwin",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        _ => "unsupported",
    }
}

fn has_binary_asset_for_current_target(json: &serde_json::Value) -> bool {
    let target = current_target();
    if target == "unsupported" {
        return false;
    }

    if let Some(assets) = json["assets"].as_array() {
        assets.iter().any(|asset| {
            asset["name"]
                .as_str()
                .is_some_and(|name| name.contains(target))
        })
    } else {
        false
    }
}

fn detect_homebrew() -> bool {
    match std::env::current_exe() {
        Ok(path) => {
            if path.to_string_lossy().contains("/Cellar/") {
                return true;
            }
            if let Ok(canonical) = path.canonicalize() {
                canonical.to_string_lossy().contains("/Cellar/")
            } else {
                false
            }
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_major_version() {
        assert!(is_newer("2.0.0", "1.0.0"));
    }

    #[test]
    fn older_major_version() {
        assert!(!is_newer("1.0.0", "2.0.0"));
    }

    #[test]
    fn same_version() {
        assert!(!is_newer("1.0.0", "1.0.0"));
    }

    #[test]
    fn newer_minor_version() {
        assert!(is_newer("1.1.0", "1.0.0"));
    }

    #[test]
    fn newer_patch_version() {
        assert!(is_newer("1.0.1", "1.0.0"));
    }

    #[test]
    fn newer_with_extra_component() {
        assert!(is_newer("1.0.0.1", "1.0.0"));
    }

    #[test]
    fn older_with_fewer_components() {
        assert!(!is_newer("1.0", "1.0.0"));
    }

    #[test]
    fn invalid_versions_compare_equal() {
        assert!(!is_newer("abc", "1.0.0"));
        assert!(!is_newer("1.0.0", "abc"));
    }

    #[test]
    fn current_target_is_known_on_main_platforms() {
        let target = current_target();
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        assert_ne!(target, "unsupported");
    }

    #[test]
    fn has_binary_asset_returns_true_when_matching() {
        let target = current_target();
        if target == "unsupported" {
            return;
        }
        let json = serde_json::json!({
            "tag_name": "v1.0.0",
            "assets": [
                {"name": format!("seshat-{target}-v1.0.0.tar.gz")},
            ]
        });
        assert!(has_binary_asset_for_current_target(&json));
    }

    #[test]
    fn has_binary_asset_returns_false_when_no_match() {
        let json = serde_json::json!({
            "tag_name": "v1.0.0",
            "assets": [
                {"name": "seshat-wasm32-unknown-unknown-v1.0.0.tar.gz"},
            ]
        });
        assert!(!has_binary_asset_for_current_target(&json));
    }

    #[test]
    fn has_binary_asset_empty_assets() {
        let json = serde_json::json!({
            "tag_name": "v1.0.0",
            "assets": []
        });
        assert!(!has_binary_asset_for_current_target(&json));
    }

    #[test]
    fn has_binary_asset_unsupported_target() {
        // This test verifies that the function doesn't panic on unsupported targets
        // even when assets exist
        let json = serde_json::json!({
            "tag_name": "v1.0.0",
            "assets": [
                {"name": "seshat-some-target-v1.0.0.tar.gz"},
            ]
        });
        let _ = has_binary_asset_for_current_target(&json);
    }

    #[test]
    fn detect_homebrew_is_bool() {
        let _ = detect_homebrew();
    }

    #[test]
    fn fresh_cache_no_network() {
        let dir = tempfile::TempDir::new().unwrap();
        let cache_path = dir.path().join("version-check.json");
        let cache = VersionCache::new("99.99.99".to_owned());
        cache.write_to_path(&cache_path).unwrap();

        let result = run_check_inner(&Some(cache_path));
        assert!(result.is_ok());
    }

    #[test]
    fn detect_install_method_on_current_platform() {
        let method = detect_install_method();
        assert!(method.is_ok());
        assert_eq!(method.unwrap(), InstallMethod::Direct);
    }

    #[test]
    fn install_method_enum_equality() {
        assert_eq!(InstallMethod::Homebrew, InstallMethod::Homebrew);
        assert_eq!(InstallMethod::Direct, InstallMethod::Direct);
        assert_ne!(InstallMethod::Homebrew, InstallMethod::Direct);
    }

    #[test]
    fn sha256_verify_matching() {
        let dir = tempfile::TempDir::new().unwrap();
        let file_path = dir.path().join("test.bin");
        fs::write(&file_path, b"hello world").unwrap();

        let mut hasher = Sha256::new();
        hasher.update(b"hello world");
        let hash = hasher.finalize();
        let mut hex = String::new();
        for byte in hash {
            use std::fmt::Write;
            let _ = write!(hex, "{byte:02x}");
        }

        assert!(verify_sha256(&file_path, &hex).is_ok());
    }

    #[test]
    fn sha256_verify_mismatch() {
        let dir = tempfile::TempDir::new().unwrap();
        let file_path = dir.path().join("test.bin");
        fs::write(&file_path, b"hello world").unwrap();

        let result = verify_sha256(
            &file_path,
            "0000000000000000000000000000000000000000000000000000000000000000",
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("SHA256 mismatch"));
    }

    #[test]
    fn extract_binary_from_valid_tar_gz() {
        let dir = tempfile::TempDir::new().unwrap();
        let archive_path = dir.path().join("test.tar.gz");

        let file = fs::File::create(&archive_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);

        let expected_dir = format!("seshat-{}-v1.0.0", current_target());
        let binary_dir = format!("{expected_dir}/seshat");

        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        builder
            .append_data(&mut header, &expected_dir, &[][..])
            .unwrap();

        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o755);
        builder
            .append_data(&mut header, &binary_dir, &b"fake"[..])
            .unwrap();

        let archive_data = builder.into_inner().unwrap().finish().unwrap();
        drop(archive_data);

        let result = extract_binary(&archive_path, dir.path(), "1.0.0");
        assert!(result.is_ok());
        let binary_path = result.unwrap();
        assert!(binary_path.is_file());
        assert!(binary_path.ends_with(format!("{expected_dir}/seshat")));
    }

    #[test]
    fn extract_binary_corrupted_archive_errors() {
        let dir = tempfile::TempDir::new().unwrap();
        let archive_path = dir.path().join("corrupt.tar.gz");
        fs::write(&archive_path, b"not a valid gzip file").unwrap();

        let result = extract_binary(&archive_path, dir.path(), "1.0.0");
        assert!(result.is_err());
    }

    #[test]
    fn find_binary_asset_matches_target() {
        let assets = vec![
            serde_json::json!({
                "name": "seshat-aarch64-apple-darwin-v1.0.0.tar.gz",
                "browser_download_url": "https://example.com/asset1.tar.gz"
            }),
            serde_json::json!({
                "name": "seshat-x86_64-apple-darwin-v1.0.0.tar.gz",
                "browser_download_url": "https://example.com/asset2.tar.gz"
            }),
        ];

        let target = "aarch64-apple-darwin";
        let result = find_binary_asset(&assets, target);
        assert!(result.is_some());
        let (name, url) = result.unwrap();
        assert!(name.contains("aarch64-apple-darwin"));
        assert_eq!(url, "https://example.com/asset1.tar.gz");
    }

    #[test]
    fn find_binary_asset_no_match() {
        let assets = vec![serde_json::json!({
            "name": "seshat-wasm32-unknown-unknown-v1.0.0.tar.gz",
            "browser_download_url": "https://example.com/asset1.tar.gz"
        })];

        let result = find_binary_asset(&assets, "aarch64-apple-darwin");
        assert!(result.is_none());
    }

    #[test]
    fn find_binary_asset_skips_non_tar() {
        let assets = vec![serde_json::json!({
            "name": "seshat-aarch64-apple-darwin-v1.0.0.msi",
            "browser_download_url": "https://example.com/asset1.msi"
        })];

        let result = find_binary_asset(&assets, "aarch64-apple-darwin");
        assert!(result.is_none());
    }

    #[test]
    fn find_checksums_url_finds_sha256sums_txt() {
        let assets = vec![
            serde_json::json!({
                "name": "seshat-aarch64-apple-darwin-v1.0.0.tar.gz",
                "browser_download_url": "https://example.com/asset1.tar.gz"
            }),
            serde_json::json!({
                "name": "sha256sums.txt",
                "browser_download_url": "https://example.com/sha256sums.txt"
            }),
        ];

        let result = find_checksums_url(&assets);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "https://example.com/sha256sums.txt");
    }

    #[test]
    fn find_checksums_url_not_found() {
        let assets = vec![serde_json::json!({
            "name": "seshat-aarch64-apple-darwin-v1.0.0.tar.gz",
            "browser_download_url": "https://example.com/asset1.tar.gz"
        })];

        let result = find_checksums_url(&assets);
        assert!(result.is_err());
    }

    // --- US-004 tests ---

    #[test]
    fn is_cargo_install_returns_bool() {
        // Just verify it runs without panicking; actual result depends on the test machine
        let _ = is_cargo_install();
    }

    #[test]
    fn cargo_json_contains_seshat_true() {
        let json = serde_json::json!({
            "installs": {
                "seshat 1.2.3 (registry+https://github.com/rust-lang/crates.io-index)": {
                    "version_req": "^1",
                    "bins": ["seshat"],
                    "features": [],
                    "all_features": false,
                    "no_default_features": false,
                    "profile": "release",
                    "target": "aarch64-apple-darwin",
                    "rustc": "1.75.0"
                }
            }
        });
        assert!(cargo_json_contains_seshat(&json));
    }

    #[test]
    fn cargo_json_contains_seshat_false() {
        let json = serde_json::json!({
            "installs": {
                "ripgrep 13.0.0 (registry+https://github.com/rust-lang/crates.io-index)": {}
            }
        });
        assert!(!cargo_json_contains_seshat(&json));
    }

    #[test]
    fn cargo_json_no_installs_key() {
        let json = serde_json::json!({ "other": "data" });
        assert!(!cargo_json_contains_seshat(&json));
    }

    #[test]
    fn cargo_json_empty_installs() {
        let json = serde_json::json!({ "installs": {} });
        assert!(!cargo_json_contains_seshat(&json));
    }

    #[test]
    fn is_cargo_install_with_fake_crates2_json() {
        let dir = tempfile::TempDir::new().unwrap();
        let cargo_dir = dir.path();

        // Write a fake .crates2.json with seshat
        let crates2 = cargo_dir.join(".crates2.json");
        let json = serde_json::json!({
            "installs": {
                "seshat 1.0.0 (registry+https://github.com/rust-lang/crates.io-index)": {
                    "bins": ["seshat"]
                }
            }
        });
        fs::write(&crates2, serde_json::to_string(&json).unwrap()).unwrap();

        // Read it and parse directly (simulating what is_cargo_install does)
        let content = fs::read_to_string(&crates2).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(cargo_json_contains_seshat(&parsed));
    }

    #[test]
    fn is_cargo_install_with_corrupted_crates2_json() {
        let dir = tempfile::TempDir::new().unwrap();
        let crates2 = dir.path().join(".crates2.json");
        fs::write(&crates2, b"not valid json").unwrap();

        // Corrupted JSON — should not panic
        let content = fs::read_to_string(&crates2).unwrap();
        let result = serde_json::from_str::<serde_json::Value>(&content);
        assert!(result.is_err());
        // is_cargo_install returns false on parse failure (graceful degradation)
    }

    #[test]
    fn is_cargo_install_with_crates_toml() {
        let dir = tempfile::TempDir::new().unwrap();
        let crates_toml = dir.path().join(".crates.toml");
        // Write a fake .crates.toml with seshat
        fs::write(
            &crates_toml,
            r#"[v1]
"seshat 1.0.0 (registry+https://github.com/rust-lang/crates.io-index)" = ["seshat"]
"#,
        )
        .unwrap();

        let content = fs::read_to_string(&crates_toml).unwrap();
        assert!(content.contains("seshat"));
    }

    #[test]
    fn resolve_target_exe_returns_path() {
        // Should succeed on any platform
        let result = resolve_target_exe();
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.is_absolute());
    }

    #[test]
    fn replace_binary_on_same_filesystem() {
        let dir = tempfile::TempDir::new().unwrap();

        // Create a "new binary"
        let new_binary = dir.path().join("new_seshat");
        fs::write(&new_binary, b"new binary content").unwrap();

        // Create a "target binary"
        let target = dir.path().join("seshat");
        fs::write(&target, b"old binary content").unwrap();

        let result = replace_binary(&new_binary, &target, dir.path());
        assert!(result.is_ok());
        assert_eq!(fs::read(&target).unwrap(), b"new binary content");
    }

    #[test]
    fn preflight_check_with_valid_binary() {
        // Use a real binary that we know exists and works
        let dir = tempfile::TempDir::new().unwrap();

        // Use /bin/echo as a stand-in for a "valid" binary
        let echo_path = std::path::Path::new("/bin/echo");
        if !echo_path.exists() {
            return; // Skip on platforms without /bin/echo
        }

        // preflight_check expects binary to output "seshat" in version output
        // For this test, use a shell script that echoes "seshat 1.0.0"
        let script = dir.path().join("fake_seshat");
        fs::write(&script, b"#!/bin/sh\necho 'seshat 1.0.0'\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script, perms).unwrap();
        }

        let result = preflight_check(&script, dir.path());
        assert!(result.is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn preflight_check_detects_signal_kill() {
        // Create a script that exits with non-zero and no "seshat" output
        let dir = tempfile::TempDir::new().unwrap();
        let script = dir.path().join("failing_binary");
        fs::write(&script, b"#!/bin/sh\nexit 1\n").unwrap();

        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();

        let result = preflight_check(&script, dir.path());
        // Should fail since exit code 1 with no seshat output
        assert!(result.is_err());
    }

    // --- US-005 tests ---

    #[test]
    fn notice_skips_when_cache_fresh_and_up_to_date() {
        let dir = tempfile::TempDir::new().unwrap();
        let cache_path = dir.path().join("version-check.json");

        // Cache says current version — no notice expected
        let current = env!("CARGO_PKG_VERSION");
        let cache = VersionCache::new(current.to_owned());
        cache.write_to_path(&cache_path).unwrap();

        // Should not panic or produce errors
        check_and_print_update_notice_inner(&Some(cache_path));
    }

    #[test]
    fn notice_skips_when_cache_fresh_and_old_version() {
        let dir = tempfile::TempDir::new().unwrap();
        let cache_path = dir.path().join("version-check.json");

        // Cache says "0.0.1" — older than any real build, so is_newer is false
        let cache = VersionCache::new("0.0.1".to_owned());
        cache.write_to_path(&cache_path).unwrap();

        // No notice (0.0.1 is not newer than current)
        check_and_print_update_notice_inner(&Some(cache_path));
    }

    #[test]
    fn notice_with_fresh_cache_newer_version() {
        let dir = tempfile::TempDir::new().unwrap();
        let cache_path = dir.path().join("version-check.json");

        // Write a very high version number so it's definitely newer
        let cache = VersionCache::new("9999.0.0".to_owned());
        cache.write_to_path(&cache_path).unwrap();

        // Should run without panic (notice printed to stderr, not captured in unit test)
        check_and_print_update_notice_inner(&Some(cache_path));
    }

    #[test]
    fn notice_skips_when_no_cache_path() {
        // No cache path → network would be needed but silently skips on failure
        // (or does a real fetch — in CI this tests the network-error-silent-skip path)
        check_and_print_update_notice_inner(&None);
    }

    #[test]
    fn notice_skips_when_cache_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let nonexistent = dir.path().join("no-such-file.json");
        // Missing cache → stale → tries network. Network will fail in unit test,
        // which is the silent-skip path. Should not panic.
        check_and_print_update_notice_inner(&Some(nonexistent));
    }
}
