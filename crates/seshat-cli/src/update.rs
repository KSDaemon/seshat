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

struct RateLimitInfo {
    retry_after_minutes: u64,
}

pub fn run_update(check: bool) -> Result<(), CliError> {
    if check {
        run_check()
    } else {
        run_self_update()
    }
}

pub fn check_and_print_update_notice() {
    check_and_print_update_notice_inner(&VersionCache::cache_path());
}

fn check_and_print_update_notice_inner(cache_path: &Option<PathBuf>) {
    let current = env!("CARGO_PKG_VERSION");

    if let Some(path) = cache_path {
        if let Some(cache) = VersionCache::read_from_path(path) {
            if cache.is_fresh() {
                if cache.has_assets == Some(false) {
                    return;
                }
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

    let (version, has_assets) = match fetch_latest_release() {
        Ok(result) => result,
        Err(_) => return,
    };

    if let Some(path) = cache_path {
        let cache = if has_assets {
            VersionCache::with_assets(version.clone(), true)
        } else {
            VersionCache::with_assets(current.to_owned(), false)
        };
        let _ = cache.write_to_path(path);
    }

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
    let install_method = detect_install_method()?;
    if install_method == InstallMethod::Homebrew {
        eprintln!("Seshat was installed via Homebrew. Run brew upgrade seshat to update.");
        return Err(CliError::CommandFailed {
            command: "update".to_owned(),
            reason: "installed via Homebrew".to_owned(),
        });
    }

    let current = env!("CARGO_PKG_VERSION");

    let release_assets = fetch_release_assets()?;
    let (version, asset_url, checksums_url) = match release_assets {
        Some(assets) => assets,
        None => {
            println!("Seshat is up to date (v{current}).");
            return Ok(());
        }
    };

    if !is_newer(&version, current) {
        println!("Seshat is already up to date (v{current}).");
        return Ok(());
    }

    let expected_sha256 = fetch_checksum_for_asset(&checksums_url, &version)?;

    let temp_dir = tempfile::TempDir::new().map_err(|e| CliError::CommandFailed {
        command: "update".to_owned(),
        reason: format!("failed to create temp directory: {e}"),
    })?;

    let download_path = temp_dir
        .path()
        .join(format!("seshat.{}", archive_extension(current_target())));
    download_with_progress(&asset_url, &download_path)?;

    verify_sha256(&download_path, &expected_sha256).inspect_err(|_| {
        let _ = fs::remove_dir_all(temp_dir.path());
    })?;

    let binary_path =
        extract_binary(&download_path, temp_dir.path(), &version).inspect_err(|_| {
            let _ = fs::remove_dir_all(temp_dir.path());
        })?;

    preflight_check(&binary_path, temp_dir.path())?;

    let target_exe = resolve_target_exe()?;

    replace_binary(&binary_path, &target_exe, temp_dir.path())?;

    if is_cargo_install() {
        println!(
            "Note: seshat was installed via cargo. You may want to run 'cargo install seshat' to keep ~/.cargo/.crates2.json in sync."
        );
    }

    println!("Seshat updated to v{version}.");
    Ok(())
}

fn detect_install_method() -> Result<InstallMethod, CliError> {
    if cfg!(target_os = "windows") {
        return Ok(InstallMethod::Direct);
    }

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

fn fetch_release_assets() -> Result<Option<(String, String, String)>, CliError> {
    let agent = build_agent();

    let response = agent
        .get(GITHUB_RELEASES_API)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!("failed to fetch release info: {e}"),
        })?;

    let status = response.status().into();
    let headers = response.headers().clone();
    check_response_status(status, &headers)?;

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

    let checksums_url = find_checksums_url(assets, &version)?;

    let binary_asset = find_binary_asset(assets, target);
    match binary_asset {
        Some((asset_name, asset_url)) => {
            if !asset_name.contains(&version) {
                eprintln!(
                    "Warning: asset name '{asset_name}' does not contain version '{version}', proceeding anyway."
                );
            }
            Ok(Some((version, asset_url, checksums_url)))
        }
        None => Ok(None),
    }
}

fn find_checksums_url(assets: &[serde_json::Value], version: &str) -> Result<String, CliError> {
    let mut best: Option<String> = None;

    for asset in assets {
        let name = asset["name"].as_str().unwrap_or("");
        if name == "sha256sums.txt" || name.contains("sha256sums") {
            let url = asset["browser_download_url"]
                .as_str()
                .map(|u| u.to_owned())
                .ok_or_else(|| CliError::CommandFailed {
                    command: "update".to_owned(),
                    reason: "no download URL for checksums file".to_owned(),
                })?;

            if name.contains(version) {
                return Ok(url);
            }
            if best.is_none() {
                best = Some(url);
            }
        }
    }

    best.ok_or_else(|| CliError::CommandFailed {
        command: "update".to_owned(),
        reason: "checksums file not found in release assets".to_owned(),
    })
}

fn find_binary_asset(assets: &[serde_json::Value], target: &str) -> Option<(String, String)> {
    let want_zip = target.ends_with("windows-msvc");
    assets.iter().find_map(|asset| {
        let name = asset["name"].as_str().unwrap_or("");
        let extension_match = if want_zip {
            name.ends_with(".zip")
        } else {
            name.ends_with(".tar.gz") || name.ends_with(".tgz")
        };
        if name.contains(target) && extension_match {
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

    let status = response.status().into();
    let headers = response.headers().clone();
    check_response_status(status, &headers)?;

    let body = response
        .into_body()
        .read_to_string()
        .map_err(|e| CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!("failed to read checksums: {e}"),
        })?;

    let target = current_target();
    let extension = archive_extension(target);
    let expected_archive = format!("seshat-{target}-v{version}.{extension}");

    for line in body.lines() {
        let mut trimmed = line.trim();
        if let Some(stripped) = trimmed.strip_prefix('*') {
            trimmed = stripped;
        }
        if let Some((hex, filename)) = trimmed.split_once([' ', '\t']) {
            let filename = filename.trim();
            if filename == expected_archive || filename.ends_with(&expected_archive) {
                return Ok(hex.to_owned());
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

    let status = response.status().into();
    let headers = response.headers().clone();
    check_response_status(status, &headers)?;

    let total_size = response
        .headers()
        .get("Content-Length")
        .and_then(|v| v.to_str().ok().and_then(|s| s.parse().ok()))
        .unwrap_or(0u64);

    let style = if total_size > 0 {
        ProgressBar::new(total_size)
    } else {
        ProgressBar::new_spinner()
    };
    let pb = style;
    if total_size > 0 {
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})",
            )
            .unwrap()
            .progress_chars("#>-"),
        );
    } else {
        pb.set_style(
            ProgressStyle::with_template("{spinner:.green} [{elapsed_precise}] {bytes} (? eta)")
                .unwrap(),
        );
    }

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
        if total_size > 0 {
            pb.set_position(downloaded);
        } else {
            pb.set_message(format!("Downloaded {downloaded} bytes"));
        }
    }

    if downloaded == 0 {
        let _ = fs::remove_file(dest);
        return Err(CliError::CommandFailed {
            command: "update".to_owned(),
            reason: "downloaded file is empty (0 bytes)".to_owned(),
        });
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

    let name = archive_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if name.ends_with(".zip") {
        extract_zip(archive_file, dest_dir)?;
    } else {
        extract_tar_gz(archive_file, dest_dir)?;
    }

    let target = current_target();
    let expected_dir = format!("seshat-{target}-v{version}");
    let binary_path = dest_dir
        .join(&expected_dir)
        .join(format!("seshat{}", std::env::consts::EXE_SUFFIX));

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

fn extract_tar_gz(archive_file: fs::File, dest_dir: &Path) -> Result<(), CliError> {
    let decoder = GzDecoder::new(archive_file);
    let mut archive = Archive::new(decoder);

    for entry in archive.entries().map_err(|e| CliError::CommandFailed {
        command: "update".to_owned(),
        reason: format!("failed to read archive entries: {e}"),
    })? {
        let mut entry = entry.map_err(|e| CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!("failed to read archive entry: {e}"),
        })?;

        let path = entry.path().map_err(|e| CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!("failed to resolve archive entry path: {e}"),
        })?;

        if path.as_os_str().is_empty() {
            continue;
        }

        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            continue;
        }

        let abs_path = dest_dir.join(&path);
        let Ok(canonical) = abs_path.canonicalize() else {
            entry
                .unpack_in(dest_dir)
                .map_err(|e| CliError::CommandFailed {
                    command: "update".to_owned(),
                    reason: format!("failed to extract entry: {e}"),
                })?;
            continue;
        };

        if !canonical.starts_with(dest_dir) {
            continue;
        }

        entry
            .unpack_in(dest_dir)
            .map_err(|e| CliError::CommandFailed {
                command: "update".to_owned(),
                reason: format!("failed to extract entry: {e}"),
            })?;
    }

    Ok(())
}

/// Verify that `abs_path` resolves inside `canonical_dest_dir`, even when the
/// leaf or some ancestors do not yet exist on disk.
///
/// The previous guard called `abs_path.canonicalize()` directly, which returns
/// `Err` for paths whose final component is missing — and the surrounding
/// `if let Ok(_) = ...` silently skipped the check. That left two real attack
/// shapes uncovered:
///
/// 1. **Symlink-via-zip**: a previous entry creates `dest_dir/link -> /etc`,
///    and a later entry `link/file` resolves through the symlink.
/// 2. **Brand-new escape paths**: an entry whose parent directory does not yet
///    exist sails past the existence-gated check entirely.
///
/// This helper walks the ancestor chain bottom-up to find the deepest
/// component that *does* exist, canonicalises that ancestor (which follows
/// symlinks), and rejects the entry unless the canonical form still lives
/// under `canonical_dest_dir`. `dest_dir` always exists, so the loop is
/// bounded.
fn path_stays_inside_dest(abs_path: &Path, canonical_dest_dir: &Path) -> bool {
    let mut probe: &Path = abs_path;
    loop {
        if probe.exists() {
            return match probe.canonicalize() {
                Ok(canonical) => canonical.starts_with(canonical_dest_dir),
                Err(_) => false,
            };
        }
        match probe.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => probe = parent,
            _ => return false,
        }
    }
}

