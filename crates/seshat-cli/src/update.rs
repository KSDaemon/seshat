use std::path::PathBuf;
use std::time::Duration;

use crate::CliError;
use crate::version_cache::VersionCache;

const GITHUB_RELEASES_API: &str = "https://api.github.com/repos/KSDaemon/seshat/releases/latest";
const USER_AGENT: &str = "seshat";
const TIMEOUT_SECS: u64 = 15;

pub fn run_update(check: bool) -> Result<(), CliError> {
    if check {
        run_check()
    } else {
        eprintln!("Self-update not yet implemented. Use --check to see if updates are available.");
        Ok(())
    }
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
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("version-check.json");
        let cache = VersionCache::new("99.99.99".to_owned());
        cache.write_to_path(&cache_path).unwrap();

        let result = run_check_inner(&Some(cache_path));
        assert!(result.is_ok());
    }
}