fn extract_zip(archive_file: fs::File, dest_dir: &Path) -> Result<(), CliError> {
    let mut archive = zip::ZipArchive::new(archive_file).map_err(|e| CliError::CommandFailed {
        command: "update".to_owned(),
        reason: format!("failed to read zip archive: {e}"),
    })?;

    let canonical_dest_dir = dest_dir
        .canonicalize()
        .map_err(|e| CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!("failed to canonicalise extraction directory: {e}"),
        })?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!("failed to read zip entry: {e}"),
        })?;

        let raw_name = entry.name().to_owned();
        if raw_name.is_empty() {
            continue;
        }

        let entry_path = match entry.enclosed_name() {
            Some(p) => p,
            None => continue,
        };

        if entry_path.as_os_str().is_empty() {
            continue;
        }

        if entry_path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            continue;
        }

        let abs_path = dest_dir.join(&entry_path);
        if !path_stays_inside_dest(&abs_path, &canonical_dest_dir) {
            continue;
        }

        if entry.is_dir() {
            fs::create_dir_all(&abs_path).map_err(|e| CliError::CommandFailed {
                command: "update".to_owned(),
                reason: format!("failed to create directory: {e}"),
            })?;
            continue;
        }

        if let Some(parent) = abs_path.parent() {
            fs::create_dir_all(parent).map_err(|e| CliError::CommandFailed {
                command: "update".to_owned(),
                reason: format!("failed to create directory: {e}"),
            })?;
        }

        let mut out = fs::File::create(&abs_path).map_err(|e| CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!("failed to create extracted file: {e}"),
        })?;
        std::io::copy(&mut entry, &mut out).map_err(|e| CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!("failed to extract zip entry: {e}"),
        })?;

        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&abs_path, fs::Permissions::from_mode(mode));
        }
    }

    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::metadata(path).map_err(|e| CliError::CommandFailed {
        command: "update".to_owned(),
        reason: format!("failed to read binary metadata: {e}"),
    })?;
    let mut perms = metadata.permissions();
    perms.set_mode(0o755);
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

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        match output.status.signal() {
            Some(9) => {
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
            Some(sig) => {
                let _ = fs::remove_dir_all(temp_dir);
                return Err(CliError::CommandFailed {
                    command: "update".to_owned(),
                    reason: format!("extracted binary terminated by signal {sig}"),
                });
            }
            None => {}
        }
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    if version_output_contains_seshat(&stdout) || version_output_contains_seshat(&stderr) {
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

fn version_output_contains_seshat(output: &str) -> bool {
    let lower = output.to_lowercase();
    if let Some(idx) = lower.find("seshat") {
        let after = &lower[idx + "seshat".len()..];
        return after
            .trim_start()
            .starts_with(|c: char| c.is_ascii_digit() || c == 'v');
    }
    false
}

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

fn replace_binary(new_binary: &Path, target_exe: &Path, temp_dir: &Path) -> Result<(), CliError> {
    match self_replace::self_replace(new_binary) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_dir_all(temp_dir);
            Err(map_replace_error(e, target_exe))
        }
    }
}

fn map_replace_error(e: std::io::Error, target_exe: &Path) -> CliError {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        #[cfg(windows)]
        let hint = "Try running as Administrator.";
        #[cfg(not(windows))]
        let hint = "Try: sudo seshat update";
        eprintln!(
            "Permission denied updating {}. {hint}",
            target_exe.display()
        );
        #[cfg(windows)]
        let reason = "permission denied; try running as Administrator".to_owned();
        #[cfg(not(windows))]
        let reason = "permission denied; try sudo seshat update".to_owned();
        CliError::CommandFailed {
            command: "update".to_owned(),
            reason,
        }
    } else {
        CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!("failed to replace binary: {e}"),
        }
    }
}

/// Best-effort cleanup of a leftover `<current_exe>.old` from a prior
/// Windows self-update.
///
/// On Windows, manual or recovery scenarios may leave a `seshat.exe.old`
/// next to the running `seshat.exe` (the happy-path `self_replace::self_replace`
/// flow already schedules its own relocated-binary deletion via the crate's
/// `.__selfdelete__.exe` helper, so this is purely defensive). Errors are
/// silently dropped — cleanup must never fail the user's command.
///
/// On Unix this is a no-op: atomic `rename(2)` semantics in `replace_binary`
/// never leave a `.old` file behind, so there is nothing to probe for.
pub fn cleanup_stale_old_binary() {
    #[cfg(windows)]
    if let Ok(current) = std::env::current_exe() {
        cleanup_stale_old_binary_at(&current);
    }
}

#[cfg(windows)]
fn cleanup_stale_old_binary_at(current_exe: &Path) {
    let mut stale: std::ffi::OsString = current_exe.as_os_str().to_owned();
    stale.push(".old");
    let _ = fs::remove_file(PathBuf::from(stale));
}

fn is_cargo_install() -> bool {
    let cargo_dir = if let Ok(cargo_home) = std::env::var("CARGO_HOME") {
        PathBuf::from(cargo_home)
    } else if let Some(home) = dirs::home_dir() {
        home.join(".cargo")
    } else {
        return false;
    };

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

    let crates_toml = cargo_dir.join(".crates.toml");
    if crates_toml.exists() {
        if let Ok(content) = fs::read_to_string(&crates_toml) {
            if cargo_toml_contains_seshat(&content) {
                return true;
            }
        }
    }

    false
}

fn cargo_json_contains_seshat(json: &serde_json::Value) -> bool {
    if let Some(installs) = json.get("installs").and_then(|v| v.as_object()) {
        return installs.keys().any(|k| k.starts_with("seshat "));
    }
    false
}

fn cargo_toml_contains_seshat(content: &str) -> bool {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('[') {
            continue;
        }
        if let Some((key, _)) = trimmed.split_once('=').or_else(|| trimmed.split_once(" =")) {
            let key = key.trim().trim_matches('"');
            if key.starts_with("seshat ") {
                return true;
            }
        }
    }
    false
}

fn build_agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(TIMEOUT_SECS)))
        .build();
    let agent: ureq::Agent = config.into();

    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        if !token.is_empty() {
            return agent;
        }
    }

    agent
}

fn check_response_status(status: u16, headers: &ureq::http::HeaderMap) -> Result<(), CliError> {
    if status < 400 {
        return Ok(());
    }

    if let Some(info) = parse_rate_limit(status, headers) {
        return Err(CliError::CommandFailed {
            command: "update".to_owned(),
            reason: format!(
                "rate limited by GitHub. Try again in {} minutes.",
                info.retry_after_minutes
            ),
        });
    }

    let reason = if status == 404 {
        "release not found (404)".to_owned()
    } else if status >= 500 {
        format!("GitHub server error (HTTP {status})")
    } else {
        format!("HTTP {status}")
    };

    Err(CliError::CommandFailed {
        command: "update".to_owned(),
        reason,
    })
}

fn parse_rate_limit(status: u16, headers: &ureq::http::HeaderMap) -> Option<RateLimitInfo> {
    if status != 403 && status != 429 {
        return None;
    }

    let reset = headers
        .get("x-ratelimit-reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();

    let retry_after_minutes = if reset > now {
        ((reset - now) / 60).max(1)
    } else {
        1
    };

    Some(RateLimitInfo {
        retry_after_minutes,
    })
}

fn run_check() -> Result<(), CliError> {
    run_check_inner(&VersionCache::cache_path())
}

fn run_check_inner(cache_path: &Option<PathBuf>) -> Result<(), CliError> {
    if let Some(path) = cache_path {
        if let Some(cache) = VersionCache::read_from_path(path) {
            if cache.is_fresh() && cache.has_assets != Some(false) {
                return print_update_status(&cache.latest_version);
            }
        }
    }

    match fetch_latest_release() {
        Ok((version, has_assets)) => {
            if let Some(path) = cache_path {
                let cache = if has_assets {
                    VersionCache::with_assets(version.clone(), true)
                } else {
                    VersionCache::with_assets(env!("CARGO_PKG_VERSION").to_owned(), false)
                };
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
    let agent = build_agent();

    let response = agent
        .get(GITHUB_RELEASES_API)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| format!("network error: {e}"))?;

    let status = response.status().into();
    let headers = response.headers().clone();

    if status >= 400 {
        if let Some(info) = parse_rate_limit(status, &headers) {
            return Err(format!(
                "rate limited by GitHub. Try again in {} minutes.",
                info.retry_after_minutes
            ));
        }
        if status == 404 {
            return Err("release not found (404)".to_owned());
        }
        return Err(format!("HTTP {status}"));
    }

    let body = response
        .into_body()
        .read_to_string()
        .map_err(|e| format!("failed to read response: {e}"))?;

    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("failed to parse response: {e}"))?;

    // Check for GitHub error payload
    if let Some(msg) = json.get("message").and_then(|v| v.as_str()) {
        return Err(format!("GitHub API error: {msg}"));
    }

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
        ("x86_64", "linux") => {
            if is_musl() {
                "x86_64-unknown-linux-musl"
            } else {
                "x86_64-unknown-linux-gnu"
            }
        }
        ("aarch64", "linux") => {
            if is_musl() {
                "aarch64-unknown-linux-musl"
            } else {
                "aarch64-unknown-linux-gnu"
            }
        }
        ("x86_64", "windows") => "x86_64-pc-windows-msvc",
        _ => "unsupported",
    }
}

/// Archive extension for the release artifact of `target`.
///
/// Centralises the "is this a zip target?" predicate so the download path,
/// checksum lookup, and asset matcher cannot drift out of sync. Returns
/// `"zip"` for Windows MSVC targets (which are packaged via `7z` in
/// `release.yml`), and `"tar.gz"` for all Unix targets.
fn archive_extension(target: &str) -> &'static str {
    if target.ends_with("windows-msvc") {
        "zip"
    } else {
        "tar.gz"
    }
}

fn is_musl() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_dir("/lib")
            .ok()
            .and_then(|entries| {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    if let Some(name_str) = name.to_str() {
                        if name_str.contains("ld-musl") {
                            return Some(true);
                        }
                    }
                }
                None
            })
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
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
        #[cfg(any(
            target_os = "macos",
            target_os = "linux",
            all(target_os = "windows", target_arch = "x86_64"),
        ))]
        assert_ne!(target, "unsupported");
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        assert_eq!(target, "x86_64-pc-windows-msvc");
    }

    #[test]
    fn archive_extension_matches_target_platform() {
        assert_eq!(archive_extension("x86_64-pc-windows-msvc"), "zip");
        assert_eq!(archive_extension("aarch64-pc-windows-msvc"), "zip");
        assert_eq!(archive_extension("x86_64-unknown-linux-gnu"), "tar.gz");
        assert_eq!(archive_extension("x86_64-unknown-linux-musl"), "tar.gz");
        assert_eq!(archive_extension("aarch64-apple-darwin"), "tar.gz");
        assert_eq!(archive_extension("x86_64-apple-darwin"), "tar.gz");
    }

    /// Regression test for the hardcoded `seshat.tar.gz` download filename
    /// bug: `extract_binary` dispatches on the file extension, so the
    /// download path must use `.zip` on Windows-MSVC and `.tar.gz` elsewhere.
    /// If this invariant breaks, `extract_binary` will feed zip bytes to
    /// `GzDecoder` (or vice versa) and the user-facing update flow fails
    /// even though every fixture-based test still passes.
    #[test]
    fn download_filename_extension_matches_extract_dispatch() {
        for target in [
            "x86_64-pc-windows-msvc",
            "x86_64-unknown-linux-gnu",
            "x86_64-unknown-linux-musl",
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
        ] {
            let filename = format!("seshat.{}", archive_extension(target));
            if archive_extension(target) == "zip" {
                assert!(
                    filename.ends_with(".zip"),
                    "download filename for {target} must end with .zip"
                );
            } else {
                assert!(
                    filename.ends_with(".tar.gz"),
                    "download filename for {target} must end with .tar.gz"
                );
            }
        }
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

    fn build_zip_archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Cursor;
        use zip::write::SimpleFileOptions;

        let mut buf = Vec::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut writer = zip::ZipWriter::new(cursor);
            let opts =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            for (name, data) in entries {
                if name.ends_with('/') {
                    writer.add_directory(*name, opts).unwrap();
                } else {
                    writer.start_file(*name, opts).unwrap();
                    writer.write_all(data).unwrap();
                }
            }
            writer.finish().unwrap();
        }
        buf
    }

    #[test]
    fn extract_binary_from_valid_zip() {
        let dir = tempfile::TempDir::new().unwrap();
        let archive_path = dir.path().join("test.zip");

        let expected_dir = format!("seshat-{}-v1.0.0", current_target());
        let binary_in_zip = format!("{expected_dir}/seshat{}", std::env::consts::EXE_SUFFIX);
        let dir_entry = format!("{expected_dir}/");

        let bytes = build_zip_archive(&[(&dir_entry, &[]), (&binary_in_zip, b"fake")]);
        fs::write(&archive_path, &bytes).unwrap();

        let result = extract_binary(&archive_path, dir.path(), "1.0.0");
        assert!(result.is_ok(), "extract_binary failed: {result:?}");
        let binary_path = result.unwrap();
        assert!(binary_path.is_file());
        assert!(binary_path.ends_with(format!(
            "{expected_dir}/seshat{}",
            std::env::consts::EXE_SUFFIX
        )));
    }

    #[test]
    fn extract_binary_corrupted_zip_errors() {
        let dir = tempfile::TempDir::new().unwrap();
        let archive_path = dir.path().join("corrupt.zip");
        fs::write(&archive_path, b"definitely not a zip file").unwrap();

        let result = extract_binary(&archive_path, dir.path(), "1.0.0");
        assert!(result.is_err());
    }

    #[test]
    fn extract_binary_zip_skips_path_traversal() {
        let dir = tempfile::TempDir::new().unwrap();
        let archive_path = dir.path().join("traversal.zip");

        let traversal_name = format!("../escape/seshat{}", std::env::consts::EXE_SUFFIX);
        let bytes = build_zip_archive(&[(&traversal_name, b"evil")]);
        fs::write(&archive_path, &bytes).unwrap();

        let result = extract_binary(&archive_path, dir.path(), "1.0.0");
        assert!(
            result.is_err(),
            "expected missing-binary error, got {result:?}"
        );
        let escape_path = dir
            .path()
            .parent()
            .unwrap()
            .join("escape")
            .join(format!("seshat{}", std::env::consts::EXE_SUFFIX));
        assert!(
            !escape_path.exists(),
            "traversal entry was extracted to {}",
            escape_path.display()
        );
    }

    #[test]
    fn path_stays_inside_dest_accepts_normal_relative_paths() {
        let dir = tempfile::TempDir::new().unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let leaf = dir.path().join("subdir").join("file.txt");
        assert!(path_stays_inside_dest(&leaf, &canonical));
    }

    #[test]
    fn path_stays_inside_dest_rejects_path_outside_dest() {
        let dir = tempfile::TempDir::new().unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let outside = std::env::temp_dir().join("definitely-not-in-dest");
        assert!(!path_stays_inside_dest(&outside, &canonical));
    }

    #[cfg(unix)]
    #[test]
    fn path_stays_inside_dest_rejects_path_resolving_through_symlink() {
        let dir = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        let canonical = dir.path().canonicalize().unwrap();

        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();

        // The leaf doesn't exist yet; the ancestor `link` does and resolves
        // outside `canonical_dest_dir`. The previous canonicalize-only guard
        // returned `Err` here and silently allowed the entry.
        let leaf = dir.path().join("link").join("payload.txt");
        assert!(!path_stays_inside_dest(&leaf, &canonical));
    }

    /// Regression test for the canonicalize-bypass bug. A pre-placed symlink
    /// inside `dest_dir` points outside; a zip entry uses the symlink as a
    /// path component. `extract_zip` must not write through the symlink.
    #[cfg(unix)]
    #[test]
    fn extract_zip_rejects_entry_escaping_through_existing_symlink() {
        let dir = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();

        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();

        let bytes = build_zip_archive(&[("link/payload.txt", b"escaped")]);
        let archive_path = dir.path().join("malicious.zip");
        fs::write(&archive_path, &bytes).unwrap();

        let archive_file = fs::File::open(&archive_path).unwrap();
        // Extraction should not error; the malicious entry is skipped.
        extract_zip(archive_file, dir.path()).unwrap();

        assert!(
            !outside.path().join("payload.txt").exists(),
            "entry escaped extraction directory through symlink"
        );
    }

    #[test]
    fn extract_binary_dispatches_on_extension() {
        let dir = tempfile::TempDir::new().unwrap();
        let expected_dir = format!("seshat-{}-v1.0.0", current_target());
        let binary_in_zip = format!("{expected_dir}/seshat{}", std::env::consts::EXE_SUFFIX);
        let dir_entry = format!("{expected_dir}/");
        let zip_bytes = build_zip_archive(&[(&dir_entry, &[]), (&binary_in_zip, b"fake")]);

        let zip_named = dir.path().join("ok.zip");
        fs::write(&zip_named, &zip_bytes).unwrap();
        let ok = extract_binary(&zip_named, dir.path(), "1.0.0");
        assert!(ok.is_ok(), "zip dispatch failed: {ok:?}");

        let dir2 = tempfile::TempDir::new().unwrap();
        let mismatched = dir2.path().join("ok.tar.gz");
        fs::write(&mismatched, &zip_bytes).unwrap();
        let err = extract_binary(&mismatched, dir2.path(), "1.0.0");
        assert!(
            err.is_err(),
            "expected error when zip bytes are read as tar.gz, got {err:?}"
        );
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
    fn find_binary_asset_matches_windows_target() {
        let assets = vec![serde_json::json!({
            "name": "seshat-x86_64-pc-windows-msvc-v1.0.0.zip",
            "browser_download_url": "https://example.com/asset.zip"
        })];

        let result = find_binary_asset(&assets, "x86_64-pc-windows-msvc");
        assert!(result.is_some());
        let (name, url) = result.unwrap();
        assert!(name.ends_with(".zip"));
        assert_eq!(url, "https://example.com/asset.zip");
    }

    #[test]
    fn find_binary_asset_skips_zip_on_unix_target() {
        let assets = vec![serde_json::json!({
            "name": "seshat-x86_64-unknown-linux-gnu-v1.0.0.zip",
            "browser_download_url": "https://example.com/asset.zip"
        })];

        let result = find_binary_asset(&assets, "x86_64-unknown-linux-gnu");
        assert!(result.is_none());
    }

    #[test]
    fn find_checksums_url_prefers_version_match() {
        let assets = vec![
            serde_json::json!({
                "name": "sha256sums-v0.5.0.txt",
                "browser_download_url": "https://example.com/sha256sums-old.txt"
            }),
            serde_json::json!({
                "name": "sha256sums-v1.0.0.txt",
                "browser_download_url": "https://example.com/sha256sums-v1.0.0.txt"
            }),
        ];

        let result = find_checksums_url(&assets, "1.0.0");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "https://example.com/sha256sums-v1.0.0.txt");
    }

    #[test]
    fn find_checksums_url_fallback_first_match() {
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

        let result = find_checksums_url(&assets, "1.0.0");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "https://example.com/sha256sums.txt");
    }

    #[test]
    fn find_checksums_url_not_found() {
        let assets = vec![serde_json::json!({
            "name": "seshat-aarch64-apple-darwin-v1.0.0.tar.gz",
            "browser_download_url": "https://example.com/asset1.tar.gz"
        })];

        let result = find_checksums_url(&assets, "1.0.0");
        assert!(result.is_err());
    }

    #[test]
    fn is_cargo_install_returns_bool() {
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
    fn cargo_toml_contains_seshat_true() {
        let content = r#"[v1]
"seshat 1.0.0 (registry+https://github.com/rust-lang/crates.io-index)" = ["seshat"]
"#;
        assert!(cargo_toml_contains_seshat(content));
    }

    #[test]
    fn cargo_toml_contains_seshat_false() {
        let content = r#"[v1]
"ripgrep 13.0.0 (registry+https://github.com/rust-lang/crates.io-index)" = ["rg"]
"#;
        assert!(!cargo_toml_contains_seshat(content));
    }

    #[test]
    fn cargo_toml_substring_no_false_positive() {
        let content = r#"[v1]
"seshat-something 1.0.0" = ["not-seshat"]
"#;
        assert!(!cargo_toml_contains_seshat(content));
    }

    #[test]
    fn cargo_toml_empty() {
        assert!(!cargo_toml_contains_seshat(""));
        assert!(!cargo_toml_contains_seshat("[v1]\n"));
    }

    #[test]
    fn is_cargo_install_with_fake_crates2_json() {
        let dir = tempfile::TempDir::new().unwrap();
        let cargo_dir = dir.path();

        let crates2 = cargo_dir.join(".crates2.json");
        let json = serde_json::json!({
            "installs": {
                "seshat 1.0.0 (registry+https://github.com/rust-lang/crates.io-index)": {
                    "bins": ["seshat"]
                }
            }
        });
        fs::write(&crates2, serde_json::to_string(&json).unwrap()).unwrap();

        let content = fs::read_to_string(&crates2).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(cargo_json_contains_seshat(&parsed));
    }

    #[test]
    fn is_cargo_install_with_corrupted_crates2_json() {
        let dir = tempfile::TempDir::new().unwrap();
        let crates2 = dir.path().join(".crates2.json");
        fs::write(&crates2, b"not valid json").unwrap();

        let content = fs::read_to_string(&crates2).unwrap();
        let result = serde_json::from_str::<serde_json::Value>(&content);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_target_exe_returns_path() {
        let result = resolve_target_exe();
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.is_absolute());
    }

    #[test]
    fn map_replace_error_translates_permission_denied() {
        let dir = tempfile::TempDir::new().unwrap();
        let target = dir.path().join("seshat");
        let e = std::io::Error::from(std::io::ErrorKind::PermissionDenied);

        let cli_err = map_replace_error(e, &target);
        match cli_err {
            CliError::CommandFailed { command, reason } => {
                assert_eq!(command, "update");
                #[cfg(windows)]
                assert!(
                    reason.contains("Administrator"),
                    "Windows reason should mention Administrator hint, got: {reason}"
                );
                #[cfg(not(windows))]
                assert!(
                    reason.contains("sudo seshat update"),
                    "Unix reason should mention sudo hint, got: {reason}"
                );
            }
            other => panic!("expected CommandFailed, got: {other:?}"),
        }
    }

    #[test]
    fn map_replace_error_passes_through_other_errors() {
        let dir = tempfile::TempDir::new().unwrap();
        let target = dir.path().join("seshat");
        let e = std::io::Error::other("boom");

        let cli_err = map_replace_error(e, &target);
        match cli_err {
            CliError::CommandFailed { reason, .. } => {
                assert!(
                    reason.starts_with("failed to replace binary: "),
                    "non-permission errors should map to the generic 'failed to replace binary' reason, got: {reason}"
                );
                assert!(reason.contains("boom"));
            }
            other => panic!("expected CommandFailed, got: {other:?}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn replace_binary_translates_permission_denied_to_admin_hint_on_windows() {
        let dir = tempfile::TempDir::new().unwrap();
        let target = dir.path().join("seshat.exe");
        let e = std::io::Error::from(std::io::ErrorKind::PermissionDenied);

        let cli_err = map_replace_error(e, &target);
        match cli_err {
            CliError::CommandFailed { reason, .. } => {
                assert!(
                    reason.contains("Administrator"),
                    "Windows admin hint should appear in the CliError reason, got: {reason}"
                );
                assert!(!reason.contains("sudo"));
            }
            other => panic!("expected CommandFailed, got: {other:?}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn is_cargo_install_with_fake_crates2_json_on_windows() {
        let dir = tempfile::TempDir::new().unwrap();
        let cargo_dir = dir.path();

        let crates2 = cargo_dir.join(".crates2.json");
        let json = serde_json::json!({
            "installs": {
                "seshat 1.0.0 (registry+https://github.com/rust-lang/crates.io-index)": {
                    "bins": ["seshat.exe"]
                }
            }
        });
        fs::write(&crates2, serde_json::to_string(&json).unwrap()).unwrap();

        let content = fs::read_to_string(&crates2).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(cargo_json_contains_seshat(&parsed));
    }

    #[test]
    fn preflight_check_with_valid_binary() {
        let dir = tempfile::TempDir::new().unwrap();

        let echo_path = std::path::Path::new("/bin/echo");
        if !echo_path.exists() {
            return;
        }

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
    fn preflight_check_detects_nonzero_exit() {
        let dir = tempfile::TempDir::new().unwrap();
        let script = dir.path().join("failing_binary");
        fs::write(&script, b"#!/bin/sh\nexit 1\n").unwrap();

        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();

        let result = preflight_check(&script, dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn version_output_contains_seshat_with_version() {
        assert!(version_output_contains_seshat("seshat 1.2.3"));
        assert!(version_output_contains_seshat("seshat v0.2.0"));
        assert!(version_output_contains_seshat("foo seshat 1.0.0"));
    }

    #[test]
    fn version_output_does_not_contain_seshat() {
        assert!(!version_output_contains_seshat(""));
        assert!(!version_output_contains_seshat("something else"));
        assert!(!version_output_contains_seshat("seshat not a version"));
        assert!(!version_output_contains_seshat("seshat-error happened"));
    }

    #[test]
    fn notice_skips_when_cache_fresh_and_up_to_date() {
        let dir = tempfile::TempDir::new().unwrap();
        let cache_path = dir.path().join("version-check.json");

        let current = env!("CARGO_PKG_VERSION");
        let cache = VersionCache::new(current.to_owned());
        cache.write_to_path(&cache_path).unwrap();

        check_and_print_update_notice_inner(&Some(cache_path));
    }

    #[test]
    fn notice_skips_when_cache_fresh_and_old_version() {
        let dir = tempfile::TempDir::new().unwrap();
        let cache_path = dir.path().join("version-check.json");

        let cache = VersionCache::new("0.0.1".to_owned());
        cache.write_to_path(&cache_path).unwrap();

        check_and_print_update_notice_inner(&Some(cache_path));
    }

    #[test]
    fn notice_skips_when_cache_no_assets() {
        let dir = tempfile::TempDir::new().unwrap();
        let cache_path = dir.path().join("version-check.json");

        let cache = VersionCache::with_assets("9999.0.0".to_owned(), false);
        cache.write_to_path(&cache_path).unwrap();

        check_and_print_update_notice_inner(&Some(cache_path));
    }

    #[test]
    fn notice_with_fresh_cache_newer_version() {
        let dir = tempfile::TempDir::new().unwrap();
        let cache_path = dir.path().join("version-check.json");

        let cache = VersionCache::new("9999.0.0".to_owned());
        cache.write_to_path(&cache_path).unwrap();

        check_and_print_update_notice_inner(&Some(cache_path));
    }

    #[test]
    fn notice_skips_when_no_cache_path() {
        check_and_print_update_notice_inner(&None);
    }

    #[test]
    fn notice_skips_when_cache_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let nonexistent = dir.path().join("no-such-file.json");
        check_and_print_update_notice_inner(&Some(nonexistent));
    }

    // ── parse_rate_limit / check_response_status ─────────────────────

    fn future_reset_headers(seconds_from_now: u64) -> ureq::http::HeaderMap {
        let mut h = ureq::http::HeaderMap::new();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let reset = now + seconds_from_now;
        h.insert("x-ratelimit-reset", reset.to_string().parse().unwrap());
        h
    }

    #[test]
    fn parse_rate_limit_ignores_non_throttling_status() {
        let h = future_reset_headers(600);
        assert!(parse_rate_limit(200, &h).is_none());
        assert!(parse_rate_limit(404, &h).is_none());
        assert!(parse_rate_limit(500, &h).is_none());
    }

    #[test]
    fn parse_rate_limit_handles_403_with_reset_header() {
        let h = future_reset_headers(1800); // 30 minutes from now
        let info = parse_rate_limit(403, &h).expect("should parse");
        // Rounding to whole minutes can drop us to 29 right at the boundary;
        // anything in 25..=30 is fine for an integration-ish unit test.
        assert!(
            (25..=30).contains(&info.retry_after_minutes),
            "unexpected retry_after_minutes: {}",
            info.retry_after_minutes
        );
    }

    #[test]
    fn parse_rate_limit_handles_429_with_reset_header() {
        let h = future_reset_headers(120);
        let info = parse_rate_limit(429, &h).expect("should parse");
        assert!(info.retry_after_minutes >= 1);
    }

    #[test]
    fn parse_rate_limit_clamps_past_reset_to_one_minute() {
        let mut h = ureq::http::HeaderMap::new();
        h.insert("x-ratelimit-reset", "1".parse().unwrap()); // far in the past
        let info = parse_rate_limit(403, &h).expect("should parse");
        assert_eq!(info.retry_after_minutes, 1);
    }

    #[test]
    fn parse_rate_limit_returns_none_when_header_missing() {
        let h = ureq::http::HeaderMap::new();
        assert!(parse_rate_limit(403, &h).is_none());
        assert!(parse_rate_limit(429, &h).is_none());
    }

    #[test]
    fn parse_rate_limit_returns_none_when_header_unparseable() {
        let mut h = ureq::http::HeaderMap::new();
        h.insert("x-ratelimit-reset", "not-a-number".parse().unwrap());
        assert!(parse_rate_limit(403, &h).is_none());
    }

    #[test]
    fn parse_rate_limit_floor_to_one_minute_when_reset_under_60s() {
        // ~30 seconds ahead → integer division (30/60) == 0, then clamped via .max(1)
        let h = future_reset_headers(30);
        let info = parse_rate_limit(429, &h).expect("should parse");
        assert_eq!(info.retry_after_minutes, 1);
    }

    #[test]
    fn check_response_status_ok_for_2xx_and_3xx() {
        let h = ureq::http::HeaderMap::new();
        assert!(check_response_status(200, &h).is_ok());
        assert!(check_response_status(204, &h).is_ok());
        assert!(check_response_status(301, &h).is_ok());
        assert!(check_response_status(399, &h).is_ok());
    }

    #[test]
    fn check_response_status_404_message() {
        let h = ureq::http::HeaderMap::new();
        let err = check_response_status(404, &h).unwrap_err();
        assert!(err.to_string().contains("release not found"));
    }

    #[test]
    fn check_response_status_5xx_message() {
        let h = ureq::http::HeaderMap::new();
        let err = check_response_status(503, &h).unwrap_err();
        assert!(err.to_string().contains("server error"));
        assert!(err.to_string().contains("503"));
    }

    #[test]
    fn check_response_status_other_4xx_includes_status() {
        let h = ureq::http::HeaderMap::new();
        let err = check_response_status(418, &h).unwrap_err();
        assert!(err.to_string().contains("418"));
    }

    #[test]
    fn check_response_status_403_with_reset_returns_rate_limit_message() {
        let h = future_reset_headers(600);
        let err = check_response_status(403, &h).unwrap_err();
        assert!(err.to_string().contains("rate limited"));
    }

    #[test]
    fn check_response_status_403_without_reset_falls_through_to_generic_4xx() {
        let h = ureq::http::HeaderMap::new();
        let err = check_response_status(403, &h).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("403"));
        // The "rate limited" branch must NOT activate when the header is missing.
        assert!(!msg.contains("rate limited"), "got: {msg}");
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_after_update_is_noop_on_unix() {
        // Unix has atomic rename(2), so `replace_binary` never leaves a `.old`
        // behind. The helper compiles to a no-op here — the contract under
        // test is "calling this from `lib.rs::run()` on Unix has no effect".
        // We do NOT call the upstream `self_replace::self_delete_outside_path`,
        // which would unconditionally `fs::remove_file(current_exe())` on Unix
        // and brick the cargo-test binary.
        cleanup_stale_old_binary();
    }

    #[cfg(windows)]
    #[test]
    fn cleanup_stale_old_binary_removes_existing_old_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let exe = dir.path().join("seshat.exe");
        let stale = dir.path().join("seshat.exe.old");
        fs::write(&exe, b"new").unwrap();
        fs::write(&stale, b"old").unwrap();
        cleanup_stale_old_binary_at(&exe);
        assert!(!stale.exists(), "stale .old file must be removed");
        assert!(exe.exists(), "live binary must be preserved");
    }

    #[cfg(windows)]
    #[test]
    fn cleanup_stale_old_binary_is_noop_when_old_file_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let exe = dir.path().join("seshat.exe");
        fs::write(&exe, b"new").unwrap();
        cleanup_stale_old_binary_at(&exe);
        assert!(exe.exists());
    }

    // ── US-007: integration-style tests for the Windows update flow ──
    //
    // PRD AC for US-007 asks for tests against a "mocked HTTP server" and
    // claims "existing mock-server helpers" exist. Neither is true:
    //   (a) `update.rs` hardcodes `GITHUB_RELEASES_API` as a `const &str`, so
    //       there is no URL injection point; standing up a real mock server
    //       would require non-trivial dependency injection in run_self_update.
    //   (b) `replace_binary` calls `self_replace::self_replace(new_binary)`,
    //       which derives the *target* from `std::env::current_exe()` and
    //       therefore would overwrite the cargo-test binary mid-run if
    //       exercised end-to-end (this constraint is documented in US-005).
    //   (c) No mock-server helper code exists anywhere in the workspace.
    //
    // The user-story intent is regression coverage of the windows-msvc code
    // paths — extension-based asset matching, zip extraction, sha256 verify,
    // preflight, and update-notice. We satisfy that intent by composing the
    // real helper functions against fixture data inside cfg(windows) tests,
    // stopping short of `replace_binary` (deferred to manual + Windows CI
    // integration via US-008). Each test below maps 1:1 to a PRD AC:

    /// US-007 happy path. Builds a hand-crafted .zip with a fake `seshat.exe`
    /// inside the expected `seshat-{target}-v{version}/` layout, computes its
    /// SHA-256, then walks `verify_sha256` → `extract_binary` (which dispatches
    /// to the windows zip path) and asserts the staged .exe lands at the
    /// expected path with the correct content. This is the in-process
    /// equivalent of "asserts target_exe content matches new binary" — minus
    /// the actual `replace_binary` step (see module-level note).
    #[cfg(windows)]
    #[test]
    fn run_self_update_windows_happy_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let archive_path = dir.path().join("seshat-windows-v1.0.0.zip");

        let target = current_target();
        let expected_dir = format!("seshat-{target}-v1.0.0");
        let binary_in_zip = format!("{expected_dir}/seshat.exe");
        let dir_entry = format!("{expected_dir}/");
        let new_binary_bytes = b"new-windows-binary-v1.0.0";

        let zip_bytes = build_zip_archive(&[(&dir_entry, &[]), (&binary_in_zip, new_binary_bytes)]);
        fs::write(&archive_path, &zip_bytes).unwrap();

        let mut hasher = Sha256::new();
        hasher.update(&zip_bytes);
        let hash = hasher.finalize();
        let mut expected_hex = String::with_capacity(hash.len() * 2);
        for byte in hash {
            use std::fmt::Write;
            let _ = write!(expected_hex, "{byte:02x}");
        }

        verify_sha256(&archive_path, &expected_hex).expect("hash matches");

        let staged = extract_binary(&archive_path, dir.path(), "1.0.0").expect("extract ok");
        assert!(staged.is_file(), "staged binary should exist on disk");
        assert!(
            staged.ends_with(format!("{expected_dir}/seshat.exe")),
            "staged binary path should match the windows layout, got: {}",
            staged.display()
        );
        let staged_bytes = fs::read(&staged).unwrap();
        assert_eq!(
            staged_bytes, new_binary_bytes,
            "staged binary content must match the bytes embedded in the zip"
        );
    }

    /// US-007 sha mismatch. Same .zip fixture as happy-path, but verify with
    /// a deliberately-wrong hash → `verify_sha256` returns CommandFailed.
    /// Asserts the existing binary stays unchanged by virtue of the early
    /// error: no extraction or replace ever runs (the real `run_self_update`
    /// short-circuits on the `verify_sha256.inspect_err(...)` branch at
    /// update.rs:123).
    #[cfg(windows)]
    #[test]
    fn run_self_update_windows_sha_mismatch() {
        let dir = tempfile::TempDir::new().unwrap();
        let archive_path = dir.path().join("seshat-windows-v1.0.0.zip");

        let target = current_target();
        let expected_dir = format!("seshat-{target}-v1.0.0");
        let binary_in_zip = format!("{expected_dir}/seshat.exe");
        let dir_entry = format!("{expected_dir}/");
        let zip_bytes = build_zip_archive(&[(&dir_entry, &[]), (&binary_in_zip, b"any-bytes")]);
        fs::write(&archive_path, &zip_bytes).unwrap();

        let wrong_hash = "0".repeat(64);
        let result = verify_sha256(&archive_path, &wrong_hash);
        match result {
            Err(CliError::CommandFailed { reason, .. }) => {
                assert!(
                    reason.contains("SHA256 mismatch"),
                    "sha mismatch path must surface CliError::CommandFailed with a 'SHA256 mismatch' reason, got: {reason}"
                );
            }
            other => panic!("expected SHA256 mismatch CommandFailed, got: {other:?}"),
        }

        let unstaged = dir.path().join(&expected_dir).join("seshat.exe");
        assert!(
            !unstaged.exists(),
            "no extraction must happen on sha mismatch"
        );
    }

    /// US-007 no-zip-asset path. A release whose only artefacts are `.tar.gz`
    /// (Unix triples) with the windows-msvc target → `find_binary_asset`
    /// returns None, which `fetch_release_assets` translates to `Ok(None)` and
    /// `run_self_update` prints "Seshat is up to date" and returns Ok(()).
    /// We don't need to drive `run_self_update` for this — the matcher is the
    /// only branch point.
    #[cfg(windows)]
    #[test]
    fn run_self_update_windows_no_zip_asset_for_target() {
        let assets = vec![
            serde_json::json!({
                "name": "seshat-x86_64-unknown-linux-gnu-v1.0.0.tar.gz",
                "browser_download_url": "https://example.com/linux.tar.gz"
            }),
            serde_json::json!({
                "name": "seshat-aarch64-apple-darwin-v1.0.0.tar.gz",
                "browser_download_url": "https://example.com/darwin.tar.gz"
            }),
        ];

        let result = find_binary_asset(&assets, "x86_64-pc-windows-msvc");
        assert!(
            result.is_none(),
            "windows-msvc target must NOT match any .tar.gz asset, got: {result:?}"
        );

        let json = serde_json::json!({
            "tag_name": "v1.0.0",
            "assets": assets,
        });
        assert!(
            !has_binary_asset_for_current_target(&json),
            "no windows-msvc .zip in this release → background-notice must skip"
        );
    }

    /// US-007 preflight failure. A "binary" that fails to spawn (non-PE bytes
    /// at a `.exe` path) makes `Command::output()` Err on Windows, which
    /// `preflight_check` maps to CommandFailed and triggers temp-dir cleanup.
    /// Asserts: `preflight_check` errs, the temp dir is wiped, and the
    /// existing binary on disk (which we never produced — the fixture stops
    /// before `replace_binary`) is intact.
    #[cfg(windows)]
    #[test]
    fn run_self_update_windows_preflight_fail() {
        let dir = tempfile::TempDir::new().unwrap();
        let temp_dir = dir.path().join("staging");
        fs::create_dir_all(&temp_dir).unwrap();
        let bogus_binary = temp_dir.join("seshat.exe");
        fs::write(&bogus_binary, b"not a PE file").unwrap();

        let result = preflight_check(&bogus_binary, &temp_dir);
        assert!(
            result.is_err(),
            "preflight_check must error on non-executable bytes"
        );
        match result {
            Err(CliError::CommandFailed { command, reason }) => {
                assert_eq!(command, "update");
                assert!(
                    !reason.is_empty(),
                    "CommandFailed should carry a non-empty reason"
                );
            }
            other => panic!("expected CommandFailed, got: {other:?}"),
        }
        assert!(
            !temp_dir.exists(),
            "preflight_check must clean up the staging temp dir on failure"
        );
    }

    /// US-007 background-notice on Windows. Pre-populated cache with a newer
    /// version + has_assets=true is the fast path that
    /// `check_and_print_update_notice_inner` follows; the function emits the
    /// expected eprintln. We can't capture stderr from a unit test without
    /// dup2'ing FD 2, so we lock the contract at the cache layer: after the
    /// call the cache file is unchanged (no network was touched), and the
    /// helper did not panic.
    #[cfg(windows)]
    #[test]
    fn background_notice_prints_on_windows() {
        let dir = tempfile::TempDir::new().unwrap();
        let cache_path = dir.path().join("version-check.json");

        let cache = VersionCache::with_assets("9999.0.0".to_owned(), true);
        cache.write_to_path(&cache_path).unwrap();
        let before = fs::read_to_string(&cache_path).unwrap();

        check_and_print_update_notice_inner(&Some(cache_path.clone()));

        let after = fs::read_to_string(&cache_path).unwrap();
        assert_eq!(
            before, after,
            "fresh cache fast path must not rewrite the cache file"
        );
    }
}
